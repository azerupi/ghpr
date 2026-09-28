use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A repository in `owner/name` form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoName {
    pub owner: String,
    pub name: String,
}

impl FromStr for RepoName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        match s.split_once('/') {
            Some((owner, name)) if valid(owner) && valid(name) => Ok(RepoName {
                owner: owner.to_owned(),
                name: name.to_owned(),
            }),
            _ => Err(format!("expected OWNER/REPO, got {s:?}")),
        }
    }
}

impl fmt::Display for RepoName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

impl Serialize for RepoName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for RepoName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_owner_and_name() {
        let repo: RepoName = "rust-lang/rust.vim".parse().unwrap();
        assert_eq!(repo.owner, "rust-lang");
        assert_eq!(repo.name, "rust.vim");
        assert_eq!(repo.to_string(), "rust-lang/rust.vim");
    }

    #[test]
    fn rejects_malformed_names() {
        for bad in ["", "owner", "owner/", "/name", "a/b/c", "a b/c", "a/b c"] {
            assert!(
                bad.parse::<RepoName>().is_err(),
                "{bad:?} should be rejected"
            );
        }
    }
}
