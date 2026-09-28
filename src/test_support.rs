//! Shared fixtures for unit tests.

use jiff::Timestamp;

use crate::model::{Activity, ActivityKind, CiState, Mergeable, Pr, ReviewDecision, Role, Turn};

pub fn now() -> Timestamp {
    "2026-09-28T12:00:00Z".parse().unwrap()
}

pub fn pr_titled(number: u64, title: &str) -> Pr {
    Pr {
        id: format!("PR_{number}"),
        repo: "acme/api".parse().unwrap(),
        number,
        title: title.to_owned(),
        url: format!("https://github.com/acme/api/pull/{number}"),
        author: Some("alice".to_owned()),
        author_is_bot: false,
        is_draft: false,
        labels: vec!["bug".to_owned(), "auth".to_owned()],
        base_ref: "main".to_owned(),
        head_ref: "fix".to_owned(),
        created_at: "2026-09-20T12:00:00Z".parse().unwrap(),
        updated_at: "2026-09-27T09:00:00Z".parse().unwrap(),
        additions: 42,
        deletions: 8,
        changed_files: 3,
        review_decision: Some(ReviewDecision::Approved),
        ci: CiState::Pass,
        mergeable: Mergeable::Clean,
        requested_reviewers: Vec::new(),
        my_review: None,
        unresolved_threads: 0,
        commits_since_my_review: 0,
        last_activity: Some(Activity {
            who: "alice".to_owned(),
            role: Role::Author,
            kind: ActivityKind::Commit,
            at: "2026-09-26T12:00:00Z".parse().unwrap(),
        }),
        turn: Turn::Me,
        waiting_since: "2026-09-26T12:00:00Z".parse().unwrap(),
        mentions_me: false,
        others_active_at: Some("2026-09-26T12:00:00Z".parse().unwrap()),
    }
}

pub fn pr(number: u64) -> Pr {
    pr_titled(number, &format!("Change number {number}"))
}
