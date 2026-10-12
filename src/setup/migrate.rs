//! Keeping seen/snoozed/muted state when setup adds or renames profiles.
//! State lives in `<state>/state.json` without a profile and in
//! `<state>/profiles/<name>/state.json` with one, so without moving it, adding
//! profiles would quietly start everything afresh.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr};

use super::write::{ProfileAnswer, Source};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Why, for the review screen and summary.
    pub why: String,
}

/// What to move under `state_root` (the directory holding `state.json`).
/// `active` is gh's active account when setup ran: the no-profile state was
/// collected with it. Nothing is ever moved over existing state.
pub fn plan(
    profiles: &[ProfileAnswer],
    active: Option<&str>,
    state_root: &Path,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<Move> {
    let dir = |name: &str| state_root.join("profiles").join(name);
    let mut moves: Vec<Move> = profiles
        .iter()
        .filter_map(|p| {
            let old = p.previous.as_deref().filter(|old| *old != p.name)?;
            let (from, to) = (dir(old), dir(&p.name));
            (exists(&from) && !exists(&to)).then(|| Move {
                from,
                to,
                why: format!("profile {old} is now {}", p.name),
            })
        })
        .collect();

    let unprofiled = state_root.join("state.json");
    let target = active.and_then(|login| {
        profiles
            .iter()
            .find(|p| p.source == Source::Gh(login.to_owned()))
    });
    if let Some(profile) = target {
        let to = dir(&profile.name).join("state.json");
        let filled_by_rename = moves.iter().any(|m| m.to == dir(&profile.name));
        if exists(&unprofiled) && !exists(&to) && !filled_by_rename {
            moves.push(Move {
                from: unprofiled,
                to,
                why: format!(
                    "it was collected with gh's active account, {}",
                    profile.source
                ),
            });
        }
    }
    moves
}

/// Carry out the moves, and drop the caches of renamed profiles (they're
/// simply fetched again).
pub fn apply(moves: &[Move], profiles: &[ProfileAnswer], cache_root: Option<&Path>) -> Result<()> {
    for m in moves {
        if let Some(parent) = m.to.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("could not create {}", parent.display()))?;
        }
        std::fs::rename(&m.from, &m.to).wrap_err_with(|| {
            format!("could not move {} to {}", m.from.display(), m.to.display())
        })?;
        tracing::info!(from = %m.from.display(), to = %m.to.display(), "moved state");
    }
    if let Some(cache_root) = cache_root {
        for p in profiles {
            if let Some(old) = p.previous.as_deref().filter(|old| *old != p.name) {
                let _ = std::fs::remove_dir_all(cache_root.join("profiles").join(old));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::write::Settings;

    fn profile(name: &str, previous: Option<&str>, login: &str) -> ProfileAnswer {
        let mut p = ProfileAnswer::new(Source::Gh(login.into()), &Settings::default());
        p.name = name.into();
        p.previous = previous.map(Into::into);
        p
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}").unwrap();
    }

    #[test]
    fn moves_unprofiled_state_to_the_active_accounts_profile() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("state.json"));
        let profiles = [
            profile("personal", None, "me"),
            profile("work", None, "me-work"),
        ];
        let moves = plan(&profiles, Some("me-work"), root, &|p| p.exists());
        assert_eq!(
            moves,
            [Move {
                from: root.join("state.json"),
                to: root.join("profiles/work/state.json"),
                why: "it was collected with gh's active account, me-work".into(),
            }]
        );
        apply(&moves, &profiles, None).unwrap();
        assert!(root.join("profiles/work/state.json").exists());
        assert!(!root.join("state.json").exists());

        assert_eq!(plan(&profiles, None, root, &|p| p.exists()), []);
    }

    #[test]
    fn never_overwrites_existing_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("state.json"));
        touch(&root.join("profiles/work/state.json"));
        let profiles = [profile("work", None, "me-work")];
        assert_eq!(plan(&profiles, Some("me-work"), root, &|p| p.exists()), []);
    }

    #[test]
    fn renames_follow_the_profile_and_drop_its_cache() {
        let dir = tempfile::tempdir().unwrap();
        let (root, cache) = (dir.path().join("state"), dir.path().join("cache"));
        touch(&root.join("profiles/job/state.json"));
        touch(&root.join("state.json"));
        touch(&cache.join("profiles/job/for-me.json"));
        let profiles = [profile("work", Some("job"), "me-work")];
        let moves = plan(&profiles, Some("me-work"), &root, &|p| p.exists());
        assert_eq!(moves.len(), 1, "the rename fills the profile: {moves:?}");
        assert_eq!(moves[0].to, root.join("profiles/work"));
        apply(&moves, &profiles, Some(&cache)).unwrap();
        assert!(root.join("profiles/work/state.json").exists());
        assert!(root.join("state.json").exists(), "left alone");
        assert!(!cache.join("profiles/job").exists());
    }
}
