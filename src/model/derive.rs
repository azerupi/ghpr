//! Conversion from raw API responses into [`Pr`].

use color_eyre::eyre::{Result, eyre};

use super::turn::{self, Event, Facts};
use super::{
    ActivityKind, Check, CheckState, CiState, MergeState, Mergeable, MyReview, Pr, QueueEntry,
    QueueState, RepoName, ReviewDecision, ReviewState, Reviewer,
};
use crate::github::types::{RawActor, RawCheck, RawPr, RawPrDetail, RawReviewer, RawTimelineItem};

pub use super::turn::{TeamMode, Viewer};

/// Combine a search result with its details (if they were fetched) into a [`Pr`].
pub fn build_pr(raw: RawPr, detail: Option<RawPrDetail>, viewer: &Viewer) -> Result<Pr> {
    let repo: RepoName = raw
        .repository
        .name_with_owner
        .parse()
        .map_err(|e| eyre!("PR {}: {e}", raw.url))?;
    let mut ci = raw
        .commits
        .nodes
        .last()
        .and_then(|c| c.commit.status_check_rollup.as_ref())
        .map_or(CiState::None, |rollup| ci_state(&rollup.state));
    let labels: Vec<String> = raw
        .labels
        .map(|l| l.nodes.into_iter().map(|l| l.name).collect())
        .unwrap_or_default();
    let mut review_decision = raw.review_decision.as_deref().and_then(review_decision);
    let mergeable = mergeable(&raw.mergeable);
    let merge_state = raw
        .merge_state_status
        .as_deref()
        .map_or(MergeState::Unknown, merge_state);
    let queue = raw.merge_queue_entry.map(|entry| QueueEntry {
        position: entry.position,
        state: queue_state(&entry.state),
        enqueued_at: entry.enqueued_at,
        eta_secs: entry.estimated_time_to_merge,
    });
    let author_is_bot = raw.author.as_ref().is_some_and(RawActor::is_bot);
    let author = raw.author.map(|a| a.login);

    let mut checks = Vec::new();
    let (requested_reviewers, my_reviews, unresolved_threads, events) = match detail {
        Some(detail) => {
            let all: Vec<(Check, bool)> = detail
                .checks
                .nodes
                .into_iter()
                .filter_map(|n| n.commit.status_check_rollup)
                .flat_map(|r| r.contexts.nodes)
                .filter_map(check)
                .collect();
            if !all.is_empty() {
                ci = ci_from_checks(&all);
                checks = all
                    .into_iter()
                    .filter_map(|(check, passed)| (!passed).then_some(check))
                    .collect();
            }
            let requested = detail
                .review_requests
                .nodes
                .into_iter()
                .filter_map(|r| r.requested_reviewer.and_then(reviewer))
                .collect();
            let my_reviews = detail
                .my_reviews
                .nodes
                .into_iter()
                .filter_map(|r| {
                    Some(MyReview {
                        state: review_state(&r.state)?,
                        at: r.submitted_at?,
                    })
                })
                .collect();
            // GitHub only decides when a branch rule requires reviews; without
            // one, the reviews themselves still say where the PR stands.
            if review_decision.is_none() {
                review_decision = decision_from_reviews(
                    detail
                        .latest_opinionated_reviews
                        .nodes
                        .iter()
                        .filter_map(|r| review_state(&r.state)),
                );
            }
            let unresolved = detail
                .review_threads
                .nodes
                .iter()
                .filter(|t| !t.is_resolved)
                .count() as u32;
            let events = detail
                .timeline_items
                .nodes
                .into_iter()
                .filter_map(|item| event(item, &viewer.login))
                .collect();
            (requested, my_reviews, unresolved, events)
        }
        None => (Vec::new(), Vec::new(), 0, Vec::new()),
    };

    let repo_name = repo.to_string();
    let derived = turn::derive(
        &Facts {
            repo: &repo_name,
            author: author.as_deref(),
            is_draft: raw.is_draft,
            labels: &labels,
            review_decision,
            ci,
            mergeable,
            merge_state,
            queue_enabled: raw.is_merge_queue_enabled,
            queue,
            requested: &requested_reviewers,
            created_at: raw.created_at,
            my_reviews: &my_reviews,
        },
        &events,
        viewer,
    );

    Ok(Pr {
        id: raw.id,
        repo,
        number: raw.number,
        title: raw.title,
        url: raw.url,
        author,
        author_is_bot,
        is_draft: raw.is_draft,
        labels,
        base_ref: raw.base_ref_name,
        head_ref: raw.head_ref_name,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
        additions: raw.additions,
        deletions: raw.deletions,
        changed_files: raw.changed_files,
        review_decision,
        ci,
        checks,
        mergeable,
        merge_state,
        queue_enabled: raw.is_merge_queue_enabled,
        queue,
        requested_reviewers,
        my_review: derived.my_review,
        unresolved_threads,
        commits_since_my_review: derived.commits_since_my_review,
        last_activity: derived.last_activity,
        turn: derived.turn,
        waiting_since: derived.waiting_since,
        mentions_me: derived.mentions_me,
        others_active_at: derived.others_active_at,
        demoted: false,
    })
}

fn event(item: RawTimelineItem, viewer: &str) -> Option<Event> {
    let from_actor = |actor: Option<RawActor>, kind, at, body: Option<&str>| {
        let actor_is_bot = actor.as_ref().is_some_and(RawActor::is_bot);
        Event {
            actor: actor.map(|a| a.login),
            actor_is_bot,
            kind,
            at,
            mentions_viewer: body.is_some_and(|b| mentions(b, viewer)),
            requested: None,
        }
    };
    Some(match item {
        RawTimelineItem::PullRequestCommit { commit } => {
            let actor = commit.author.and_then(|a| a.user).map(|u| u.login);
            Event {
                // Commit authors are plain users in the API; bot accounts end in `[bot]`.
                actor_is_bot: actor.as_deref().is_some_and(|l| l.ends_with("[bot]")),
                actor,
                kind: ActivityKind::Commit,
                at: commit.committed_date,
                mentions_viewer: false,
                requested: None,
            }
        }
        RawTimelineItem::HeadRefForcePushedEvent { actor, created_at } => {
            from_actor(actor, ActivityKind::ForcePush, created_at, None)
        }
        RawTimelineItem::IssueComment {
            author,
            created_at,
            body,
        } => from_actor(author, ActivityKind::Comment, created_at, Some(&body)),
        RawTimelineItem::PullRequestReview {
            author,
            state,
            submitted_at,
            body,
        } => {
            // Pending reviews haven't been submitted, so nobody else can see them.
            let kind = ActivityKind::Review(review_state(&state)?);
            from_actor(author, kind, submitted_at?, Some(&body))
        }
        RawTimelineItem::ReadyForReviewEvent { actor, created_at } => {
            from_actor(actor, ActivityKind::ReadyForReview, created_at, None)
        }
        RawTimelineItem::ReviewRequestedEvent {
            actor,
            created_at,
            requested_reviewer,
        } => Event {
            requested: requested_reviewer.and_then(reviewer),
            ..from_actor(actor, ActivityKind::ReviewRequested, created_at, None)
        },
        RawTimelineItem::Unknown => return None,
    })
}

/// Whether `body` contains `@login` as a whole mention (case-insensitive).
fn mentions(body: &str, login: &str) -> bool {
    if login.is_empty() {
        return false;
    }
    let body = body.to_ascii_lowercase();
    let needle = format!("@{}", login.to_ascii_lowercase());
    let is_login_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    body.match_indices(&needle).any(|(i, _)| {
        let before = body[..i].chars().next_back();
        let after = body[i + needle.len()..].chars().next();
        // `before` rules out email addresses like `me@login.com`.
        !before.is_some_and(is_login_char) && !after.is_some_and(is_login_char)
    })
}

fn reviewer(raw: RawReviewer) -> Option<Reviewer> {
    match raw {
        RawReviewer::User { login }
        | RawReviewer::Bot { login }
        | RawReviewer::Mannequin { login } => Some(Reviewer::User(login)),
        RawReviewer::Team { slug, organization } => {
            Some(Reviewer::Team(format!("{}/{slug}", organization.login)))
        }
        RawReviewer::Unknown => None,
    }
}

fn review_state(value: &str) -> Option<ReviewState> {
    match value {
        "APPROVED" => Some(ReviewState::Approved),
        "CHANGES_REQUESTED" => Some(ReviewState::ChangesRequested),
        "COMMENTED" => Some(ReviewState::Commented),
        "DISMISSED" => Some(ReviewState::Dismissed),
        _ => None,
    }
}

fn review_decision(value: &str) -> Option<ReviewDecision> {
    match value {
        "APPROVED" => Some(ReviewDecision::Approved),
        "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
        "REVIEW_REQUIRED" => Some(ReviewDecision::ReviewRequired),
        _ => None,
    }
}

/// Any change request outweighs approvals, as in GitHub's own decision.
fn decision_from_reviews(states: impl Iterator<Item = ReviewState>) -> Option<ReviewDecision> {
    let mut decision = None;
    for state in states {
        match state {
            ReviewState::ChangesRequested => return Some(ReviewDecision::ChangesRequested),
            ReviewState::Approved => decision = Some(ReviewDecision::Approved),
            ReviewState::Commented | ReviewState::Dismissed => {}
        }
    }
    decision
}

/// A check, and whether it passed (successful, neutral, or skipped).
fn check(raw: RawCheck) -> Option<(Check, bool)> {
    let (name, state, required) = match raw {
        RawCheck::CheckRun {
            name,
            status,
            conclusion,
            is_required,
        } => {
            let state = match (status.as_str(), conclusion.as_deref()) {
                ("COMPLETED", Some("SUCCESS" | "NEUTRAL" | "SKIPPED")) => None,
                ("COMPLETED", _) => Some(CheckState::Fail),
                _ => Some(CheckState::Pending),
            };
            (name, state, is_required)
        }
        RawCheck::StatusContext {
            context,
            state,
            is_required,
        } => {
            let state = match ci_state(&state) {
                CiState::Fail => Some(CheckState::Fail),
                CiState::Pending => Some(CheckState::Pending),
                _ => None,
            };
            (context, state, is_required)
        }
        RawCheck::Unknown => return None,
    };
    Some((
        Check {
            name,
            state: state.unwrap_or(CheckState::Pending),
            required,
        },
        state.is_none(),
    ))
}

/// Only required checks decide, in repos that require some; otherwise all do.
fn ci_from_checks(checks: &[(Check, bool)]) -> CiState {
    let any_required = checks.iter().any(|(c, _)| c.required);
    let deciding = |c: &Check| c.required || !any_required;
    let failed = |c: &Check, passed: bool| !passed && c.state == CheckState::Fail;
    if checks.iter().any(|(c, p)| deciding(c) && failed(c, *p)) {
        CiState::Fail
    } else if checks
        .iter()
        .any(|(c, p)| deciding(c) && !p && c.state == CheckState::Pending)
    {
        CiState::Pending
    } else if checks.iter().any(|(c, p)| failed(c, *p)) {
        CiState::Optional
    } else {
        CiState::Pass
    }
}

fn ci_state(value: &str) -> CiState {
    match value {
        "SUCCESS" => CiState::Pass,
        "FAILURE" | "ERROR" => CiState::Fail,
        "PENDING" | "EXPECTED" => CiState::Pending,
        _ => CiState::None,
    }
}

fn merge_state(value: &str) -> MergeState {
    match value {
        "CLEAN" | "HAS_HOOKS" => MergeState::Clean,
        "UNSTABLE" => MergeState::Unstable,
        "BEHIND" => MergeState::Behind,
        // A draft can't be merged; the turn already says so.
        "BLOCKED" | "DRAFT" => MergeState::Blocked,
        "DIRTY" => MergeState::Conflict,
        _ => MergeState::Unknown,
    }
}

fn queue_state(value: &str) -> QueueState {
    match value {
        "AWAITING_CHECKS" => QueueState::AwaitingChecks,
        "MERGEABLE" => QueueState::Mergeable,
        "UNMERGEABLE" => QueueState::Unmergeable,
        "LOCKED" => QueueState::Locked,
        _ => QueueState::Queued,
    }
}

fn mergeable(value: &str) -> Mergeable {
    match value {
        "MERGEABLE" => Mergeable::Clean,
        "CONFLICTING" => Mergeable::Conflicting,
        _ => Mergeable::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> serde_json::Value {
        serde_json::from_str(include_str!("../../tests/fixtures/pr_node.json")).unwrap()
    }

    fn convert(value: serde_json::Value) -> Pr {
        let raw: RawPr = serde_json::from_value(value).unwrap();
        build_pr(raw, None, &Viewer::default()).unwrap()
    }

    #[test]
    fn reads_merge_state_and_queue_entries() {
        let pr = convert(fixture());
        assert_eq!(pr.merge_state, MergeState::Unknown, "missing in older data");
        assert_eq!(pr.queue, None);

        let mut node = fixture();
        node["mergeStateStatus"] = "BEHIND".into();
        node["isMergeQueueEnabled"] = true.into();
        node["mergeQueueEntry"] = serde_json::json!({
            "position": 3,
            "state": "AWAITING_CHECKS",
            "enqueuedAt": "2026-09-27T10:00:00Z",
            "estimatedTimeToMerge": null,
        });
        let pr = convert(node);
        assert_eq!(pr.merge_state, MergeState::Behind);
        assert!(pr.queue_enabled);
        let entry = pr.queue.unwrap();
        assert_eq!(
            (entry.position, entry.state),
            (3, QueueState::AwaitingChecks)
        );
        assert_eq!(entry.eta_secs, None);
        assert_eq!(pr.merge_status(), "queue 3");
        for (raw, state) in [
            ("HAS_HOOKS", MergeState::Clean),
            ("UNSTABLE", MergeState::Unstable),
            ("DIRTY", MergeState::Conflict),
            ("SOMETHING_NEW", MergeState::Unknown),
        ] {
            assert_eq!(merge_state(raw), state, "{raw}");
        }
    }

    #[test]
    fn converts_a_full_pr() {
        let pr = convert(fixture());
        assert_eq!(pr.repo.to_string(), "acme/api");
        assert_eq!(pr.number, 1234);
        assert_eq!(pr.author.as_deref(), Some("alice"));
        assert_eq!(pr.labels, ["bug", "auth"]);
        assert_eq!(pr.review_decision, Some(ReviewDecision::ChangesRequested));
        assert_eq!(pr.ci, CiState::Fail);
        assert_eq!(pr.mergeable, Mergeable::Conflicting);
    }

    #[test]
    fn handles_missing_optional_data() {
        let mut value = fixture();
        value["author"] = serde_json::Value::Null;
        value["reviewDecision"] = serde_json::Value::Null;
        value["mergeable"] = "UNKNOWN".into();
        value["commits"]["nodes"][0]["commit"]["statusCheckRollup"] = serde_json::Value::Null;
        let pr = convert(value);
        assert_eq!(pr.author, None);
        assert_eq!(pr.review_decision, None);
        assert_eq!(pr.mergeable, Mergeable::Unknown);
        assert_eq!(pr.ci, CiState::None);
    }

    #[test]
    fn falls_back_to_the_reviews_without_a_decision() {
        use ReviewState::*;
        let decide = |states: &[ReviewState]| decision_from_reviews(states.iter().copied());
        assert_eq!(decide(&[]), None);
        assert_eq!(decide(&[Commented]), None);
        assert_eq!(decide(&[Approved]), Some(ReviewDecision::Approved));
        assert_eq!(
            decide(&[Approved, ChangesRequested, Approved]),
            Some(ReviewDecision::ChangesRequested)
        );
    }

    #[test]
    fn only_required_checks_decide_ci_when_some_are_required() {
        let c = |name: &str, state: Option<CheckState>, required| {
            (
                Check {
                    name: name.into(),
                    state: state.unwrap_or(CheckState::Pending),
                    required,
                },
                state.is_none(),
            )
        };
        use CheckState::{Fail, Pending};
        let gate = |state| c("gate", state, true);
        assert_eq!(
            ci_from_checks(&[gate(None), c("e2e", Some(Fail), false)]),
            CiState::Optional
        );
        assert_eq!(
            ci_from_checks(&[gate(None), c("e2e", Some(Pending), false)]),
            CiState::Pass
        );
        assert_eq!(
            ci_from_checks(&[gate(Some(Pending)), c("e2e", Some(Fail), false)]),
            CiState::Pending
        );
        assert_eq!(ci_from_checks(&[gate(Some(Fail))]), CiState::Fail);
        // Nothing required: every check counts.
        assert_eq!(
            ci_from_checks(&[c("e2e", Some(Fail), false)]),
            CiState::Fail
        );
        assert_eq!(
            ci_from_checks(&[c("e2e", Some(Pending), false)]),
            CiState::Pending
        );
        assert_eq!(ci_from_checks(&[c("e2e", None, false)]), CiState::Pass);
    }

    #[test]
    fn maps_every_ci_state() {
        for (state, expected) in [
            ("SUCCESS", CiState::Pass),
            ("ERROR", CiState::Fail),
            ("PENDING", CiState::Pending),
            ("EXPECTED", CiState::Pending),
        ] {
            let mut value = fixture();
            value["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"] = state.into();
            assert_eq!(convert(value).ci, expected, "{state}");
        }
    }
}

#[cfg(test)]
mod mention_tests {
    use super::mentions;

    #[test]
    fn matches_whole_mentions_only() {
        assert!(mentions("thanks @me!", "me"));
        assert!(mentions("@Me please look", "me"));
        assert!(mentions("cc @me\n", "me"));
        assert!(!mentions("ping @meow", "me"), "longer login");
        assert!(!mentions("mail foo@me.com", "me"), "email address");
        assert!(!mentions("@me-too", "me"), "hyphenated login");
        assert!(!mentions("no mention", "me"));
        assert!(!mentions("@", ""));
    }
}
