//! The table's columns: which exist, what they show, and which to drop first
//! when the terminal is too narrow.

use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;
use serde::{Deserialize, Deserializer};

use crate::model::{Pr, ScoreWeights, score, slots};
use crate::output::age;
use crate::sort::SortKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Turn,
    Status,
    Repo,
    Number,
    Title,
    Author,
    Size,
    Ci,
    Review,
    Waiting,
    Last,
    Updated,
    Age,
    Score,
    Threads,
    Files,
    Labels,
}

/// Name (as used in the config), header, priority (higher is kept longer), and
/// the sort key for the number keys.
const COLUMNS: &[(Column, &str, &str, u8, Option<SortKey>)] = &[
    (Column::Title, "title", "TITLE", 100, Some(SortKey::Title)),
    (Column::Turn, "turn", "TURN", 90, Some(SortKey::Turn)),
    (Column::Status, "status", "STAT", 88, Some(SortKey::Status)),
    (Column::Number, "number", "#", 85, Some(SortKey::Number)),
    (Column::Repo, "repo", "REPO", 80, Some(SortKey::Repo)),
    (
        Column::Waiting,
        "waiting",
        "WAIT",
        70,
        Some(SortKey::Waiting),
    ),
    (Column::Ci, "ci", "CI", 60, Some(SortKey::Ci)),
    (
        Column::Review,
        "review",
        "REVIEW",
        50,
        Some(SortKey::Review),
    ),
    (
        Column::Author,
        "author",
        "AUTHOR",
        40,
        Some(SortKey::Author),
    ),
    (Column::Last, "last", "LAST", 30, None),
    (Column::Size, "size", "SIZE", 25, Some(SortKey::Size)),
    (Column::Score, "score", "SCORE", 20, Some(SortKey::Score)),
    (
        Column::Updated,
        "updated",
        "UPDATED",
        15,
        Some(SortKey::Updated),
    ),
    (Column::Age, "age", "AGE", 12, Some(SortKey::Created)),
    (
        Column::Threads,
        "threads",
        "THREADS",
        10,
        Some(SortKey::Threads),
    ),
    (Column::Files, "files", "FILES", 8, Some(SortKey::Files)),
    (Column::Labels, "labels", "LABELS", 5, None),
];

pub const DEFAULT: &[Column] = &[
    Column::Turn,
    Column::Status,
    Column::Repo,
    Column::Number,
    Column::Title,
    Column::Author,
    Column::Size,
    Column::Waiting,
    Column::Last,
];

/// Columns always keep at least this much room for the title.
pub const MIN_TITLE_WIDTH: usize = 20;

impl Column {
    fn info(self) -> &'static (Column, &'static str, &'static str, u8, Option<SortKey>) {
        COLUMNS
            .iter()
            .find(|c| c.0 == self)
            .expect("every column is listed")
    }

    pub fn name(self) -> &'static str {
        self.info().1
    }

    pub fn header(self) -> &'static str {
        self.info().2
    }

    fn priority(self) -> u8 {
        self.info().3
    }

    pub fn sort_key(self) -> Option<SortKey> {
        self.info().4
    }

    /// The cell as plain text.
    pub fn text(self, pr: &Pr, weights: &ScoreWeights, now: Timestamp) -> String {
        match self {
            Column::Turn => pr.turn.label().to_owned(),
            Column::Status => slots(pr).iter().map(|s| s.symbol).collect(),
            Column::Repo => pr.repo.to_string(),
            Column::Number => pr.number.to_string(),
            Column::Title if pr.is_draft => format!("[draft] {}", pr.title),
            Column::Title => pr.title.clone(),
            Column::Author => pr.author.clone().unwrap_or_else(|| "ghost".to_owned()),
            Column::Size => format!("+{} -{}", pr.additions, pr.deletions),
            Column::Ci => pr.ci.label().to_owned(),
            Column::Review => pr.review_decision.map_or("-", |r| r.label()).to_owned(),
            Column::Waiting => age(now, pr.waiting_since),
            Column::Last => pr
                .last_activity
                .as_ref()
                .map_or("-", |a| a.role.label())
                .to_owned(),
            Column::Updated => age(now, pr.updated_at),
            Column::Age => age(now, pr.created_at),
            Column::Score => score(pr, weights, now).to_string(),
            Column::Threads => pr.unresolved_threads.to_string(),
            Column::Files => pr.changed_files.to_string(),
            Column::Labels => pr.labels.join(","),
        }
    }

    /// Numbers read better right-aligned.
    pub fn right_aligned(self) -> bool {
        matches!(
            self,
            Column::Number | Column::Score | Column::Threads | Column::Files
        )
    }
}

impl fmt::Display for Column {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Column {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // STATUS took over from the MERGE column.
        if s.eq_ignore_ascii_case("merge") {
            return Ok(Column::Status);
        }
        COLUMNS
            .iter()
            .find(|c| c.1.eq_ignore_ascii_case(s))
            .map(|c| c.0)
            .ok_or_else(|| {
                let names: Vec<&str> = COLUMNS.iter().map(|c| c.1).collect();
                format!("unknown column {s:?} (columns: {})", names.join(", "))
            })
    }
}

impl<'de> Deserialize<'de> for Column {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Which of `columns` to show in `width` characters, given each column's
/// content width. The lowest-priority columns go first; the title is never
/// dropped, and gets whatever room is left (at least [`MIN_TITLE_WIDTH`]).
pub fn fit(columns: &[Column], widths: &[usize], width: usize, spacing: usize) -> Vec<bool> {
    let mut keep = vec![true; columns.len()];
    let needed = |keep: &[bool]| -> usize {
        let shown: Vec<usize> = columns
            .iter()
            .zip(widths)
            .zip(keep)
            .filter(|(_, k)| **k)
            .map(|((c, w), _)| {
                if *c == Column::Title {
                    MIN_TITLE_WIDTH.min(*w)
                } else {
                    *w
                }
            })
            .collect();
        shown.iter().sum::<usize>() + spacing * shown.len().saturating_sub(1)
    };
    let mut by_priority: Vec<usize> = (0..columns.len()).collect();
    by_priority.sort_by_key(|&i| columns[i].priority());
    for i in by_priority {
        if needed(&keep) <= width {
            break;
        }
        if columns[i] != Column::Title {
            keep[i] = false;
        }
    }
    keep
}

/// How much room the title should have before the clippable columns give up
/// some of theirs.
const COMFORT_TITLE_WIDTH: usize = 30;

/// How narrow a column can be clipped to (with a `…`), if any.
fn clip_floor(column: Column, width: usize) -> usize {
    match column {
        Column::Turn => width.min(6),
        Column::Author => width.min(8),
        _ => width,
    }
}

/// Like [`fit`], but turn and author are clipped before any column is
/// dropped, and only as far as the title needs. Returns which columns to
/// show and the width of each.
pub fn fit_clipping(
    columns: &[Column],
    widths: &[usize],
    width: usize,
    spacing: usize,
) -> (Vec<bool>, Vec<usize>) {
    let floors: Vec<usize> = columns
        .iter()
        .zip(widths)
        .map(|(c, w)| clip_floor(*c, *w))
        .collect();
    let keep = fit(columns, &floors, width, spacing);
    let mut out = widths.to_vec();
    let shown = keep.iter().filter(|k| **k).count();
    let others: usize = (0..columns.len())
        .filter(|&i| keep[i] && columns[i] != Column::Title)
        .map(|i| out[i])
        .sum();
    let room = width.saturating_sub(others + spacing * shown.saturating_sub(1));
    let wanted = columns
        .iter()
        .position(|c| *c == Column::Title)
        .map_or(0, |i| widths[i].min(COMFORT_TITLE_WIDTH));
    let mut deficit = wanted.saturating_sub(room);
    for target in [Column::Author, Column::Turn] {
        for i in (0..columns.len()).filter(|&i| keep[i] && columns[i] == target) {
            let cut = deficit.min(out[i] - floors[i]);
            out[i] -= cut;
            deficit -= cut;
        }
    }
    (keep, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_author_then_turn_before_dropping_columns() {
        let columns = DEFAULT;
        // turn status repo # title author size wait last
        let widths = [9, 4, 20, 5, 60, 14, 10, 4, 8];
        let (keep, out) = fit_clipping(columns, &widths, 120, 3);
        assert!(keep.iter().all(|k| *k), "everything still fits clipped");
        assert!(out[5] < 14 && out[5] >= 8, "author clipped first: {out:?}");
        let (_, roomy) = fit_clipping(columns, &widths, 200, 3);
        assert_eq!(roomy, widths, "nothing clipped when there's room");
    }

    #[test]
    fn names_round_trip() {
        for (column, name, ..) in COLUMNS {
            assert_eq!(name.parse::<Column>().unwrap(), *column);
            assert_eq!(column.to_string(), *name);
        }
        let err = "colour".parse::<Column>().unwrap_err();
        assert!(err.contains("waiting"), "{err}");
    }

    #[test]
    fn fit_drops_low_priority_columns_first() {
        let columns = DEFAULT;
        // turn status repo # title author size wait last
        let widths = [9, 4, 20, 5, 60, 10, 10, 4, 8];
        let all = fit(columns, &widths, 200, 1);
        assert!(all.iter().all(|k| *k));

        let narrow = fit(columns, &widths, 80, 1);
        let kept: Vec<&str> = columns
            .iter()
            .zip(&narrow)
            .filter(|(_, k)| **k)
            .map(|(c, _)| c.name())
            .collect();
        // 9+4+20+5+20+10+4 = 72, plus 6 spaces = 78; adding size (10+1) would be 89.
        assert_eq!(
            kept,
            [
                "turn", "status", "repo", "number", "title", "author", "waiting"
            ]
        );

        let tiny = fit(columns, &widths, 10, 1);
        let kept: Vec<&str> = columns
            .iter()
            .zip(&tiny)
            .filter(|(_, k)| **k)
            .map(|(c, _)| c.name())
            .collect();
        assert_eq!(kept, ["title"], "the title always stays");
    }
}
