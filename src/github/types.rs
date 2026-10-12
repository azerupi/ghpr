//! Raw GraphQL response shapes. These mirror the API; `model` turns them into
//! the types the rest of the program uses.

use jiff::Timestamp;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GraphQlResponse<T> {
    pub data: Option<T>,
    #[serde(default)]
    pub errors: Vec<GraphQlError>,
}

#[derive(Debug, Deserialize)]
pub struct GraphQlError {
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct SearchData {
    pub viewer: RawActor,
    pub search: SearchConnection,
}

#[derive(Debug, Deserialize)]
pub struct ViewerOrgsData {
    pub viewer: RawViewerOrgs,
}

#[derive(Debug, Deserialize)]
pub struct RawViewerOrgs {
    pub login: String,
    pub organizations: Nodes<RawOrganization>,
}

#[derive(Debug, Deserialize)]
pub struct OrgTeamsData {
    /// `null` when the organization isn't visible.
    pub organization: Option<RawOrgTeams>,
}

#[derive(Debug, Deserialize)]
pub struct RawOrgTeams {
    pub teams: Nodes<RawTeam>,
}

#[derive(Debug, Deserialize)]
pub struct RawTeam {
    pub slug: String,
}

#[derive(Debug, Deserialize)]
pub struct CountData {
    pub search: RawCount,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCount {
    pub issue_count: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchConnection {
    pub issue_count: u32,
    pub page_info: PageInfo,
    /// Kept untyped: results that aren't pull requests come back as `{}`.
    pub nodes: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawPr {
    pub id: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub is_draft: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub additions: u32,
    pub deletions: u32,
    pub changed_files: u32,
    pub base_ref_name: String,
    pub head_ref_name: String,
    pub review_decision: Option<String>,
    pub mergeable: String,
    /// Missing from older fixtures and caches; treated as unknown.
    #[serde(default)]
    pub merge_state_status: Option<String>,
    #[serde(default)]
    pub is_merge_queue_enabled: bool,
    #[serde(default)]
    pub merge_queue_entry: Option<RawQueueEntry>,
    pub author: Option<RawActor>,
    pub repository: RawRepository,
    pub labels: Option<Nodes<RawLabel>>,
    pub commits: Nodes<RawCommitNode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawQueueEntry {
    pub position: u32,
    pub state: String,
    pub enqueued_at: Timestamp,
    pub estimated_time_to_merge: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct Nodes<T> {
    pub nodes: Vec<T>,
}

#[derive(Debug, Deserialize)]
pub struct RawActor {
    pub login: String,
    /// `User`, `Bot`, `Mannequin`, ... Missing when not requested.
    #[serde(rename = "__typename", default)]
    pub typename: Option<String>,
}

impl RawActor {
    pub fn is_bot(&self) -> bool {
        self.typename.as_deref() == Some("Bot")
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawRepository {
    pub name_with_owner: String,
}

#[derive(Debug, Deserialize)]
pub struct RawLabel {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct RawCommitNode {
    pub commit: RawCommit,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCommit {
    pub status_check_rollup: Option<RawStatusRollup>,
}

#[derive(Debug, Deserialize)]
pub struct RawStatusRollup {
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct NodesData {
    /// `null` for ids that no longer resolve (e.g. a deleted repository).
    pub nodes: Vec<Option<RawPrDetail>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawPrDetail {
    pub id: String,
    pub review_requests: Nodes<RawReviewRequest>,
    pub my_reviews: Nodes<RawMyReview>,
    pub review_threads: Nodes<RawReviewThread>,
    pub timeline_items: Nodes<RawTimelineItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReviewRequest {
    /// `null` when the requested reviewer is no longer visible to the viewer.
    pub requested_reviewer: Option<RawReviewer>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
pub enum RawReviewer {
    User {
        login: String,
    },
    Bot {
        login: String,
    },
    Mannequin {
        login: String,
    },
    Team {
        slug: String,
        organization: RawOrganization,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
pub struct RawOrganization {
    pub login: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawMyReview {
    pub state: String,
    /// `null` for a pending (unsubmitted) review.
    pub submitted_at: Option<Timestamp>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReviewThread {
    pub is_resolved: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "__typename")]
pub enum RawTimelineItem {
    PullRequestCommit {
        commit: RawTimelineCommit,
    },
    #[serde(rename_all = "camelCase")]
    HeadRefForcePushedEvent {
        actor: Option<RawActor>,
        created_at: Timestamp,
    },
    #[serde(rename_all = "camelCase")]
    IssueComment {
        author: Option<RawActor>,
        created_at: Timestamp,
        body: String,
    },
    #[serde(rename_all = "camelCase")]
    PullRequestReview {
        author: Option<RawActor>,
        state: String,
        submitted_at: Option<Timestamp>,
        body: String,
    },
    #[serde(rename_all = "camelCase")]
    ReadyForReviewEvent {
        actor: Option<RawActor>,
        created_at: Timestamp,
    },
    #[serde(rename_all = "camelCase")]
    ReviewRequestedEvent {
        actor: Option<RawActor>,
        created_at: Timestamp,
        requested_reviewer: Option<RawReviewer>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawTimelineCommit {
    pub committed_date: Timestamp,
    pub author: Option<RawGitActor>,
}

#[derive(Debug, Deserialize)]
pub struct RawGitActor {
    /// The GitHub account matching the commit's author email, if any.
    pub user: Option<RawUserLogin>,
}

#[derive(Debug, Deserialize)]
pub struct RawUserLogin {
    pub login: String,
}
