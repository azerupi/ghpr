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
    parse_remote_url(String::from_utf8_lossy(&output.stdout).trim())
}

/// Extract `owner/repo` from a github.com remote URL (HTTPS, SSH, or scp-style).
fn parse_remote_url(url: &str) -> Option<RepoName> {
    let path = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let path = path.trim_end_matches('/');
    path.strip_suffix(".git").unwrap_or(path).parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_remotes() {
        for url in [
            "git@github.com:owner/repo.git",
            "git@github.com:owner/repo",
            "ssh://git@github.com/owner/repo.git",
            "https://github.com/owner/repo.git",
            "https://github.com/owner/repo/",
        ] {
            assert_eq!(
                parse_remote_url(url).map(|r| r.to_string()).as_deref(),
                Some("owner/repo"),
                "{url}"
            );
        }
    }

    #[test]
    fn ignores_other_hosts() {
        assert_eq!(parse_remote_url("git@gitlab.com:owner/repo.git"), None);
        assert_eq!(parse_remote_url("https://example.com/owner/repo"), None);
    }
}
