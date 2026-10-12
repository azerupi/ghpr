//! The GitHub accounts the gh CLI is logged in to.

use std::collections::BTreeMap;
use std::process::Command;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub login: String,
    /// gh's active account, which ghpr uses when no profile pins one.
    pub active: bool,
    /// Token scopes as gh reports them, e.g. `gist, read:org, repo`.
    pub scopes: Option<String>,
}

impl Account {
    /// Listing your teams needs `read:org`. Unknown scopes get the benefit of
    /// the doubt.
    pub fn can_read_org(&self) -> bool {
        self.scopes.as_deref().is_none_or(|scopes| {
            scopes
                .split(',')
                .any(|s| matches!(s.trim(), "read:org" | "admin:org" | "write:org"))
        })
    }
}

/// The github.com accounts gh has a working token for, active one first. An
/// error means gh couldn't be asked at all (not installed, too old).
pub fn gh_accounts() -> Result<Vec<Account>, String> {
    accounts_from("gh")
}

fn accounts_from(gh: &str) -> Result<Vec<Account>, String> {
    let output = match Command::new(gh)
        .args([
            "auth",
            "status",
            "--hostname",
            "github.com",
            "--json",
            "hosts",
        ])
        .output()
    {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("the GitHub CLI (gh) isn't installed".to_owned());
        }
        Err(e) => return Err(format!("couldn't run gh: {e}")),
    };
    // gh exits non-zero when one of several accounts has a broken token, but
    // still prints the JSON; only a missing or garbled one is a failure.
    parse(&String::from_utf8_lossy(&output.stdout)).map_err(|_| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        match stderr.lines().find(|l| !l.trim().is_empty()) {
            Some(line) => format!("`gh auth status` failed: {}", line.trim()),
            None => "`gh auth status` failed (gh 2.70 or newer is needed)".to_owned(),
        }
    })
}

fn parse(json: &str) -> serde_json::Result<Vec<Account>> {
    #[derive(Deserialize)]
    struct Status {
        hosts: BTreeMap<String, Vec<Entry>>,
    }
    #[derive(Deserialize)]
    struct Entry {
        login: String,
        #[serde(default)]
        active: bool,
        #[serde(default)]
        state: String,
        scopes: Option<String>,
    }
    let status: Status = serde_json::from_str(json)?;
    let mut accounts: Vec<Account> = status
        .hosts
        .get("github.com")
        .into_iter()
        .flatten()
        .filter(|e| e.state == "success")
        .map(|e| Account {
            login: e.login.clone(),
            active: e.active,
            scopes: e.scopes.clone().filter(|s| !s.is_empty()),
        })
        .collect();
    accounts.sort_by_key(|a| !a.active);
    Ok(accounts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_accounts_active_first() {
        let json = r#"{"hosts":{"github.com":[
            {"state":"success","active":false,"host":"github.com","login":"me","tokenSource":"keyring","scopes":"gist, repo"},
            {"state":"success","active":true,"host":"github.com","login":"me-work","tokenSource":"keyring","scopes":"gist, read:org, repo"},
            {"state":"error","active":false,"host":"github.com","login":"broken","tokenSource":"keyring"}
        ]}}"#;
        let accounts = parse(json).unwrap();
        let logins: Vec<_> = accounts.iter().map(|a| a.login.as_str()).collect();
        assert_eq!(logins, ["me-work", "me"], "broken tokens are left out");
        assert!(accounts[0].active && accounts[0].can_read_org());
        assert!(!accounts[1].can_read_org());
    }

    #[test]
    fn parses_no_accounts() {
        assert_eq!(parse(r#"{"hosts":{}}"#).unwrap(), []);
        let other_host =
            r#"{"hosts":{"ghe.example":[{"state":"success","active":true,"login":"x"}]}}"#;
        assert_eq!(parse(other_host).unwrap(), [], "only github.com counts");
    }

    #[test]
    fn reports_a_missing_gh() {
        let err = accounts_from("ghpr-test-no-such-gh").unwrap_err();
        assert!(err.contains("isn't installed"), "{err}");
    }

    #[test]
    fn rejects_unexpected_output() {
        assert!(parse("unknown flag: --json").is_err());
        assert!(parse("").is_err());
    }
}
