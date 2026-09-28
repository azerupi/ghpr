//! What you've seen, snoozed, and muted. Stored locally, never sent to GitHub.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, eyre};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::model::Pr;

/// Entries for PRs that haven't shown up in any view for this long are dropped.
const FORGET_AFTER: SignedDuration = SignedDuration::from_hours(30 * 24);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalState {
    /// When the state was first created. PRs never marked seen count as new only
    /// if someone acted on them after this, so the first run isn't all "new".
    pub created_at: Timestamp,
    /// Keyed by PR node id.
    #[serde(default)]
    prs: BTreeMap<String, PrState>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PrState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seen_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    snooze: Option<Snooze>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    muted: bool,
    /// The last time the PR was part of a fetched view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_present: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Snooze {
    Until(Timestamp),
    /// Until someone else acts on the PR after this time.
    UntilActivity {
        after: Timestamp,
    },
}

/// The local state of one PR, as the UI and filters see it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    pub new: bool,
    /// The snooze, if it's still in effect.
    pub snoozed: Option<Snooze>,
    pub muted: bool,
}

impl LocalState {
    pub fn new(now: Timestamp) -> Self {
        LocalState {
            created_at: now,
            prs: BTreeMap::new(),
        }
    }

    pub fn flags(&self, pr: &Pr, now: Timestamp) -> Flags {
        let entry = self.prs.get(&pr.id);
        let activity = pr.others_active_at.unwrap_or(pr.created_at);
        let seen = entry.and_then(|e| e.seen_at).unwrap_or(self.created_at);
        let snoozed = entry.and_then(|e| e.snooze).filter(|snooze| match snooze {
            Snooze::Until(until) => now < *until,
            Snooze::UntilActivity { after } => activity <= *after,
        });
        Flags {
            new: activity > seen,
            snoozed,
            muted: entry.is_some_and(|e| e.muted),
        }
    }

    pub fn mark_seen(&mut self, id: &str, now: Timestamp) {
        self.entry(id).seen_at = Some(now);
    }

    /// Make the PR show as new again.
    pub fn mark_unseen(&mut self, id: &str) {
        // Before anyone could have acted, so any activity counts as new.
        self.entry(id).seen_at = Some(Timestamp::MIN);
    }

    pub fn snooze(&mut self, id: &str, snooze: Snooze) {
        self.entry(id).snooze = Some(snooze);
    }

    pub fn unsnooze(&mut self, id: &str) {
        self.entry(id).snooze = None;
    }

    pub fn set_muted(&mut self, id: &str, muted: bool) {
        self.entry(id).muted = muted;
    }

    /// Record that these PRs are still around, and forget ones that haven't been
    /// seen in a long time (they were most likely merged or closed).
    pub fn touch(&mut self, prs: &[Pr], now: Timestamp) {
        for pr in prs {
            if let Some(entry) = self.prs.get_mut(&pr.id) {
                entry.last_present = Some(now);
            }
        }
        self.prs.retain(|_, entry| {
            let recent = entry
                .last_present
                .is_none_or(|at| now.duration_since(at) < FORGET_AFTER);
            recent
                && *entry
                    != PrState {
                        last_present: entry.last_present,
                        ..PrState::default()
                    }
        });
    }

    fn entry(&mut self, id: &str) -> &mut PrState {
        self.prs.entry(id.to_owned()).or_default()
    }
}

/// `$XDG_STATE_HOME/ghpr/state.json` (or the platform equivalent).
pub fn default_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "ghpr")
        .ok_or_else(|| eyre!("could not determine the state directory"))?;
    let dir = dirs.state_dir().unwrap_or_else(|| dirs.data_local_dir());
    Ok(dir.join("state.json"))
}

/// A missing file means a fresh start. A corrupt one is an error rather than
/// silently losing everything that was snoozed and muted.
pub fn load(path: &Path, now: Timestamp) -> Result<LocalState> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).wrap_err_with(|| {
            format!(
                "invalid state file {} (move it away to start fresh)",
                path.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LocalState::new(now)),
        Err(e) => Err(e).wrap_err_with(|| format!("could not read {}", path.display())),
    }
}

pub fn save(path: &Path, state: &LocalState) -> Result<()> {
    let dir = path.parent().ok_or_else(|| eyre!("invalid state path"))?;
    std::fs::create_dir_all(dir).wrap_err_with(|| format!("could not create {}", dir.display()))?;
    let text = serde_json::to_string_pretty(state)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).wrap_err_with(|| format!("could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).wrap_err_with(|| format!("could not write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{now, pr};

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn only_activity_after_the_state_was_created_is_new() {
        // pr(1): someone else acted on 2026-09-26.
        let p = pr(1);
        assert!(
            !LocalState::new(at("2026-09-27T00:00:00Z"))
                .flags(&p, now())
                .new
        );
        assert!(
            LocalState::new(at("2026-09-25T00:00:00Z"))
                .flags(&p, now())
                .new
        );
    }

    #[test]
    fn seeing_clears_new_until_someone_acts_again() {
        let mut state = LocalState::new(at("2026-09-01T00:00:00Z"));
        let mut p = pr(1);
        assert!(state.flags(&p, now()).new);
        state.mark_seen(&p.id, at("2026-09-27T00:00:00Z"));
        assert!(!state.flags(&p, now()).new);
        p.others_active_at = Some(at("2026-09-28T09:00:00Z"));
        assert!(state.flags(&p, now()).new);
        state.mark_unseen(&p.id);
        p.others_active_at = Some(at("2026-09-02T00:00:00Z"));
        assert!(state.flags(&p, now()).new, "unseen makes it new regardless");
    }

    #[test]
    fn prs_without_activity_use_their_creation_time() {
        let mut p = pr(1); // created 2026-09-20
        p.others_active_at = None;
        assert!(
            LocalState::new(at("2026-09-19T00:00:00Z"))
                .flags(&p, now())
                .new
        );
        assert!(
            !LocalState::new(at("2026-09-21T00:00:00Z"))
                .flags(&p, now())
                .new
        );
    }

    #[test]
    fn snoozes_expire_or_wake_on_activity() {
        let mut state = LocalState::new(now());
        let mut p = pr(1);
        state.snooze(&p.id, Snooze::Until(at("2026-09-29T00:00:00Z")));
        assert!(state.flags(&p, now()).snoozed.is_some());
        assert!(
            state
                .flags(&p, at("2026-09-29T00:00:01Z"))
                .snoozed
                .is_none()
        );

        state.snooze(
            &p.id,
            Snooze::UntilActivity {
                after: at("2026-09-26T12:00:00Z"),
            },
        );
        assert!(state.flags(&p, now()).snoozed.is_some());
        p.others_active_at = Some(at("2026-09-28T09:00:00Z"));
        assert!(
            state.flags(&p, now()).snoozed.is_none(),
            "woken by new activity"
        );

        state.unsnooze(&p.id);
        p.others_active_at = Some(at("2026-09-26T12:00:00Z"));
        assert!(state.flags(&p, now()).snoozed.is_none());
    }

    #[test]
    fn muting() {
        let mut state = LocalState::new(now());
        let p = pr(1);
        state.set_muted(&p.id, true);
        assert!(state.flags(&p, now()).muted);
        state.set_muted(&p.id, false);
        assert!(!state.flags(&p, now()).muted);
    }

    #[test]
    fn touch_forgets_old_and_empty_entries() {
        let mut state = LocalState::new(now());
        let (a, b, c) = (pr(1), pr(2), pr(3));
        state.set_muted(&a.id, true);
        state.set_muted(&b.id, true);
        state.set_muted(&c.id, true);
        state.set_muted(&c.id, false); // back to the defaults: nothing worth keeping
        state.touch(&[a.clone(), b.clone()], now());
        assert_eq!(state.prs.len(), 2);

        // A month later only `a` is still around.
        let later = now() + SignedDuration::from_hours(31 * 24);
        state.touch(std::slice::from_ref(&a), later);
        assert!(state.flags(&a, later).muted);
        assert!(!state.flags(&b, later).muted, "b was forgotten");
    }

    #[test]
    fn saves_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/state.json");
        let fresh = load(&path, now()).unwrap();
        assert_eq!(fresh.created_at, now());

        let mut state = LocalState::new(at("2026-09-01T00:00:00Z"));
        state.snooze("PR_1", Snooze::UntilActivity { after: now() });
        state.mark_seen("PR_2", now());
        save(&path, &state).unwrap();
        assert_eq!(load(&path, now()).unwrap(), state);

        std::fs::write(&path, "{broken").unwrap();
        assert!(load(&path, now()).is_err(), "don't silently drop snoozes");
    }
}
