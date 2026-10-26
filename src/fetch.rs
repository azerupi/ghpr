//! Fetching everything a view needs: searches, then details, then the turn.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use color_eyre::eyre::Result;
use futures::future::try_join_all;
use jiff::Timestamp;

use crate::cache::Cached;
use crate::github::Client;
use crate::github::SearchResult;
use crate::github::types::{RawPr, RawPrDetail};
use crate::model::{Pr, RepoName, TeamMode, Turn, Viewer, build_pr};
use crate::progress::{Progress, Reporter};
use crate::views::{Keep, View};

pub struct Fetched {
    pub prs: Vec<Pr>,
    /// The authenticated user's login.
    pub viewer: String,
    /// Searches that matched more PRs than `limit` allowed fetching.
    pub truncated: Vec<Truncated>,
    /// Set when GitHub's search came back empty while it was failing, so `prs`
    /// are the previous results instead, fetched at this time.
    pub stale_since: Option<Timestamp>,
}

pub struct Truncated {
    pub query: String,
    pub fetched: usize,
    pub total: u32,
}

/// How many repos to check when deciding whether search is down.
const PROBE_REPOS: usize = 3;
/// Rounds of asking again before an empty search is believed.
const RECHECKS: u32 = 3;

/// `viewer` supplies the settings; its login is filled in from the API.
/// `previous` is the last good result: it's returned (marked stale) instead
/// of the fresh one when the search looks broken rather than empty.
pub async fn fetch(
    client: &Client,
    view: &View,
    mut viewer: Viewer,
    previous: Option<&Cached>,
    limit: u32,
    reporter: &Reporter,
) -> Result<Fetched> {
    let started = std::time::Instant::now();
    let total = view.searches.len();
    tracing::info!(view = %view.name, searches = total, "fetching");
    let (done, found) = (AtomicUsize::new(0), AtomicUsize::new(0));
    reporter.report(Progress::Searching {
        done: 0,
        total,
        found: 0,
    });
    let searches = try_join_all(view.searches.iter().map(|s| async {
        let result = client.search_prs(&s.query, limit, reporter).await?;
        let found = found.fetch_add(result.prs.len(), Ordering::Relaxed) + result.prs.len();
        let done = done.fetch_add(1, Ordering::Relaxed) + 1;
        reporter.report(Progress::Searching { done, total, found });
        Ok::<_, color_eyre::Report>(result)
    }));
    let mut results = searches.await?;
    // Every search reports who's asking; views always have at least one.
    viewer.login = results
        .first()
        .map(|r| r.viewer.clone())
        .unwrap_or_default();

    if let Some(previous) = previous
        && search_is_down(client, view, &mut results, &previous.prs, limit, reporter).await?
    {
        return Ok(Fetched {
            prs: previous.prs.clone(),
            viewer: viewer.login,
            truncated: Vec::new(),
            stale_since: Some(previous.fetched_at),
        });
    }

    let mut truncated = Vec::new();
    let mut hits = Vec::new();
    for (search, result) in view.searches.iter().zip(results) {
        if (result.prs.len() as u32) < result.total {
            truncated.push(Truncated {
                query: search.query.clone(),
                fetched: result.prs.len(),
                total: result.total,
            });
        }
        hits.extend(result.prs.into_iter().map(|pr| (pr, search.keep)));
    }
    let hits = dedupe(hits);

    let ids: Vec<String> = hits.iter().map(|(pr, _)| pr.id.clone()).collect();
    let mut details: HashMap<String, RawPrDetail> = client
        .pr_details(&ids, &viewer.login, reporter)
        .await?
        .into_iter()
        .map(|d| (d.id.clone(), d))
        .collect();

    let mut prs = Vec::with_capacity(hits.len());
    for (raw, keeps) in hits {
        let detail = details.remove(&raw.id);
        let pr = build_pr(raw, detail, &viewer)?;
        let wanted = keeps.iter().any(|keep| match keep {
            Keep::All => true,
            Keep::MyTurn => pr.turn == Turn::Me,
            Keep::Team => viewer.team_mode(&pr.repo.to_string()) != TeamMode::Ignore,
        });
        if wanted {
            prs.push(pr);
        }
    }
    tracing::info!(
        view = %view.name,
        prs = prs.len(),
        truncated = truncated.len(),
        ms = started.elapsed().as_millis() as u64,
        "fetched"
    );
    Ok(Fetched {
        prs,
        viewer: viewer.login,
        truncated,
        stale_since: None,
    })
}

/// Whether empty searches are GitHub failing rather than PRs really being
/// gone. During search outages GitHub answers some requests with zero matches
/// (and status 200), which would otherwise wipe the view.
///
/// It only matters when a search found nothing and PRs from last time are
/// missing. Then each round asks again, alongside a probe of whether those
/// PRs' repos have any PRs at all, open or not: never zero for a repo we've
/// seen PRs in, unless search is broken. The failures come and go between
/// requests, so one answer isn't trusted: a retry that finds PRs replaces the
/// empty result, a zero probe means search is down, and only [`RECHECKS`]
/// rounds of consistent answers make the empty result believable.
async fn search_is_down(
    client: &Client,
    view: &View,
    results: &mut [SearchResult],
    previous: &[Pr],
    limit: u32,
    reporter: &Reporter,
) -> Result<bool> {
    for round in 1..=RECHECKS {
        let found: HashSet<&str> = results
            .iter()
            .flat_map(|r| r.prs.iter().map(|pr| pr.id.as_str()))
            .collect();
        let probes = probe_repos(previous.iter().filter(|pr| !found.contains(pr.id.as_str())));
        let empty: Vec<usize> = (0..results.len())
            .filter(|&i| results[i].total == 0)
            .collect();
        if probes.is_empty() || empty.is_empty() {
            return Ok(false);
        }

        let retries = try_join_all(
            empty
                .iter()
                .map(|&i| client.search_prs(&view.searches[i].query, limit, reporter)),
        );
        let counts = try_join_all(
            probes
                .iter()
                .map(|repo| async move { client.count(&format!("is:pr repo:{repo}")).await }),
        );
        let (retries, counts) = futures::try_join!(retries, counts)?;

        let down: Vec<String> = probes
            .iter()
            .zip(&counts)
            .filter(|(_, count)| **count == 0)
            .map(|(repo, _)| repo.to_string())
            .collect();
        if !down.is_empty() {
            tracing::warn!(view = %view.name, round, repos = ?down, "search returned nothing for repos with PRs; keeping previous results");
            return Ok(true);
        }
        for (i, retry) in empty.into_iter().zip(retries) {
            if retry.total > 0 {
                tracing::warn!(view = %view.name, round, query = %view.searches[i].query, "empty search found PRs when retried");
                results[i] = retry;
            }
        }
    }
    let still_empty = results.iter().any(|r| r.total == 0);
    tracing::debug!(view = %view.name, still_empty, "empty searches rechecked");
    Ok(false)
}

/// The distinct repos of `missing`, most PRs first, at most [`PROBE_REPOS`].
fn probe_repos<'a>(missing: impl Iterator<Item = &'a Pr>) -> Vec<RepoName> {
    let mut counts: Vec<(RepoName, usize)> = Vec::new();
    for pr in missing {
        match counts.iter_mut().find(|(repo, _)| *repo == pr.repo) {
            Some((_, n)) => *n += 1,
            None => counts.push((pr.repo.clone(), 1)),
        }
    }
    // Stable, so ties keep first-seen order.
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    counts
        .into_iter()
        .take(PROBE_REPOS)
        .map(|(repo, _)| repo)
        .collect()
}

/// Merge PRs found by several searches, keeping the first-seen order and every
/// reason to keep each one. It's shown if any of those reasons holds.
fn dedupe(hits: Vec<(RawPr, Keep)>) -> Vec<(RawPr, Vec<Keep>)> {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<(RawPr, Vec<Keep>)> = Vec::new();
    for (pr, keep) in hits {
        match index.get(&pr.id) {
            Some(&i) => {
                if !out[i].1.contains(&keep) {
                    out[i].1.push(keep);
                }
            }
            None => {
                index.insert(pr.id.clone(), out.len());
                out.push((pr, vec![keep]));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(id: &str) -> RawPr {
        let mut node: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/pr_node.json")).unwrap();
        node["id"] = id.into();
        serde_json::from_value(node).unwrap()
    }

    mod stale {
        use serde_json::json;
        use wiremock::matchers::body_partial_json;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        use super::*;
        use crate::test_support::{now, pr};
        use crate::views::Search;

        const QUERY: &str = "is:pr is:open repo:acme/api";
        const PROBE: &str = "is:pr repo:acme/api";

        fn view() -> View {
            View {
                name: "repo".to_owned(),
                searches: vec![Search {
                    query: QUERY.to_owned(),
                    keep: Keep::All,
                }],
                filter: String::new(),
                sort: String::new(),
            }
        }

        fn viewer() -> Viewer {
            Viewer {
                login: String::new(),
                bots: Vec::new(),
                teams: Vec::new(),
                team_mode: TeamMode::default(),
                team_repos: Vec::new(),
            }
        }

        fn search_page(nodes: Vec<serde_json::Value>) -> ResponseTemplate {
            ResponseTemplate::new(200).set_body_json(json!({
                "data": { "viewer": { "login": "me" }, "search": {
                    "issueCount": nodes.len(),
                    "pageInfo": { "hasNextPage": false, "endCursor": null },
                    "nodes": nodes,
                }}
            }))
        }

        /// A server whose search finds nothing, and whose probe of the repo
        /// counts `repo_prs` PRs, expected `probes` times.
        async fn server(repo_prs: u32, probes: u64) -> MockServer {
            let server = MockServer::start().await;
            Mock::given(body_partial_json(
                json!({ "variables": { "query": QUERY } }),
            ))
            .respond_with(search_page(Vec::new()))
            .mount(&server)
            .await;
            Mock::given(body_partial_json(
                json!({ "variables": { "query": PROBE } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "search": { "issueCount": repo_prs } }
            })))
            .expect(probes)
            .mount(&server)
            .await;
            server
        }

        async fn run(server: &MockServer, previous: Option<&Cached>) -> Fetched {
            let client = Client::with_endpoint("secret".to_owned(), server.uri()).unwrap();
            fetch(&client, &view(), viewer(), previous, 10, &Reporter::none())
                .await
                .unwrap()
        }

        #[tokio::test]
        async fn keeps_previous_results_when_search_is_down() {
            let server = server(0, 1).await;
            let previous = Cached::new(vec![pr(1), pr(2)], "me".to_owned(), now());
            let fetched = run(&server, Some(&previous)).await;
            let numbers: Vec<u64> = fetched.prs.iter().map(|p| p.number).collect();
            assert_eq!(numbers, [1, 2]);
            assert_eq!(fetched.stale_since, Some(now()));
        }

        #[tokio::test]
        async fn believes_empty_results_when_the_repo_still_has_prs() {
            let server = server(12, u64::from(RECHECKS)).await;
            let previous = Cached::new(vec![pr(1)], "me".to_owned(), now());
            let fetched = run(&server, Some(&previous)).await;
            assert!(fetched.prs.is_empty());
            assert_eq!(fetched.stale_since, None);
        }

        #[tokio::test]
        async fn nothing_to_lose_means_no_probe() {
            let server = server(0, 0).await;
            let fetched = run(&server, None).await;
            assert!(fetched.prs.is_empty());
            let empty = Cached::new(Vec::new(), "me".to_owned(), now());
            let fetched = run(&server, Some(&empty)).await;
            assert!(fetched.prs.is_empty());
            assert_eq!(fetched.stale_since, None);
        }

        #[tokio::test]
        async fn uses_a_retry_that_finds_prs() {
            let server = MockServer::start().await;
            let mut node: serde_json::Value =
                serde_json::from_str(include_str!("../tests/fixtures/pr_node.json")).unwrap();
            node["id"] = "PR_1".into();
            // Mounted first, so it answers first; the flapping search fails once.
            Mock::given(body_partial_json(
                json!({ "variables": { "query": QUERY } }),
            ))
            .respond_with(search_page(Vec::new()))
            .up_to_n_times(1)
            .mount(&server)
            .await;
            Mock::given(body_partial_json(
                json!({ "variables": { "query": QUERY } }),
            ))
            .respond_with(search_page(vec![node]))
            .mount(&server)
            .await;
            Mock::given(body_partial_json(
                json!({ "variables": { "query": PROBE } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "search": { "issueCount": 5 } }
            })))
            .mount(&server)
            .await;
            Mock::given(body_partial_json(json!({ "variables": { "login": "me" } })))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({ "data": { "nodes": [] } })),
                )
                .mount(&server)
                .await;

            let previous = Cached::new(vec![pr(1), pr(2)], "me".to_owned(), now());
            let fetched = run(&server, Some(&previous)).await;
            let ids: Vec<&str> = fetched.prs.iter().map(|p| p.id.as_str()).collect();
            assert_eq!(ids, ["PR_1"], "the retried result, not the cache");
            assert_eq!(fetched.stale_since, None);
        }

        #[test]
        fn probes_the_repos_missing_most_prs() {
            let in_repo = |number, repo: &str| Pr {
                repo: repo.parse().unwrap(),
                ..pr(number)
            };
            let missing = [
                in_repo(1, "a/one"),
                in_repo(2, "b/two"),
                in_repo(3, "b/two"),
                in_repo(4, "c/three"),
                in_repo(5, "d/four"),
            ];
            let repos: Vec<String> = probe_repos(missing.iter())
                .iter()
                .map(ToString::to_string)
                .collect();
            assert_eq!(repos, ["b/two", "a/one", "c/three"]);
        }
    }

    #[test]
    fn dedupe_keeps_order_and_every_reason() {
        let hits = vec![
            (raw("a"), Keep::MyTurn),
            (raw("b"), Keep::All),
            (raw("a"), Keep::Team),
            (raw("c"), Keep::MyTurn),
            (raw("b"), Keep::All),
        ];
        let out: Vec<(String, Vec<Keep>)> =
            dedupe(hits).into_iter().map(|(p, k)| (p.id, k)).collect();
        assert_eq!(
            out,
            [
                ("a".to_owned(), vec![Keep::MyTurn, Keep::Team]),
                ("b".to_owned(), vec![Keep::All]),
                ("c".to_owned(), vec![Keep::MyTurn]),
            ]
        );
    }
}
