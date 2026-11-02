//! What stands between a PR and merging, most pressing first.

use super::turn::BLOCKING_LABELS;
use super::{Check, CheckState, CiState, MergeState, Mergeable, Pr, QueueState, ReviewDecision};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Leaving the merge queue: its checks failed or it conflicts.
    Ejecting,
    Queued(u32),
    Draft,
    /// A `wip` / `blocked` / `do-not-merge` label, as spelled on the PR.
    Label(String),
    Conflicts,
    /// The failing checks that count (see [`CiState::Fail`]).
    CiFailing(Vec<String>),
    ChangesRequested,
    CiPending,
    /// Branch protection wants it brought up to date with this branch.
    Behind(String),
    NeedsReview,
    /// GitHub won't merge it, for a reason it doesn't say (a rule ghpr can't
    /// see, like code owners or a second approval).
    Rules,
    /// Approved, but something above is still in the way.
    Approved,
    /// Approved (or not needing it) with nothing in the way.
    Ready,
    /// Failing checks that aren't required.
    OptionalCi(Vec<String>),
    Threads(u32),
}

/// How a [`slots`] symbol reads at a glance; the UI picks the colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// That part is fine (green).
    Fine,
    /// Someone has to fix something.
    Bad,
    /// Waiting on something or someone.
    Waiting,
    /// Not meant to merge yet, or not known.
    Parked,
    Queued,
}

/// Filter names and descriptions, in the order statuses are listed.
pub const NAMES: &[(&str, &str)] = &[
    ("ejecting", "leaving the merge queue"),
    ("queued", "in the merge queue"),
    ("draft", "a draft"),
    ("label", "a wip / blocked / do-not-merge label"),
    ("conflicts", "merge conflicts"),
    ("ci", "a required check failed"),
    ("changes", "changes requested"),
    ("pending", "required checks still running"),
    ("behind", "needs updating with its base branch"),
    ("review", "needs a review"),
    ("rules", "blocked by a rule GitHub doesn't name"),
    ("approved", "approved, with something else in the way"),
    ("ready", "nothing left but merging"),
    ("optional", "checks that aren't required failed"),
    ("threads", "unresolved review threads"),
];

impl Status {
    /// The name `status:` filters use.
    pub fn name(&self) -> &'static str {
        NAMES[self.rank()].0
    }

    /// Its place in [`NAMES`]: lower is more pressing.
    pub fn rank(&self) -> usize {
        match self {
            Status::Ejecting => 0,
            Status::Queued(_) => 1,
            Status::Draft => 2,
            Status::Label(_) => 3,
            Status::Conflicts => 4,
            Status::CiFailing(_) => 5,
            Status::ChangesRequested => 6,
            Status::CiPending => 7,
            Status::Behind(_) => 8,
            Status::NeedsReview => 9,
            Status::Rules => 10,
            Status::Approved => 11,
            Status::Ready => 12,
            Status::OptionalCi(_) => 13,
            Status::Threads(_) => 14,
        }
    }

    /// A word or so, for `{status}` in templates.
    pub fn tag(&self) -> String {
        match self {
            Status::Ejecting => "eject".to_owned(),
            Status::Queued(position) => format!("q{position}"),
            Status::Draft => "draft".to_owned(),
            Status::Label(label) => label.to_lowercase(),
            Status::Conflicts => "conflict".to_owned(),
            Status::CiFailing(_) => "CI".to_owned(),
            Status::ChangesRequested => "changes".to_owned(),
            Status::CiPending => "CI…".to_owned(),
            Status::Behind(_) => "behind".to_owned(),
            Status::NeedsReview => "review".to_owned(),
            Status::Rules => "rules".to_owned(),
            Status::Approved => "approved".to_owned(),
            Status::Ready => "ready".to_owned(),
            Status::OptionalCi(_) => "CI?".to_owned(),
            Status::Threads(n) => format!("{n}💬"),
        }
    }

    /// In words, with every check named, for the detail pane.
    pub fn detail(&self) -> String {
        let checks = |what: &str, checks: &[String]| match checks {
            [] => format!("{what} failing"),
            names => format!("{what} failing: {}", names.join(", ")),
        };
        match self {
            Status::Ejecting => "leaving the merge queue".to_owned(),
            Status::Queued(position) => format!("in the merge queue, position {position}"),
            Status::Draft => "draft".to_owned(),
            Status::Label(label) => format!("labeled {label}"),
            Status::Conflicts => "conflicts".to_owned(),
            Status::CiFailing(names) => checks("CI", names),
            Status::ChangesRequested => "changes requested".to_owned(),
            Status::CiPending => "CI pending".to_owned(),
            Status::Behind(base) => format!("behind {base}"),
            Status::NeedsReview => "needs review".to_owned(),
            Status::Rules => "blocked by rules GitHub doesn't name".to_owned(),
            Status::Approved => "approved".to_owned(),
            Status::Ready => "ready to merge".to_owned(),
            Status::OptionalCi(names) => checks("optional CI", names),
            Status::Threads(1) => "1 unresolved thread".to_owned(),
            Status::Threads(n) => format!("{n} unresolved threads"),
        }
    }
}

/// Everything that applies to the PR, most pressing first.
pub fn statuses(pr: &Pr) -> Vec<Status> {
    let mut out = Vec::new();
    match pr.queue {
        Some(q) if q.state == QueueState::Unmergeable => out.push(Status::Ejecting),
        // Waiting its turn in the queue; nothing else matters until it's out.
        Some(q) => return vec![Status::Queued(q.position)],
        None => {}
    }
    if pr.is_draft {
        out.push(Status::Draft);
    }
    if let Some(label) = pr
        .labels
        .iter()
        .find(|l| BLOCKING_LABELS.iter().any(|b| l.eq_ignore_ascii_case(b)))
    {
        out.push(Status::Label(label.clone()));
    }
    if pr.mergeable == Mergeable::Conflicting || pr.merge_state == MergeState::Conflict {
        out.push(Status::Conflicts);
    }
    let failing = |required: Option<bool>| -> Vec<String> {
        pr.checks
            .iter()
            .filter(|c| c.state == CheckState::Fail && required.is_none_or(|r| c.required == r))
            .map(|c: &Check| c.name.clone())
            .collect()
    };
    if pr.ci == CiState::Fail {
        // Without a required one failing, every check counted.
        let required = failing(Some(true));
        out.push(Status::CiFailing(if required.is_empty() {
            failing(None)
        } else {
            required
        }));
    }
    if pr.review_decision == Some(ReviewDecision::ChangesRequested) {
        out.push(Status::ChangesRequested);
    }
    if pr.ci == CiState::Pending {
        out.push(Status::CiPending);
    }
    // With a merge queue, the queue tests the merged result: being behind
    // doesn't matter, and the merge state isn't reliable (see the turn).
    let gated = !pr.queue_enabled;
    if gated && pr.merge_state == MergeState::Behind {
        out.push(Status::Behind(pr.base_ref.clone()));
    }
    if pr.review_decision == Some(ReviewDecision::ReviewRequired) {
        out.push(Status::NeedsReview);
    }
    if out.is_empty() && gated && pr.merge_state == MergeState::Blocked {
        out.push(Status::Rules);
    }
    let approved = pr.review_decision == Some(ReviewDecision::Approved);
    if out.is_empty() && (approved || pr.merge_state == MergeState::Clean) {
        out.push(Status::Ready);
    } else if approved {
        out.push(Status::Approved);
    }
    if pr.ci == CiState::Optional {
        out.push(Status::OptionalCi(failing(Some(false))));
    }
    if pr.unresolved_threads > 0 {
        out.push(Status::Threads(pr.unresolved_threads));
    }
    out
}

/// One symbol of the STAT column, and what it means for this PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub symbol: &'static str,
    pub tone: Tone,
    /// In words, for the detail pane: `CI failing: lint`.
    pub meaning: String,
}

/// The STAT column: one symbol each for the PR's state, CI, review, and
/// mergeability, in that order. Every slot shows a green `✓` when that part
/// is fine, so `✓✓✓✓` is ready to merge.
pub fn slots(pr: &Pr) -> [Slot; 4] {
    let slot = |symbol, tone, meaning: String| Slot {
        symbol,
        tone,
        meaning,
    };
    let statuses = statuses(pr);
    let find = |pred: fn(&Status) -> bool| statuses.iter().find(|s| pred(s));

    let parked: Vec<String> = statuses
        .iter()
        .filter(|s| matches!(s, Status::Draft | Status::Label(_)))
        .map(Status::detail)
        .collect();
    let state = match pr.queue {
        Some(q) if q.state == QueueState::Unmergeable => {
            slot("⏏", Tone::Bad, Status::Ejecting.detail())
        }
        Some(q) => slot("⧗", Tone::Queued, Status::Queued(q.position).detail()),
        None if !parked.is_empty() => slot("◌", Tone::Parked, parked.join(", ")),
        None => slot("✓", Tone::Fine, "open".to_owned()),
    };
    let ci = match pr.ci {
        CiState::Fail => {
            let failing = find(|s| matches!(s, Status::CiFailing(_)));
            slot(
                "✗",
                Tone::Bad,
                failing.map_or_else(String::new, Status::detail),
            )
        }
        CiState::Pending => slot("◷", Tone::Waiting, "CI running".to_owned()),
        CiState::Optional => {
            let failing = find(|s| matches!(s, Status::OptionalCi(_)));
            slot(
                "✓",
                Tone::Waiting,
                failing.map_or_else(String::new, Status::detail),
            )
        }
        CiState::Pass => slot("✓", Tone::Fine, "CI passing".to_owned()),
        CiState::None => slot("·", Tone::Parked, "no checks".to_owned()),
    };
    let review = match pr.review_decision {
        Some(ReviewDecision::ChangesRequested) => {
            slot("✎", Tone::Bad, "changes requested".to_owned())
        }
        Some(ReviewDecision::ReviewRequired) => slot("?", Tone::Waiting, "needs review".to_owned()),
        Some(ReviewDecision::Approved) => slot("✓", Tone::Fine, "approved".to_owned()),
        None => slot("·", Tone::Parked, "no reviews yet".to_owned()),
    };
    let merge = if let Some(s) = find(|s| matches!(s, Status::Conflicts)) {
        slot(
            "≠",
            Tone::Bad,
            format!("{} with {}", s.detail(), pr.base_ref),
        )
    } else if let Some(s) = find(|s| matches!(s, Status::Behind(_))) {
        slot("↓", Tone::Waiting, s.detail())
    } else if let Some(s) = find(|s| matches!(s, Status::Rules)) {
        slot("⊘", Tone::Waiting, s.detail())
    } else if pr.mergeable == Mergeable::Clean || pr.merge_state != MergeState::Unknown {
        slot("✓", Tone::Fine, "mergeable".to_owned())
    } else {
        slot("·", Tone::Parked, "mergeability not known yet".to_owned())
    };
    [state, ci, review, merge]
}

/// Every status as a tag (`CI approved 2💬`), for scripts; `-` if none.
pub fn status_label(pr: &Pr) -> String {
    let tags: Vec<String> = statuses(pr).iter().map(Status::tag).collect();
    if tags.is_empty() {
        "-".to_owned()
    } else {
        tags.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::QueueEntry;
    use crate::test_support::pr;

    fn check(name: &str, required: bool) -> Check {
        Check {
            name: name.into(),
            state: CheckState::Fail,
            required,
        }
    }

    fn tags(pr: &Pr) -> Vec<String> {
        statuses(pr).iter().map(Status::tag).collect()
    }

    #[test]
    fn names_follow_the_order() {
        let all = [
            Status::Ejecting,
            Status::Queued(1),
            Status::Draft,
            Status::Label(String::new()),
            Status::Conflicts,
            Status::CiFailing(Vec::new()),
            Status::ChangesRequested,
            Status::CiPending,
            Status::Behind(String::new()),
            Status::NeedsReview,
            Status::Rules,
            Status::Approved,
            Status::Ready,
            Status::OptionalCi(Vec::new()),
            Status::Threads(1),
        ];
        let names: Vec<&str> = all.iter().map(Status::name).collect();
        let expected: Vec<&str> = NAMES.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn lists_everything_that_applies_most_pressing_first() {
        let mut p = pr(1); // approved
        p.is_draft = true;
        p.mergeable = Mergeable::Conflicting;
        p.ci = CiState::Fail;
        p.checks = vec![check("CI Gate", true), check("e2e", false)];
        p.unresolved_threads = 2;
        assert_eq!(tags(&p), ["draft", "conflict", "CI", "approved", "2💬"]);
        assert_eq!(status_label(&p), "draft conflict CI approved 2💬");
        assert_eq!(
            statuses(&p)[2].detail(),
            "CI failing: CI Gate",
            "required only"
        );
    }

    #[test]
    fn approved_but_failing_or_with_changes_requested() {
        let mut p = pr(1);
        p.ci = CiState::Fail;
        assert_eq!(status_label(&p), "CI approved");
        p.review_decision = Some(ReviewDecision::ChangesRequested);
        assert_eq!(status_label(&p), "CI changes");
    }

    #[test]
    fn failing_checks_are_named_in_the_detail() {
        let mut p = pr(1);
        p.ci = CiState::Fail;
        p.checks = vec![check("lint", false), check("test", false)];
        assert_eq!(
            statuses(&p)[0].detail(),
            "CI failing: lint, test",
            "none required: all count"
        );

        p.ci = CiState::Optional;
        p.checks = vec![check("e2e", false)];
        assert_eq!(tags(&p), ["ready", "CI?"]);
        assert_eq!(statuses(&p)[1].detail(), "optional CI failing: e2e");
    }

    #[test]
    fn ready_rules_and_behind() {
        let mut p = pr(1); // approved, CI passing
        p.merge_state = MergeState::Clean;
        assert_eq!(tags(&p), ["ready"]);
        p.merge_state = MergeState::Blocked;
        assert_eq!(tags(&p), ["rules", "approved"]);
        p.merge_state = MergeState::Behind;
        assert_eq!(tags(&p), ["behind", "approved"]);
        p.queue_enabled = true;
        assert_eq!(tags(&p), ["ready"], "the queue handles being behind");

        p.review_decision = None;
        p.merge_state = MergeState::Unknown;
        assert_eq!(status_label(&p), "-");
    }

    fn symbols(pr: &Pr) -> String {
        slots(pr).iter().map(|s| s.symbol).collect()
    }

    #[test]
    fn slots_are_all_green_when_ready() {
        let mut p = pr(1); // approved, CI passing, clean
        assert_eq!(symbols(&p), "✓✓✓✓");
        assert!(slots(&p).iter().all(|s| s.tone == Tone::Fine));
        let meanings: Vec<String> = slots(&p).into_iter().map(|s| s.meaning).collect();
        assert_eq!(meanings, ["open", "CI passing", "approved", "mergeable"]);

        p.ci = CiState::Optional;
        let ci = &slots(&p)[1];
        assert_eq!(
            (ci.symbol, ci.tone),
            ("✓", Tone::Waiting),
            "optional failures: yellow"
        );

        p.is_draft = true;
        p.ci = CiState::Fail;
        p.review_decision = Some(ReviewDecision::ChangesRequested);
        p.mergeable = Mergeable::Conflicting;
        assert_eq!(symbols(&p), "◌✗✎≠");
        assert_eq!(slots(&p)[3].meaning, "conflicts with main");

        let mut p = pr(1);
        p.review_decision = Some(ReviewDecision::ReviewRequired);
        p.ci = CiState::Pending;
        p.merge_state = MergeState::Behind;
        assert_eq!(symbols(&p), "✓◷?↓");

        let mut p = pr(1);
        p.merge_state = MergeState::Blocked;
        assert_eq!(symbols(&p), "✓✓✓⊘", "blocked by rules");
        p.review_decision = Some(ReviewDecision::ReviewRequired);
        assert_eq!(symbols(&p), "✓✓?✓", "the review explains it");

        let mut p = pr(1);
        p.ci = CiState::None;
        p.review_decision = None;
        p.mergeable = Mergeable::Unknown;
        p.merge_state = MergeState::Unknown;
        assert_eq!(symbols(&p), "✓···");
    }

    #[test]
    fn the_queue_comes_first() {
        let mut p = pr(1);
        p.ci = CiState::Fail;
        p.queue = Some(QueueEntry {
            position: 2,
            state: QueueState::AwaitingChecks,
            enqueued_at: crate::test_support::now(),
            eta_secs: None,
        });
        assert_eq!(tags(&p), ["q2"]);
        assert_eq!(slots(&p)[0].symbol, "⧗");
        p.queue.as_mut().unwrap().state = QueueState::Unmergeable;
        assert_eq!(tags(&p), ["eject", "CI", "approved"]);
        assert_eq!(slots(&p)[0].symbol, "⏏");
    }
}
