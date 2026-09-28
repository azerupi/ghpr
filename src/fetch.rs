//! Fetching everything a view needs: searches, then details, then the turn.

use std::collections::HashMap;

use color_eyre::eyre::Result;
use futures::future::try_join_all;

use crate::github::Client;
use crate::github::types::{RawPr, RawPrDetail};
use crate::model::{Pr, TeamMode, Turn, Viewer, build_pr};
use crate::views::{Keep, View};

pub struct Fetched {
    pub prs: Vec<Pr>,
    /// The authenticated user's login.
    pub viewer: String,
    /// Searches that matched more PRs than `limit` allowed fetching.
    pub truncated: Vec<Truncated>,
}

pub struct Truncated {
    pub query: String,
    pub fetched: usize,
    pub total: u32,
}

/// `viewer` supplies the settings; its login is filled in from the API.
pub async fn fetch(
    client: &Client,
    view: &View,
    mut viewer: Viewer,
    limit: u32,
) -> Result<Fetched> {
    let searches = try_join_all(
        view.searches
            .iter()
            .map(|s| client.search_prs(&s.query, limit)),
    );
    let (login, results) = futures::try_join!(client.viewer_login(), searches)?;
    viewer.login = login;

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
        .pr_details(&ids, &viewer.login)
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
    Ok(Fetched {
        prs,
        viewer: viewer.login,
        truncated,
    })
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
