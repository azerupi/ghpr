/// Search for pull requests, fetching only the fields needed for the list view.
/// The viewer's login comes along, saving a request of its own.
pub const SEARCH_PRS: &str = r#"
query SearchPrs($query: String!, $first: Int!, $after: String) {
  viewer { login }
  search(type: ISSUE, query: $query, first: $first, after: $after) {
    issueCount
    pageInfo { hasNextPage endCursor }
    nodes {
      ... on PullRequest {
        id
        number
        title
        url
        isDraft
        createdAt
        updatedAt
        additions
        deletions
        changedFiles
        baseRefName
        headRefName
        reviewDecision
        mergeable
        mergeStateStatus
        isMergeQueueEnabled
        mergeQueueEntry { position state enqueuedAt estimatedTimeToMerge }
        author { login __typename }
        repository { nameWithOwner }
        labels(first: 20) { nodes { name } }
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
      }
    }
  }
}
"#;

/// Review, request, timeline, and check details for a batch of pull requests:
/// `p0` for `$id0`, and so on. Each PR is its own field because whether a
/// check is required depends on the PR, which has to be spelled out in it.
/// `$login` is the viewer, used to fetch only their own reviews.
pub fn pr_details(count: usize) -> String {
    let variables: String = (0..count).map(|i| format!(", $id{i}: ID!")).collect();
    let fields: String = (0..count)
        .map(|i| {
            format!(
                "  p{i}: node(id: $id{i}) {{ ...PrDetail ... on PullRequest {{ {} }} }}\n",
                CHECKS.replace("$id", &format!("$id{i}"))
            )
        })
        .collect();
    format!("query PrDetails($login: String!{variables}) {{\n{fields}}}\n{PR_DETAIL}")
}

/// The head commit's checks; `$id` is the PR's.
const CHECKS: &str = "checks: commits(last: 1) { nodes { commit { statusCheckRollup { \
    contexts(first: 100) { nodes { __typename \
      ... on CheckRun { name status conclusion isRequired(pullRequestId: $id) } \
      ... on StatusContext { context state isRequired(pullRequestId: $id) } \
    } } } } } }";

const PR_DETAIL: &str = r#"
fragment Actor on Actor { login __typename }

fragment PrDetail on PullRequest {
  id
  reviewRequests(first: 30) {
    nodes { requestedReviewer { ...Reviewer } }
  }
  myReviews: reviews(last: 20, author: $login) {
    nodes { state submittedAt }
  }
  latestOpinionatedReviews(first: 50) { nodes { state } }
  reviewThreads(first: 100) { nodes { isResolved } }
  timelineItems(last: 50, itemTypes: [
    PULL_REQUEST_COMMIT, HEAD_REF_FORCE_PUSHED_EVENT, ISSUE_COMMENT,
    PULL_REQUEST_REVIEW, READY_FOR_REVIEW_EVENT, REVIEW_REQUESTED_EVENT
  ]) {
    nodes {
      __typename
      ... on PullRequestCommit {
        commit { committedDate author { user { login } } }
      }
      ... on HeadRefForcePushedEvent { actor { ...Actor } createdAt }
      ... on IssueComment { author { ...Actor } createdAt body }
      ... on PullRequestReview { author { ...Actor } state submittedAt body }
      ... on ReadyForReviewEvent { actor { ...Actor } createdAt }
      ... on ReviewRequestedEvent {
        actor { ...Actor }
        createdAt
        requestedReviewer { ...Reviewer }
      }
    }
  }
}

fragment Reviewer on RequestedReviewer {
  __typename
  ... on User { login }
  ... on Bot { login }
  ... on Mannequin { login }
  ... on Team { slug organization { login } }
}
"#;

/// The viewer's organizations, for listing their teams during setup.
pub const VIEWER_ORGS: &str = r#"
query ViewerOrgs {
  viewer { login organizations(first: 100) { nodes { login } } }
}
"#;

/// The teams in one organization that `$login` is on. One request per
/// organization, so one that refuses (SAML SSO, say) doesn't hide the rest.
pub const ORG_TEAMS: &str = r#"
query OrgTeams($org: String!, $login: String!) {
  organization(login: $org) {
    teams(first: 100, userLogins: [$login]) { nodes { slug } }
  }
}
"#;

/// Whether a PR is open, merged, or closed. One PR per request: an id that
/// no longer resolves makes the whole request fail.
pub const PR_STATE: &str = r#"
query PrState($id: ID!) {
  node(id: $id) { ... on PullRequest { state } }
}
"#;

/// How many issues or PRs match a search, without fetching any.
pub const COUNT: &str = r#"
query Count($query: String!) {
  search(type: ISSUE, query: $query, first: 0) { issueCount }
}
"#;
