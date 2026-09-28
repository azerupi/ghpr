//! Multi-key sorting, e.g. `turn,waiting,-size`.
//!
//! Every key has a natural direction (the one you'd usually want), and a `-`
//! prefix reverses it.

use std::cmp::Ordering;

use jiff::Timestamp;

use crate::model::{CiState, Pr, ReviewDecision, ScoreWeights, score};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Score,
    Turn,
    Waiting,
    Updated,
    Created,
    Size,
    Files,
    Threads,
    Repo,
    Number,
    Title,
    Author,
    Ci,
    Review,
}

/// Name, key, and what the natural direction means (shown in help and errors).
pub const KEYS: &[(&str, SortKey, &str)] = &[
    ("score", SortKey::Score, "highest first"),
    ("turn", SortKey::Turn, "your turn first"),
    ("waiting", SortKey::Waiting, "longest waiting first"),
    ("updated", SortKey::Updated, "most recently updated first"),
    ("created", SortKey::Created, "newest first"),
    ("size", SortKey::Size, "smallest first"),
    ("files", SortKey::Files, "fewest files first"),
    ("threads", SortKey::Threads, "most unresolved threads first"),
    ("repo", SortKey::Repo, "A-Z"),
    ("number", SortKey::Number, "lowest first"),
    ("title", SortKey::Title, "A-Z"),
    ("author", SortKey::Author, "A-Z"),
    ("ci", SortKey::Ci, "failing first"),
    ("review", SortKey::Review, "changes requested first"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortSpec {
    keys: Vec<(SortKey, bool)>,
}

impl SortSpec {
    pub fn parse(text: &str) -> Result<Self, String> {
        let keys = text
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(|k| {
                let (reversed, name) = match k.strip_prefix('-') {
                    Some(name) => (true, name),
                    None => (false, k),
                };
                KEYS.iter()
                    .find(|(n, _, _)| n.eq_ignore_ascii_case(name))
                    .map(|(_, key, _)| (*key, reversed))
                    .ok_or_else(|| {
                        let names: Vec<&str> = KEYS.iter().map(|(n, _, _)| *n).collect();
                        format!("unknown sort key {name:?} (keys: {})", names.join(", "))
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(SortSpec { keys })
    }

    pub fn primary(&self) -> Option<(SortKey, bool)> {
        self.keys.first().copied()
    }

    pub fn to_text(&self) -> String {
        self.keys
            .iter()
            .map(|(key, reversed)| {
                let name = KEYS
                    .iter()
                    .find(|(_, k, _)| k == key)
                    .map_or("?", |(n, _, _)| *n);
                if *reversed {
                    format!("-{name}")
                } else {
                    name.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Sort by `key` alone; if it's already the primary key, flip its direction.
    pub fn toggled(&self, key: SortKey) -> SortSpec {
        let reversed = match self.primary() {
            Some((primary, reversed)) if primary == key => !reversed,
            _ => false,
        };
        SortSpec {
            keys: vec![(key, reversed)],
        }
    }

    /// Reverse the primary key.
    pub fn reversed(&self) -> SortSpec {
        let mut keys = self.keys.clone();
        if let Some(first) = keys.first_mut() {
            first.1 = !first.1;
        }
        SortSpec { keys }
    }

    pub fn sort(&self, prs: &mut Vec<Pr>, weights: &ScoreWeights, now: Timestamp) {
        let mut scored: Vec<(i32, Pr)> = prs
            .drain(..)
            .map(|pr| (score(&pr, weights, now), pr))
            .collect();
        scored.sort_by(|(sa, a), (sb, b)| {
            self.keys
                .iter()
                .map(|&(key, reversed)| {
                    let ord = compare(key, a, b, *sa, *sb);
                    if reversed { ord.reverse() } else { ord }
                })
                // Stable tie-breakers, so the order doesn't jump around between refreshes.
                .chain([
                    a.turn.cmp(&b.turn),
                    a.waiting_since.cmp(&b.waiting_since),
                    a.repo.cmp(&b.repo),
                    a.number.cmp(&b.number),
                ])
                .find(|o| o.is_ne())
                .unwrap_or(Ordering::Equal)
        });
        prs.extend(scored.into_iter().map(|(_, pr)| pr));
    }
}

/// Compare in the key's natural direction.
fn compare(key: SortKey, a: &Pr, b: &Pr, score_a: i32, score_b: i32) -> Ordering {
    let lower = |s: &Option<String>| s.as_deref().unwrap_or("").to_lowercase();
    match key {
        SortKey::Score => score_b.cmp(&score_a),
        SortKey::Turn => a.turn.cmp(&b.turn),
        SortKey::Waiting => a.waiting_since.cmp(&b.waiting_since),
        SortKey::Updated => b.updated_at.cmp(&a.updated_at),
        SortKey::Created => b.created_at.cmp(&a.created_at),
        SortKey::Size => (a.additions + a.deletions).cmp(&(b.additions + b.deletions)),
        SortKey::Files => a.changed_files.cmp(&b.changed_files),
        SortKey::Threads => b.unresolved_threads.cmp(&a.unresolved_threads),
        SortKey::Repo => a
            .repo
            .to_string()
            .to_lowercase()
            .cmp(&b.repo.to_string().to_lowercase()),
        SortKey::Number => a.number.cmp(&b.number),
        SortKey::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
        SortKey::Author => lower(&a.author).cmp(&lower(&b.author)),
        SortKey::Ci => ci_rank(a.ci).cmp(&ci_rank(b.ci)),
        SortKey::Review => review_rank(a.review_decision).cmp(&review_rank(b.review_decision)),
    }
}

fn ci_rank(ci: CiState) -> u8 {
    match ci {
        CiState::Fail => 0,
        CiState::Pending => 1,
        CiState::Pass => 2,
        CiState::None => 3,
    }
}

fn review_rank(decision: Option<ReviewDecision>) -> u8 {
    match decision {
        Some(ReviewDecision::ChangesRequested) => 0,
        Some(ReviewDecision::ReviewRequired) => 1,
        Some(ReviewDecision::Approved) => 2,
        None => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Turn;
    use crate::test_support::{now, pr};

    fn numbers(prs: &[Pr]) -> Vec<u64> {
        prs.iter().map(|p| p.number).collect()
    }

    fn sorted(spec: &str, mut prs: Vec<Pr>) -> Vec<u64> {
        SortSpec::parse(spec)
            .unwrap()
            .sort(&mut prs, &ScoreWeights::default(), now());
        numbers(&prs)
    }

    fn sample() -> Vec<Pr> {
        let mut a = pr(1);
        a.turn = Turn::Author;
        a.additions = 900;
        let mut b = pr(2);
        b.turn = Turn::Me;
        b.additions = 5;
        b.waiting_since = "2026-09-27T12:00:00Z".parse().unwrap();
        let mut c = pr(3);
        c.turn = Turn::Me;
        c.additions = 300;
        c.waiting_since = "2026-09-20T12:00:00Z".parse().unwrap();
        vec![a, b, c]
    }

    #[test]
    fn natural_directions() {
        assert_eq!(sorted("size", sample()), [2, 3, 1]);
        assert_eq!(sorted("-size", sample()), [1, 3, 2]);
        assert_eq!(sorted("waiting", sample()), [3, 1, 2]);
        assert_eq!(sorted("number", sample()), [1, 2, 3]);
    }

    #[test]
    fn multiple_keys() {
        assert_eq!(sorted("turn,size", sample()), [2, 3, 1]);
        assert_eq!(sorted("turn,-size", sample()), [3, 2, 1]);
    }

    #[test]
    fn score_puts_my_turn_and_long_waits_first() {
        // 3: me + waiting 8d (40) = 140; 2: me + small + 1d = 120; 1: author + 2d = 10
        assert_eq!(sorted("score", sample()), [3, 2, 1]);
    }

    #[test]
    fn empty_spec_uses_tie_breakers() {
        assert_eq!(sorted("", sample()), [3, 2, 1]);
    }

    #[test]
    fn parse_errors_and_round_trip() {
        let err = SortSpec::parse("turn,colour").unwrap_err();
        assert!(err.contains("\"colour\"") && err.contains("score"), "{err}");
        let spec = SortSpec::parse(" Turn , -waiting ").unwrap();
        assert_eq!(spec.to_text(), "turn,-waiting");
    }

    #[test]
    fn toggling_and_reversing() {
        let spec = SortSpec::parse("score").unwrap();
        assert_eq!(spec.toggled(SortKey::Size).to_text(), "size");
        assert_eq!(
            spec.toggled(SortKey::Size).toggled(SortKey::Size).to_text(),
            "-size"
        );
        assert_eq!(
            SortSpec::parse("turn,size").unwrap().reversed().to_text(),
            "-turn,size"
        );
    }
}
