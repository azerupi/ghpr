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
    /// What GitHub says about merging it now (branch protection included).
    pub merge_state: MergeState,
    /// The repo merges through a merge queue (for the PR's base branch).
    pub queue_enabled: bool,
    /// Its place in the merge queue, while it's in one.
    pub queue: Option<QueueEntry>,

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

impl Pr {
    /// Merge queue place or merge state, in a word or two: `queue 2`,
    /// `behind`, `clean`; `-` when unknown.
    pub fn merge_status(&self) -> String {
        match self.queue {
            Some(q) if q.state == QueueState::Unmergeable => "ejecting".to_owned(),
            Some(q) => format!("queue {}", q.position),
            None if self.merge_state == MergeState::Unknown => "-".to_owned(),
            None => self.merge_state.label().to_owned(),
        }
    }
}

/// GitHub's `mergeStateStatus`: whether the merge button would work now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    /// Mergeable, checks passing.
    Clean,
    /// Mergeable, but some checks that aren't required are failing or pending.
    Unstable,
    /// Branch protection wants the branch brought up to date first.
    Behind,
    /// Something branch protection requires is missing: reviews, checks, ...
    Blocked,
    /// Merge conflicts.
    Conflict,
    /// Not computed yet (GitHub works it out lazily), or unknown to ghpr.
    Unknown,
}

impl MergeState {
    pub fn label(self) -> &'static str {
        match self {
            MergeState::Clean => "clean",
            MergeState::Unstable => "unstable",
            MergeState::Behind => "behind",
            MergeState::Blocked => "blocked",
            MergeState::Conflict => "conflict",
            MergeState::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    /// As GitHub reports it.
    pub position: u32,
    pub state: QueueState,
    pub enqueued_at: Timestamp,
    /// GitHub's estimate of the time left, in seconds.
    pub eta_secs: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueState {
    Queued,
    AwaitingChecks,
    Mergeable,
    /// About to be removed from the queue: its checks failed, or it conflicts.
    Unmergeable,
    Locked,
}

impl QueueState {
    pub fn label(self) -> &'static str {
        match self {
            QueueState::Queued => "queued",
            QueueState::AwaitingChecks => "awaiting checks",
            QueueState::Mergeable => "mergeable",
            QueueState::Unmergeable => "unmergeable",
            QueueState::Locked => "locked",
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

impl ActivityKind {
    /// What someone did: `pushed a commit`, `approved`.
    pub fn verb(self) -> &'static str {
        match self {
            ActivityKind::Commit => "pushed a commit",
            ActivityKind::ForcePush => "force-pushed",
            ActivityKind::Comment => "commented",
            ActivityKind::Review(state) => match state {
                ReviewState::Approved => "approved",
                ReviewState::ChangesRequested => "requested changes",
                ReviewState::Commented => "reviewed",
                ReviewState::Dismissed => "reviewed (dismissed)",
            },
            ActivityKind::ReadyForReview => "marked it ready",
            ActivityKind::ReviewRequested => "requested a review",
        }
    }
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
    /// In the merge queue: nothing to do but wait.
    Queued,
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
            Turn::Queued => "queued",
            Turn::Author => "author",
            Turn::Reviewers => "reviewers",
            Turn::Blocked => "blocked",
        }
    }
}
