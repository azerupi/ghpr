use std::process::Command;

use crate::model::RepoName;

/// The GitHub repository of the current directory's `origin` remote, if any.
pub fn current_repo() -> Option<RepoName> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_remote_url(String::from_utf8_lossy(&output.stdout).trim(), ssh_hostname)
}

/// A git config value as seen from the current directory (so `includeIf`
/// sections apply), if it's set.
pub fn config_value(key: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["config", "--get", key])
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !value.is_empty()).then_some(value)
}

/// The real hostname ssh would connect to for `host`, which differs from
/// `host` when it's an alias in `~/.ssh/config` (e.g. `github.com-work`).
/// `ssh -G` only evaluates the config; it doesn't connect.
fn ssh_hostname(host: &str) -> Option<String> {
    if host.starts_with('-') {
        return None;
    }
    let output = Command::new("ssh").args(["-G", "--", host]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("hostname "))
        .map(|hostname| hostname.trim().to_owned())
}

/// Extract `owner/repo` from a GitHub remote URL (HTTPS, SSH, or scp-style).
/// SSH hosts other than github.com are passed to `resolve_ssh_host`, so
/// aliases that point at github.com are accepted too.
fn parse_remote_url(
    url: &str,
    resolve_ssh_host: impl Fn(&str) -> Option<String>,
) -> Option<RepoName> {
    let path = if let Some(path) = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
    {
        path
    } else {
        let (host, path) = split_ssh_url(url)?;
        if host != "github.com" && resolve_ssh_host(host).as_deref() != Some("github.com") {
            return None;
        }
        path
    };
    let path = path.trim_end_matches('/');
    path.strip_suffix(".git").unwrap_or(path).parse().ok()
}

/// Split an `ssh://[user@]host[:port]/path` or scp-style `[user@]host:path`
/// URL into its host and path.
fn split_ssh_url(url: &str) -> Option<(&str, &str)> {
    let (authority, path) = if let Some(rest) = url.strip_prefix("ssh://") {
        let (authority, path) = rest.split_once('/')?;
        let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        (
            authority.split_once(':').map_or(authority, |(h, _)| h),
            path,
        )
    } else {
        // A `/` before the first `:` makes it a local path, not scp-style.
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') || url.contains("://") {
            return None;
        }
        (
            authority.rsplit_once('@').map_or(authority, |(_, h)| h),
            path,
        )
    };
    (!authority.is_empty()).then_some((authority, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_aliases(_: &str) -> Option<String> {
        None
    }

    fn work_alias(host: &str) -> Option<String> {
        match host {
            "github.com-work" | "gh-work" => Some("github.com".to_owned()),
            "gitlab-work" => Some("gitlab.com".to_owned()),
            _ => None,
        }
    }

    #[test]
    fn parses_github_remotes() {
        for url in [
            "git@github.com:owner/repo.git",
            "git@github.com:owner/repo",
            "ssh://git@github.com/owner/repo.git",
            "ssh://git@github.com:22/owner/repo.git",
            "https://github.com/owner/repo.git",
            "https://github.com/owner/repo/",
        ] {
            assert_eq!(
                parse_remote_url(url, no_aliases)
                    .map(|r| r.to_string())
                    .as_deref(),
                Some("owner/repo"),
                "{url}"
            );
        }
    }

    #[test]
    fn parses_ssh_aliases_for_github() {
        for url in [
            "git@github.com-work:owner/repo.git",
            "gh-work:owner/repo",
            "ssh://git@github.com-work/owner/repo.git",
        ] {
            assert_eq!(
                parse_remote_url(url, work_alias)
                    .map(|r| r.to_string())
                    .as_deref(),
                Some("owner/repo"),
                "{url}"
            );
        }
    }

    #[test]
    fn ignores_other_hosts() {
        for url in [
            "git@gitlab.com:owner/repo.git",
            "https://example.com/owner/repo",
            "git@gitlab-work:owner/repo.git",
            "git@unknown-alias:owner/repo.git",
            "/local/path:with/colon",
        ] {
            assert_eq!(parse_remote_url(url, work_alias), None, "{url}");
        }
    }
}
