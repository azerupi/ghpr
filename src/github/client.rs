use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use std::time::Duration;

use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::queries;
use futures::{StreamExt, TryStreamExt, stream};

use super::types::{GraphQlResponse, NodesData, RawPr, RawPrDetail, SearchData, ViewerData};

const GITHUB_GRAPHQL: &str = "https://api.github.com/graphql";
const PAGE_SIZE: u32 = 100;
/// Tries per request, counting the first.
const MAX_ATTEMPTS: u32 = 3;
/// Pull requests per detail request; the timeline makes each one fairly large.
const DETAIL_BATCH: usize = 25;
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
}

impl Client {
    pub fn new(token: String) -> Result<Self> {
        Self::with_endpoint(token, GITHUB_GRAPHQL.to_owned())
    }

    pub fn with_endpoint(token: String, endpoint: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ghpr/", env!("CARGO_PKG_VERSION")))
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
    pub async fn search_prs(&self, query: &str, limit: u32) -> Result<SearchResult> {
        let mut prs = Vec::new();
        let mut after: Option<String> = None;
        let mut total = 0;

        while (prs.len() as u32) < limit {
            let first = PAGE_SIZE.min(limit - prs.len() as u32);
            let variables = json!({ "query": query, "first": first, "after": after });
            let data: SearchData = self.graphql(queries::SEARCH_PRS, variables).await?;
            let search = data.search;
            total = search.issue_count;

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

        Ok(SearchResult { prs, total })
    }

    /// The login of the authenticated user.
    pub async fn viewer_login(&self) -> Result<String> {
        let data: ViewerData = self.graphql(queries::VIEWER, json!({})).await?;
        Ok(data.viewer.login)
    }

    /// Fetch review and timeline details for the given PR node ids. Ids that no
    /// longer resolve are left out of the result.
    pub async fn pr_details(&self, ids: &[String], viewer: &str) -> Result<Vec<RawPrDetail>> {
        // Owned batches: borrowing chunks here makes the future non-`Send`
        // (rust-lang/rust#102211), and it has to be spawnable.
        let batches: Vec<Vec<String>> = ids.chunks(DETAIL_BATCH).map(<[String]>::to_vec).collect();
        let batches: Vec<Vec<RawPrDetail>> = stream::iter(batches)
            .map(|batch| self.detail_batch(batch, viewer))
            .buffered(DETAIL_CONCURRENCY)
            .try_collect()
            .await?;
        Ok(batches.into_iter().flatten().collect())
    }

    async fn detail_batch(&self, ids: Vec<String>, viewer: &str) -> Result<Vec<RawPrDetail>> {
        let variables = json!({ "ids": ids, "login": viewer });
        let data: NodesData = self.graphql(queries::PR_DETAILS, variables).await?;
        Ok(data.nodes.into_iter().flatten().collect())
    }

    /// POST the body, retrying dropped connections and gateway errors, which
    /// flaky networks (and GitHub under load) produce now and then.
    async fn send(&self, body: &serde_json::Value) -> Result<reqwest::Response> {
        let mut delay = self.retry_delay;
        for attempt in 1.. {
            let result = self
                .http
                .post(&self.endpoint)
                .bearer_auth(&self.token)
                .json(body)
                .send()
                .await;
            let retry = match &result {
                Ok(response) => matches!(
                    response.status(),
                    StatusCode::BAD_GATEWAY
                        | StatusCode::SERVICE_UNAVAILABLE
                        | StatusCode::GATEWAY_TIMEOUT
                ),
                Err(e) => e.is_connect() || e.is_timeout() || e.is_request(),
            };
            if !retry || attempt == MAX_ATTEMPTS {
                return result.wrap_err("request to GitHub failed");
            }
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
        unreachable!("the loop returns on its last attempt")
    }

    async fn graphql<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<T> {
        let body = json!({ "query": query, "variables": variables });
        let response = self.send(&body).await?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            bail!("GitHub rejected the token (401). Run `gh auth login` or set GH_TOKEN.");
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("GitHub returned {status}: {}", body.trim());
        }

        let body: GraphQlResponse<T> = response
            .json()
            .await
            .wrap_err("could not parse GitHub response")?;

        if !body.errors.is_empty() {
            let messages: Vec<_> = body.errors.iter().map(|e| e.message.as_str()).collect();
            bail!("GitHub error: {}", messages.join("; "));
        }
        body.data
            .ok_or_else(|| eyre!("GitHub response contained no data"))
    }
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
            "data": { "search": {
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
        let result = client(&server).await.search_prs("is:pr", 10).await.unwrap();
        assert_eq!(result.prs.len(), 1);
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
            .search_prs("is:pr", 10)
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
        assert!(client(&server).await.search_prs("is:pr", 10).await.is_err());
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
            .search_prs("is:pr", 100)
            .await
            .unwrap();
        let numbers: Vec<_> = result.prs.iter().map(|p| p.number).collect();
        assert_eq!(numbers, [1, 2, 3]);
        assert_eq!(result.total, 3);
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

        let result = client(&server).await.search_prs("is:pr", 2).await.unwrap();
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
            .search_prs("is:pr", 10)
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
            .search_prs("is:pr", 10)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("gh auth login"), "{err}");
    }
}
