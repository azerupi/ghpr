use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use std::time::Duration;

use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::queries;
use crate::progress::{Progress, Reporter};
use futures::{StreamExt, TryStreamExt, stream};

use super::types::{GraphQlResponse, NodesData, RawPr, RawPrDetail, SearchData};

const GITHUB_GRAPHQL: &str = "https://api.github.com/graphql";
const PAGE_SIZE: u32 = 100;
/// Tries per request, counting the first.
const MAX_ATTEMPTS: u32 = 3;
/// Longest a single request may take, including reading the response.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Pull requests per detail request. Small enough that progress moves steadily
/// on slow connections; batches run concurrently, so it costs little.
const DETAIL_BATCH: usize = 10;
/// Detail requests in flight at once. GitHub discourages heavy concurrency.
const DETAIL_CONCURRENCY: usize = 4;

pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    token: String,
    /// Wait before the first retry; doubled for each one after.
    retry_delay: Duration,
}

#[derive(Debug)]
pub struct SearchResult {
    pub prs: Vec<RawPr>,
    /// Total number of matches on GitHub, which may exceed what was fetched.
    pub total: u32,
    /// The authenticated user's login.
    pub viewer: String,
}

impl Client {
    pub fn new(token: String) -> Result<Self> {
        Self::with_endpoint(token, GITHUB_GRAPHQL.to_owned())
    }

    pub fn with_endpoint(token: String, endpoint: String) -> Result<Self> {
        // Without timeouts a request on a dead connection can hang forever;
        // timeouts are retried like any other dropped connection.
        let http = reqwest::Client::builder()
            .user_agent(concat!("ghpr/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(REQUEST_TIMEOUT)
            .build()
            .wrap_err("failed to build HTTP client")?;
        Ok(Client {
            http,
            endpoint,
            token,
            retry_delay: Duration::from_secs(1),
        })
    }

    /// Run a pull request search, following pages until `limit` results are collected.
    pub async fn search_prs(
        &self,
        query: &str,
        limit: u32,
        reporter: &Reporter,
    ) -> Result<SearchResult> {
        let mut prs = Vec::new();
        let mut after: Option<String> = None;
        let mut total = 0;
        let mut viewer = String::new();

        while (prs.len() as u32) < limit {
            let first = PAGE_SIZE.min(limit - prs.len() as u32);
            let variables = json!({ "query": query, "first": first, "after": after });
            let data: SearchData = self
                .graphql(queries::SEARCH_PRS, variables, reporter)
                .await?;
            let search = data.search;
            total = search.issue_count;
            viewer = data.viewer.login;

            for node in search.nodes {
                // Non-PR results (which `is:pr` should exclude anyway) are empty objects.
                if node.as_object().is_some_and(|o| o.is_empty()) {
                    continue;
                }
                prs.push(serde_json::from_value(node).wrap_err("unexpected pull request shape")?);
            }

            match search.page_info.end_cursor {
                Some(cursor) if search.page_info.has_next_page => after = Some(cursor),
                _ => break,
            }
        }

        Ok(SearchResult { prs, total, viewer })
    }

    /// Fetch review and timeline details for the given PR node ids. Ids that no
    /// longer resolve are left out of the result.
    pub async fn pr_details(
        &self,
        ids: &[String],
        viewer: &str,
        reporter: &Reporter,
    ) -> Result<Vec<RawPrDetail>> {
        let total = ids.len();
        reporter.report(Progress::Details { done: 0, total });
        // Owned batches: borrowing chunks here makes the future non-`Send`
        // (rust-lang/rust#102211), and it has to be spawnable.
        let batches: Vec<Vec<String>> = ids.chunks(DETAIL_BATCH).map(<[String]>::to_vec).collect();
        let mut done = 0;
        let batches: Vec<Vec<RawPrDetail>> = stream::iter(batches)
            .map(|batch| self.detail_batch(batch, viewer, reporter))
            // Unordered: details are matched to PRs by id, and progress then
            // reflects batches as they actually finish.
            .buffer_unordered(DETAIL_CONCURRENCY)
            .inspect_ok(|(requested, _)| {
                done += requested;
                reporter.report(Progress::Details { done, total });
            })
            .map_ok(|(_, details)| details)
            .try_collect()
            .await?;
        Ok(batches.into_iter().flatten().collect())
    }

    /// Details for one batch, and how many PRs were asked for.
    async fn detail_batch(
        &self,
        ids: Vec<String>,
        viewer: &str,
        reporter: &Reporter,
    ) -> Result<(usize, Vec<RawPrDetail>)> {
        let requested = ids.len();
        let variables = json!({ "ids": ids, "login": viewer });
        let data: NodesData = self
            .graphql(queries::PR_DETAILS, variables, reporter)
            .await?;
        Ok((requested, data.nodes.into_iter().flatten().collect()))
    }

    /// POST the body, retrying dropped connections and gateway errors, which
    /// flaky networks (and GitHub under load) produce now and then.
    async fn send(
        &self,
        body: &serde_json::Value,
        reporter: &Reporter,
    ) -> Result<reqwest::Response> {
        let operation = operation_name(body["query"].as_str().unwrap_or(""));
        let mut delay = self.retry_delay;
        for attempt in 1.. {
            let started = std::time::Instant::now();
            tracing::debug!(operation, attempt, variables = %summarize(&body["variables"]), "request");
            let result = self
                .http
                .post(&self.endpoint)
                .bearer_auth(&self.token)
                .json(body)
                .send()
                .await;
            match &result {
                Ok(response) => tracing::info!(
                    operation,
                    attempt,
                    status = response.status().as_u16(),
                    ms = started.elapsed().as_millis() as u64,
                    rate_limit_remaining = response
                        .headers()
                        .get("x-ratelimit-remaining")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("?"),
                    "response"
                ),
                Err(e) => tracing::warn!(
                    operation,
                    attempt,
                    ms = started.elapsed().as_millis() as u64,
                    error = %e,
                    "request failed"
                ),
            }
            let reason = match &result {
                Ok(response) => match response.status() {
                    status @ (StatusCode::BAD_GATEWAY
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT) => Some(format!("GitHub returned {status}")),
                    _ => None,
                },
                Err(e) if e.is_timeout() => Some("request timed out".to_owned()),
                Err(e) if e.is_connect() || e.is_request() => Some("connection failed".to_owned()),
                Err(_) => None,
            };
            let Some(reason) = reason.filter(|_| attempt < MAX_ATTEMPTS) else {
                return result.wrap_err("request to GitHub failed");
            };
            tracing::warn!(operation, attempt, %reason, delay_ms = delay.as_millis() as u64, "retrying");
            reporter.report(Progress::Retrying {
                attempt: attempt + 1,
                max: MAX_ATTEMPTS,
                reason,
            });
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
        unreachable!("the loop returns on its last attempt")
    }

    async fn graphql<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: serde_json::Value,
        reporter: &Reporter,
    ) -> Result<T> {
        let body = json!({ "query": query, "variables": variables });
        let response = self.send(&body, reporter).await?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            tracing::error!("token rejected (401)");
            bail!("GitHub rejected the token (401). Run `gh auth login` or set GH_TOKEN.");
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            tracing::error!(status = status.as_u16(), body = %body.trim(), "request failed");
            bail!("GitHub returned {status}: {}", body.trim());
        }

        let body: GraphQlResponse<T> = response
            .json()
            .await
            .wrap_err("could not parse GitHub response")?;

        if !body.errors.is_empty() {
            let messages: Vec<_> = body.errors.iter().map(|e| e.message.as_str()).collect();
            tracing::error!(errors = %messages.join("; "), "GraphQL errors");
            bail!("GitHub error: {}", messages.join("; "));
        }
        body.data
            .ok_or_else(|| eyre!("GitHub response contained no data"))
    }
}

/// `SearchPrs` from `query SearchPrs($query: ...`, for the log.
fn operation_name(query: &str) -> &str {
    query
        .split_whitespace()
        .skip_while(|w| *w != "query")
        .nth(1)
        .map_or("?", |name| name.split('(').next().unwrap_or(name))
}

/// Variables for the log, with long lists (like node ids) cut down to a count.
fn summarize(variables: &serde_json::Value) -> String {
    let Some(map) = variables.as_object() else {
        return variables.to_string();
    };
    map.iter()
        .map(|(key, value)| match value {
            serde_json::Value::Array(items) => format!("{key}=[{} items]", items.len()),
            other => format!("{key}={other}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn pr_node(number: u64) -> serde_json::Value {
        let mut node: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/pr_node.json")).unwrap();
        node["number"] = number.into();
        node
    }

    fn page(nodes: Vec<serde_json::Value>, next: Option<&str>) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "data": { "viewer": { "login": "me" }, "search": {
                "issueCount": 3,
                "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next },
                "nodes": nodes,
            }}
        }))
    }

    async fn client(server: &MockServer) -> Client {
        let mut client = Client::with_endpoint("secret".to_owned(), server.uri()).unwrap();
        client.retry_delay = Duration::from_millis(1);
        client
    }

    #[test]
    fn log_helpers() {
        assert_eq!(operation_name(queries::SEARCH_PRS), "SearchPrs");
        assert_eq!(operation_name(queries::PR_DETAILS), "PrDetails");
        let vars = json!({ "ids": ["a", "b"], "login": "me" });
        assert_eq!(summarize(&vars), "ids=[2 items] login=\"me\"");
    }

    #[tokio::test]
    async fn retries_gateway_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(502))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(page(vec![pr_node(1)], None))
            .mount(&server)
            .await;
        let result = client(&server)
            .await
            .search_prs("is:pr", 10, &Reporter::none())
            .await
            .unwrap();
        assert_eq!(result.prs.len(), 1);
    }

    #[tokio::test]
    async fn reports_retries_and_detail_progress() {
        use std::sync::{Arc, Mutex};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(504))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "nodes": [] }
            })))
            .mount(&server)
            .await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let reporter = Reporter::new(move |p| sink.lock().unwrap().push(p));
        let ids: Vec<String> = (0..30).map(|i| format!("PR_{i}")).collect();
        client(&server)
            .await
            .pr_details(&ids, "me", &reporter)
            .await
            .unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(seen[0], Progress::Details { done: 0, total: 30 });
        assert!(
            seen.contains(&Progress::Retrying {
                attempt: 2,
                max: 3,
                reason: "GitHub returned 504 Gateway Timeout".into(),
            }),
            "{seen:?}"
        );
        assert_eq!(
            seen.last(),
            Some(&Progress::Details {
                done: 30,
                total: 30
            })
        );
    }

    #[tokio::test]
    async fn gives_up_after_a_few_attempts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .expect(3)
            .mount(&server)
            .await;
        let err = client(&server)
            .await
            .search_prs("is:pr", 10, &Reporter::none())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("503"), "{err}");
    }

    #[tokio::test]
    async fn does_not_retry_other_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        assert!(
            client(&server)
                .await
                .search_prs("is:pr", 10, &Reporter::none())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn follows_pages_and_skips_non_pr_nodes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("authorization", "Bearer secret"))
            .and(body_partial_json(json!({ "variables": { "after": null } })))
            .respond_with(page(vec![pr_node(1), json!({}), pr_node(2)], Some("c1")))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ "variables": { "after": "c1" } })))
            .respond_with(page(vec![pr_node(3)], None))
            .expect(1)
            .mount(&server)
            .await;

        let result = client(&server)
            .await
            .search_prs("is:pr", 100, &Reporter::none())
            .await
            .unwrap();
        let numbers: Vec<_> = result.prs.iter().map(|p| p.number).collect();
        assert_eq!(numbers, [1, 2, 3]);
        assert_eq!(result.total, 3);
        assert_eq!(result.viewer, "me");
    }

    #[tokio::test]
    async fn stops_at_the_limit() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ "variables": { "first": 2 } })))
            .respond_with(page(vec![pr_node(1), pr_node(2)], Some("c1")))
            .expect(1)
            .mount(&server)
            .await;

        let result = client(&server)
            .await
            .search_prs("is:pr", 2, &Reporter::none())
            .await
            .unwrap();
        assert_eq!(result.prs.len(), 2);
    }

    #[tokio::test]
    async fn reports_graphql_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": null,
                "errors": [{ "message": "Invalid search query" }],
            })))
            .mount(&server)
            .await;

        let err = client(&server)
            .await
            .search_prs("is:pr", 10, &Reporter::none())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Invalid search query"), "{err}");
    }

    #[tokio::test]
    async fn explains_a_rejected_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = client(&server)
            .await
            .search_prs("is:pr", 10, &Reporter::none())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("gh auth login"), "{err}");
    }
}
