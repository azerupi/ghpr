use std::process::Command;

use crate::model::RepoName;

/// The GitHub repositories the current checkout works on, base first: the
/// repo gh would use (the remote `gh repo set-default` marked, else
/// `upstream`, `github`, `origin`), then `origin` too when it's another one,
/// as in a fork. Empty outside a GitHub checkout.
pub fn current_repos() -> Vec<RepoName> {
    let remotes = run(&["remote", "-v"])
        .map(|out| remote_urls(&out))
        .unwrap_or_default();
    let resolved = run(&["config", "--get-regexp", r"^remote\..*\.gh-resolved$"])
        .map(|out| gh_resolved(&out))
        .unwrap_or_default();
    pick_repos(&remotes, &resolved, &ssh_hostname)
}

fn run(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `(remote, fetch URL)` from `git remote -v`, which applies `insteadOf`.
fn remote_urls(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(char::is_whitespace)?;
            let url = rest.trim().strip_suffix("(fetch)")?.trim();
            Some((name.to_owned(), url.to_owned()))
        })
        .collect()
}

/// `(remote, value)` from `git config --get-regexp ...gh-resolved`. gh stores
/// `base`, or (older versions) the repo's `OWNER/REPO`.
fn gh_resolved(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(' ')?;
            let name = key.strip_prefix("remote.")?.strip_suffix(".gh-resolved")?;
            Some((name.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

fn pick_repos(
    remotes: &[(String, String)],
    resolved: &[(String, String)],
    resolve_ssh_host: &dyn Fn(&str) -> Option<String>,
) -> Vec<RepoName> {
    let repo_of = |name: &str| {
        remotes
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, url)| parse_remote_url(url, resolve_ssh_host))
    };
    let base = resolved
        .iter()
        .find_map(|(name, value)| match value.as_str() {
            "base" => repo_of(name),
            other => other.parse().ok(),
        })
        .or_else(|| {
            ["upstream", "github", "origin"]
                .into_iter()
                .find_map(repo_of)
        })
        .or_else(|| {
            remotes
                .iter()
                .find_map(|(_, url)| parse_remote_url(url, resolve_ssh_host))
        });
    let mut repos: Vec<RepoName> = base.into_iter().collect();
    if let Some(origin) = repo_of("origin")
        && !repos.contains(&origin)
    {
        repos.push(origin);
    }
    repos
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

    fn remotes(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect()
    }

    fn picked(remotes: &[(String, String)], resolved: &[(String, String)]) -> Vec<String> {
        pick_repos(remotes, resolved, &work_alias)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn forks_use_upstream_then_themselves() {
        let fork = remotes(&[
            ("origin", "git@github.com:me/ros2_rust.git"),
            ("upstream", "git@github.com:ros2-rust/ros2_rust.git"),
        ]);
        assert_eq!(picked(&fork, &[]), ["ros2-rust/ros2_rust", "me/ros2_rust"]);
        // `gh repo set-default` wins over the remote names.
        let marked = remotes(&[("origin", "base")]);
        assert_eq!(picked(&fork, &marked), ["me/ros2_rust"]);
        let older_gh = remotes(&[("origin", "acme/ros2_rust")]);
        assert_eq!(picked(&fork, &older_gh), ["acme/ros2_rust", "me/ros2_rust"]);

        let plain = remotes(&[("origin", "https://github.com/o/r.git")]);
        assert_eq!(picked(&plain, &[]), ["o/r"]);
        let elsewhere = remotes(&[
            ("gitlab", "git@gitlab.com:o/r.git"),
            ("mine", "git@github.com:me/r"),
        ]);
        assert_eq!(
            picked(&elsewhere, &[]),
            ["me/r"],
            "any GitHub remote, as a last resort"
        );
        let aliased = remotes(&[("origin", "git@github.com-work:me/r.git")]);
        assert_eq!(
            picked(&aliased, &[]),
            ["me/r"],
            "SSH aliases for github.com"
        );
        assert!(picked(&[], &[]).is_empty());
    }

    #[test]
    fn reads_git_output() {
        let out = "origin\tgit@github.com:me/r.git (fetch)\norigin\tgit@github.com:me/r.git (push)\nupstream\thttps://github.com/o/r (fetch)\n";
        assert_eq!(
            remote_urls(out),
            remotes(&[
                ("origin", "git@github.com:me/r.git"),
                ("upstream", "https://github.com/o/r")
            ])
        );
        let out = "remote.upstream.gh-resolved base\nremote.my.fork.gh-resolved o/r\n";
        assert_eq!(
            gh_resolved(out),
            remotes(&[("upstream", "base"), ("my.fork", "o/r")])
        );
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
