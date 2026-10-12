//! Listing the teams you're on, for the team reviews step.

use color_eyre::eyre::Result;
use futures::{StreamExt, stream};

use crate::github::Client;

/// Organizations asked at once. Work accounts can be in dozens.
const CONCURRENCY: usize = 8;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamList {
    /// `org/slug`, sorted.
    pub teams: Vec<String>,
    /// Organizations that couldn't be listed, and why: `acme: SAML SSO required`.
    pub notes: Vec<String>,
}

/// Every team the token's account is on. Organizations are asked one by one,
/// so one that refuses only costs its own teams. Fails only when the
/// organizations themselves can't be listed (offline, or no `read:org`).
pub async fn fetch(client: &Client) -> Result<TeamList> {
    let (login, orgs) = client.viewer_orgs().await?;
    let results: Vec<(String, Result<Vec<String>>)> = stream::iter(orgs)
        .map(|org| {
            let login = &login;
            async move {
                let teams = client.org_teams(&org, login).await;
                (org, teams)
            }
        })
        .buffered(CONCURRENCY)
        .collect()
        .await;
    let mut list = TeamList::default();
    for (org, result) in results {
        match result {
            Ok(slugs) => list
                .teams
                .extend(slugs.into_iter().map(|slug| format!("{org}/{slug}"))),
            Err(e) => {
                tracing::warn!(%org, error = %format!("{e:#}"), "couldn't list teams");
                list.notes
                    .push(format!("{org}: {}", short(&format!("{e:#}"))));
            }
        }
    }
    list.teams.sort_by_key(|t| t.to_ascii_lowercase());
    Ok(list)
}

/// The search behind a team's review requests.
pub fn requests_query(team: &str) -> String {
    format!("is:pr is:open archived:false team-review-requested:{team}")
}

/// A one-line reason, for the notes under the list.
pub fn short(error: &str) -> String {
    if error.contains("SAML") {
        return "SAML SSO required".to_owned();
    }
    let line = error.lines().next().unwrap_or(error);
    let line = line.strip_prefix("GitHub error: ").unwrap_or(line);
    match line.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn skips_organizations_that_refuse() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("ViewerOrgs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "viewer": { "login": "me", "organizations": { "nodes": [
                    { "login": "acme" }, { "login": "locked" }
                ]}}}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({ "variables": { "org": "acme", "login": "me" } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "organization": { "teams": { "nodes": [
                    { "slug": "robotics" }, { "slug": "Platform" }
                ]}}}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ "variables": { "org": "locked" } })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "organization": null },
                "errors": [{
                    "type": "FORBIDDEN",
                    "path": ["organization"],
                    "message": "Resource protected by organization SAML enforcement. You must grant your OAuth token access to this organization."
                }]
            })))
            .mount(&server)
            .await;

        let client = Client::with_endpoint("t".into(), server.uri()).unwrap();
        let list = fetch(&client).await.unwrap();
        assert_eq!(list.teams, ["acme/Platform", "acme/robotics"]);
        assert_eq!(list.notes, ["locked: SAML SSO required"]);
    }

    #[tokio::test]
    async fn fails_when_organizations_cant_be_listed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": null,
                "errors": [{ "message": "Your token has not been granted the required scopes: read:org" }]
            })))
            .mount(&server)
            .await;
        let client = Client::with_endpoint("t".into(), server.uri()).unwrap();
        let err = fetch(&client).await.unwrap_err();
        assert!(short(&err.to_string()).starts_with("Your token"), "{err}");
    }
}
