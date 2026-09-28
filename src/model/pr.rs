use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::RepoName;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pr {
    pub id: String,
    pub repo: RepoName,
    pub number: u64,
    pub title: String,
    pub url: String,
    /// `None` when the author's account has been deleted.
    pub author: Option<String>,
    pub author_is_bot: bool,
    pub is_draft: bool,
    pub labels: Vec<String>,
    pub base_ref: String,
    pub head_ref: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub additions: u32,
    pub deletions: u32,
    pub changed_files: u32,
    pub review_decision: Option<ReviewDecision>,
    pub ci: CiState,
    pub mergeable: Mergeable,

    /// Outstanding review requests (reviewers who have reviewed are no longer listed).
    pub requested_reviewers: Vec<Reviewer>,
    /// The viewer's most recent submitted review.
    pub my_review: Option<MyReview>,
    pub unresolved_threads: u32,
    /// Commits made after the viewer's most recent review.
    pub commits_since_my_review: u32,
    /// The latest activity by a person (bots are ignored).
    pub last_activity: Option<Activity>,
    pub turn: Turn,
    /// When the PR landed in its current turn.
    pub waiting_since: Timestamp,
    /// Someone @mentioned the viewer in a comment or review.
    pub mentions_me: bool,
    /// The latest activity by someone other than the viewer; drives the "new" marker.
    pub others_active_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

impl ReviewDecision {
    pub fn label(self) -> &'static str {
        match self {
            ReviewDecision::Approved => "approved",
            ReviewDecision::ChangesRequested => "changes",
            ReviewDecision::ReviewRequired => "required",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiState {
    Pass,
    Fail,
    Pending,
    /// No checks have reported on the head commit.
    None,
}

impl CiState {
    pub fn label(self) -> &'static str {
        match self {
            CiState::Pass => "pass",
            CiState::Fail => "fail",
            CiState::Pending => "pending",
            CiState::None => "-",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mergeable {
    Clean,
    Conflicting,
    /// GitHub hasn't computed mergeability yet.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reviewer {
    User(String),
    /// `org/slug`
    Team(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MyReview {
    pub state: ReviewState,
    pub at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
}

impl ReviewState {
    pub fn label(self) -> &'static str {
        match self {
            ReviewState::Approved => "approved",
            ReviewState::ChangesRequested => "changes",
            ReviewState::Commented => "commented",
            ReviewState::Dismissed => "dismissed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub who: String,
    pub role: Role,
    pub kind: ActivityKind,
    pub at: Timestamp,
}

/// Who someone is relative to a PR, from the viewer's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The viewer (whether they're the author or a reviewer).
    Me,
    Author,
    /// Anyone other than the author who took part.
    Reviewer,
    Bot,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Me => "me",
            Role::Author => "author",
            Role::Reviewer => "reviewer",
            Role::Bot => "bot",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Commit,
    ForcePush,
    Comment,
    Review(ReviewState),
    ReadyForReview,
    ReviewRequested,
}

/// Who needs to act next, from the viewer's perspective. Ordered by urgency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Turn {
    Me,
    /// Requested from one of the viewer's teams, not the viewer directly.
    Team,
    /// Approved, green, no conflicts: only needs merging.
    Merge,
    Author,
    Reviewers,
    /// Draft, failing CI, conflicts, or a blocking label.
    Blocked,
}

impl Turn {
    pub fn label(self) -> &'static str {
        match self {
            Turn::Me => "me",
            Turn::Team => "team",
            Turn::Merge => "merge",
            Turn::Author => "author",
            Turn::Reviewers => "reviewers",
            Turn::Blocked => "blocked",
        }
    }
}
