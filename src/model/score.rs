//! A single number for "how much does this PR need my attention?".

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::Deserialize;

use super::{CiState, Mergeable, Pr, Turn};

/// Points added or subtracted per property. Set in the config's `[score]` table.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScoreWeights {
    pub turn_me: i32,
    pub turn_team: i32,
    pub turn_merge: i32,
    pub mentioned: i32,
    pub waiting_per_day: i32,
    pub cap_waiting_days: i32,
    pub small_pr_bonus: i32,
    /// PRs with fewer changed lines than this get `small_pr_bonus`.
    pub small_pr_lines: u32,
    pub ci_failing: i32,
    pub draft: i32,
    pub conflict: i32,
    /// Per label name (case-insensitive).
    pub label: BTreeMap<String, i32>,
    /// Per `owner/repo`.
    pub repo: BTreeMap<String, i32>,
}

impl Default for ScoreWeights {
    fn default() -> Self {
        ScoreWeights {
            turn_me: 100,
            turn_team: 60,
            turn_merge: 40,
            mentioned: 30,
            waiting_per_day: 5,
            cap_waiting_days: 10,
            small_pr_bonus: 15,
            small_pr_lines: 100,
            ci_failing: -40,
            draft: -80,
            conflict: -30,
            label: BTreeMap::new(),
            repo: BTreeMap::new(),
        }
    }
}

/// The contributions that make up a PR's score, largest first.
pub fn breakdown(pr: &Pr, weights: &ScoreWeights, now: Timestamp) -> Vec<(String, i32)> {
    let mut parts = Vec::new();
    let mut add = |label: String, points: i32| {
        if points != 0 {
            parts.push((label, points));
        }
    };

    match pr.turn {
        Turn::Me => add("your turn".into(), weights.turn_me),
        Turn::Team => add("team's turn".into(), weights.turn_team),
        Turn::Merge => add("ready to merge".into(), weights.turn_merge),
        Turn::Author | Turn::Reviewers | Turn::Blocked => {}
    }
    if pr.mentions_me {
        add("mentioned".into(), weights.mentioned);
    }
    let days = (now.as_second() - pr.waiting_since.as_second()).max(0) / 86_400;
    let days = days.min(weights.cap_waiting_days.max(0) as i64) as i32;
    add(format!("waiting {days}d"), days * weights.waiting_per_day);
    if pr.additions + pr.deletions < weights.small_pr_lines {
        add("small".into(), weights.small_pr_bonus);
    }
    if pr.ci == CiState::Fail {
        add("CI failing".into(), weights.ci_failing);
    }
    if pr.is_draft {
        add("draft".into(), weights.draft);
    }
    if pr.mergeable == Mergeable::Conflicting {
        add("conflicts".into(), weights.conflict);
    }
    for label in &pr.labels {
        if let Some((name, points)) = weights
            .label
            .iter()
            .find(|(l, _)| l.eq_ignore_ascii_case(label))
        {
            add(format!("label {name}"), *points);
        }
    }
    let repo = pr.repo.to_string();
    if let Some((name, points)) = weights
        .repo
        .iter()
        .find(|(r, _)| r.eq_ignore_ascii_case(&repo))
    {
        add(format!("repo {name}"), *points);
    }

    parts.sort_by_key(|(_, points)| -points.abs());
    parts
}

pub fn score(pr: &Pr, weights: &ScoreWeights, now: Timestamp) -> i32 {
    breakdown(pr, weights, now).iter().map(|(_, p)| p).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{now, pr};

    #[test]
    fn my_turn_small_pr_waiting_two_days() {
        let p = pr(1); // turn me, 50 lines, waiting 2d
        let parts = breakdown(&p, &ScoreWeights::default(), now());
        assert_eq!(
            parts,
            [
                ("your turn".into(), 100),
                ("small".into(), 15),
                ("waiting 2d".into(), 10)
            ]
        );
        assert_eq!(score(&p, &ScoreWeights::default(), now()), 125);
    }

    #[test]
    fn penalties_labels_and_repos() {
        let mut p = pr(1);
        p.turn = Turn::Blocked;
        p.is_draft = true;
        p.ci = CiState::Fail;
        p.mergeable = Mergeable::Conflicting;
        p.additions = 500;
        p.labels = vec!["Urgent".into()];
        let mut weights = ScoreWeights::default();
        weights.label.insert("urgent".into(), 50);
        weights.repo.insert("acme/api".into(), 20);
        assert_eq!(score(&p, &weights, now()), -80 - 40 - 30 + 50 + 20 + 10);
    }

    #[test]
    fn waiting_is_capped() {
        let mut p = pr(1);
        p.waiting_since = "2026-01-01T00:00:00Z".parse().unwrap();
        let parts = breakdown(&p, &ScoreWeights::default(), now());
        assert!(parts.contains(&("waiting 10d".into(), 50)), "{parts:?}");
    }
}
