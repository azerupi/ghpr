//! The last fetched result of each view, so the UI can start instantly.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, eyre};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::model::Pr;
use crate::views::View;

/// Bump when the cached shape changes; older files are then ignored.
const VERSION: u32 = 4;

/// How long cached results may stand in for searches that look broken (see
/// [`crate::fetch::fetch`]). Past this an empty result is believed, so a repo
/// that was deleted or lost access to can't keep its PRs on screen forever.
const FALLBACK_FOR: jiff::SignedDuration = jiff::SignedDuration::from_hours(24);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cached {
    version: u32,
    pub fetched_at: Timestamp,
    /// Whose perspective the PRs were computed from.
    pub viewer: String,
    pub prs: Vec<Pr>,
}

impl Cached {
    #[cfg(test)]
    pub fn new(prs: Vec<Pr>, viewer: String, fetched_at: Timestamp) -> Self {
        Cached {
            version: VERSION,
            fetched_at,
            viewer,
            prs,
        }
    }
}

/// Profiles each get their own subdirectory, so switching between them never
/// overwrites the other's cache.
pub fn default_dir(profile: Option<&str>) -> Result<PathBuf> {
    let dir = directories::ProjectDirs::from("", "", "ghpr")
        .map(|dirs| dirs.cache_dir().to_owned())
        .ok_or_else(|| eyre!("could not determine the cache directory"))?;
    Ok(match profile {
        Some(name) => dir.join("profiles").join(name),
        None => dir,
    })
}

/// The cache file for a view. Keyed by its searches too, so `-R` restrictions
/// and config changes get their own entry.
pub fn path(dir: &Path, view: &View) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    for search in &view.searches {
        search.query.hash(&mut hasher);
    }
    let name: String = view
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{name}-{:016x}.json", hasher.finish()))
}

/// Missing, unreadable, or outdated caches are all just "no cache".
pub fn load(path: &Path) -> Option<Cached> {
    let text = std::fs::read_to_string(path).ok()?;
    let cached = match serde_json::from_str::<Cached>(&text) {
        Ok(cached) if cached.version == VERSION => cached,
        Ok(cached) => {
            tracing::debug!(path = %path.display(), version = cached.version, "ignoring old cache");
            return None;
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "ignoring unreadable cache");
            return None;
        }
    };
    tracing::debug!(path = %path.display(), prs = cached.prs.len(), fetched_at = %cached.fetched_at, "loaded cache");
    Some(cached)
}

/// The cached result to fall back on if a refresh's searches look broken.
pub fn fallback(path: Option<&Path>, now: Timestamp) -> Option<Cached> {
    load(path?).filter(|c| now.duration_since(c.fetched_at) < FALLBACK_FOR)
}

pub fn save(path: &Path, prs: &[Pr], viewer: &str, fetched_at: Timestamp) -> Result<()> {
    #[derive(Serialize)]
    struct CachedRef<'a> {
        version: u32,
        fetched_at: Timestamp,
        viewer: &'a str,
        prs: &'a [Pr],
    }

    let dir = path.parent().ok_or_else(|| eyre!("invalid cache path"))?;
    std::fs::create_dir_all(dir).wrap_err_with(|| format!("could not create {}", dir.display()))?;
    let text = serde_json::to_string(&CachedRef {
        version: VERSION,
        fetched_at,
        viewer,
        prs,
    })?;
    // Write then rename, so a crash never leaves a half-written cache behind.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).wrap_err_with(|| format!("could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).wrap_err_with(|| format!("could not write {}", path.display()))?;
    tracing::debug!(path = %path.display(), prs = prs.len(), "saved cache");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::{Keep, Search};

    fn view(name: &str, query: &str) -> View {
        View {
            name: name.to_owned(),
            searches: vec![Search {
                query: query.to_owned(),
                keep: Keep::All,
            }],
            filter: String::new(),
            sort: String::new(),
        }
    }

    #[test]
    fn path_depends_on_name_and_queries() {
        let dir = Path::new("/c");
        let a = path(dir, &view("repo", "is:pr repo:a/b"));
        assert_eq!(a, path(dir, &view("repo", "is:pr repo:a/b")));
        assert_ne!(a, path(dir, &view("repo", "is:pr repo:c/d")));
        assert!(
            a.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("repo-")
        );
        let odd = path(dir, &view("../x y", "q"));
        assert_eq!(odd.parent(), Some(dir), "names can't escape the cache dir");
    }

    #[test]
    fn round_trips_and_ignores_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub/view.json");
        assert!(load(&file).is_none());

        let at: Timestamp = "2026-09-28T12:00:00Z".parse().unwrap();
        save(&file, &[], "me", at).unwrap();
        let cached = load(&file).unwrap();
        assert_eq!(cached.fetched_at, at);
        assert_eq!(cached.viewer, "me");
        assert!(cached.prs.is_empty());

        std::fs::write(&file, "{not json").unwrap();
        assert!(load(&file).is_none());

        std::fs::write(
            &file,
            r#"{"version":0,"fetched_at":"2026-09-28T12:00:00Z","viewer":"me","prs":[]}"#,
        )
        .unwrap();
        assert!(load(&file).is_none(), "old versions are ignored");
    }
}
