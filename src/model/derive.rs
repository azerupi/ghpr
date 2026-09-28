//! Conversion from raw API responses into [`Pr`].

use color_eyre::eyre::{Result, eyre};

use super::turn::{self, Event, Facts};
use super::{
    ActivityKind, CiState, Mergeable, MyReview, Pr, RepoName, ReviewDecision, ReviewState, Reviewer,
};
use crate::github::types::{RawActor, RawPr, RawPrDetail, RawReviewer, RawTimelineItem};

pub use super::turn::{TeamMode, Viewer};

/// Combine a search result with its details (if they were fetched) into a [`Pr`].
pub fn build_pr(raw: RawPr, detail: Option<RawPrDetail>, viewer: &Viewer) -> Result<Pr> {
    let repo: RepoName = raw
        .repository
        .name_with_owner
        .parse()
        .map_err(|e| eyre!("PR {}: {e}", raw.url))?;
    let ci = raw
        .commits
        .nodes
        .last()
        .and_then(|c| c.commit.status_check_rollup.as_ref())
        .map_or(CiState::None, |rollup| ci_state(&rollup.state));
    let labels: Vec<String> = raw
        .labels
        .map(|l| l.nodes.into_iter().map(|l| l.name).collect())
        .unwrap_or_default();
    let review_decision = raw.review_decision.as_deref().and_then(review_decision);
    let mergeable = mergeable(&raw.mergeable);
    let author_is_bot = raw.author.as_ref().is_some_and(RawActor::is_bot);
    let author = raw.author.map(|a| a.login);

    let (requested_reviewers, my_reviews, unresolved_threads, events) = match detail {
        Some(detail) => {
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
        mergeable,
        requested_reviewers,
        my_review: derived.my_review,
        unresolved_threads,
        commits_since_my_review: derived.commits_since_my_review,
        last_activity: derived.last_activity,
        turn: derived.turn,
        waiting_since: derived.waiting_since,
        mentions_me: derived.mentions_me,
        others_active_at: derived.others_active_at,
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

fn ci_state(value: &str) -> CiState {
    match value {
        "SUCCESS" => CiState::Pass,
        "FAILURE" | "ERROR" => CiState::Fail,
        "PENDING" | "EXPECTED" => CiState::Pending,
        _ => CiState::None,
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
