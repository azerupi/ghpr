//! Working out whose turn a pull request is, from its activity.

use jiff::Timestamp;

use crate::glob::Glob;

use super::{
    Activity, ActivityKind, CiState, Mergeable, MyReview, ReviewDecision, ReviewState, Reviewer,
    Role, Turn,
};

/// Labels that mean "don't review or merge this yet" (compared case-insensitively).
const BLOCKING_LABELS: &[&str] = &["blocked", "wip", "do not merge", "do-not-merge"];

/// The user the turn is computed for, plus the settings that affect it.
#[derive(Debug, Clone, Default)]
pub struct Viewer {
    pub login: String,
    /// Extra logins treated as bots (GitHub App bots are detected automatically).
    pub bots: Vec<String>,
    /// Teams (`org/slug`) whose review requests concern the viewer.
    pub teams: Vec<String>,
    /// How team requests count, unless a repo pattern below says otherwise.
    pub team_mode: TeamMode,
    /// Per-repo overrides; the most specific matching pattern wins.
    pub team_repos: Vec<(Glob, TeamMode)>,
}

/// What a review request to one of the viewer's teams means.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TeamMode {
    /// As if requested from the viewer: turn `me`.
    Mine,
    /// Its own turn, `team`, ranked below `me`.
    #[default]
    Team,
    /// Not at all: team-only PRs aren't shown.
    Ignore,
}

impl Viewer {
    fn is(&self, login: &str) -> bool {
        same_login(&self.login, login)
    }

    fn is_bot(&self, login: &str) -> bool {
        self.bots.iter().any(|b| same_login(b, login))
    }

    fn has_team(&self, team: &str) -> bool {
        self.teams.iter().any(|t| t.eq_ignore_ascii_case(team))
    }

    /// How team requests count in `repo` (`owner/name`).
    pub fn team_mode(&self, repo: &str) -> TeamMode {
        self.team_repos
            .iter()
            .filter(|(glob, _)| glob.matches(repo))
            .max_by_key(|(glob, _)| glob.specificity())
            .map_or(self.team_mode, |(_, mode)| *mode)
    }
}

/// Something that happened on a PR, as input to the turn computation.
#[derive(Debug, Clone)]
pub struct Event {
    /// `None` for deleted accounts, or commits whose email matches no account.
    pub actor: Option<String>,
    pub actor_is_bot: bool,
    pub kind: ActivityKind,
    pub at: Timestamp,
    pub mentions_viewer: bool,
    /// For `ReviewRequested`: who was requested.
    pub requested: Option<Reviewer>,
}

/// The parts of a PR the turn depends on.
#[derive(Debug, Clone)]
pub struct Facts<'a> {
    /// `owner/name`
    pub repo: &'a str,
    pub author: Option<&'a str>,
    pub is_draft: bool,
    pub labels: &'a [String],
    pub review_decision: Option<ReviewDecision>,
    pub ci: CiState,
    pub mergeable: Mergeable,
    pub requested: &'a [Reviewer],
    pub created_at: Timestamp,
    /// The viewer's submitted reviews, in any order.
    pub my_reviews: &'a [MyReview],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    pub turn: Turn,
    pub waiting_since: Timestamp,
    pub last_activity: Option<Activity>,
    pub mentions_me: bool,
    pub my_review: Option<MyReview>,
    pub commits_since_my_review: u32,
    /// The latest activity by someone other than the viewer (and not a bot).
    pub others_active_at: Option<Timestamp>,
}

/// Which side of the review an event is on. The ball passes between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Author,
    Reviewer,
}

struct Classified<'e> {
    event: &'e Event,
    role: Role,
    side: Side,
}

pub fn derive(facts: &Facts, events: &[Event], viewer: &Viewer) -> Derived {
    let viewer_is_author = facts.author.is_some_and(|a| viewer.is(a));

    let mut sorted: Vec<&Event> = events.iter().collect();
    sorted.sort_by_key(|e| e.at);

    // Events by people, each tagged with a side. Bots never pass the ball, and a
    // review request only does when the author makes it (asking for another look).
    let activity: Vec<Classified> = sorted
        .iter()
        .filter_map(|&event| {
            let role = role(event, facts.author, viewer);
            let side = match role {
                Role::Bot => return None,
                Role::Author => Side::Author,
                Role::Reviewer => Side::Reviewer,
                Role::Me if viewer_is_author => Side::Author,
                Role::Me => Side::Reviewer,
            };
            // Pushing code or marking ready is always the author's side of the exchange.
            let side = match event.kind {
                ActivityKind::Commit | ActivityKind::ForcePush | ActivityKind::ReadyForReview
                    if role != Role::Me =>
                {
                    Side::Author
                }
                ActivityKind::ReviewRequested if side != Side::Author => return None,
                _ => side,
            };
            Some(Classified { event, role, side })
        })
        .collect();

    let last_activity = activity.last().map(|c| Activity {
        who: c.event.actor.clone().unwrap_or_else(|| "ghost".to_owned()),
        role: c.role,
        kind: c.event.kind,
        at: c.event.at,
    });

    let others_active_at = activity
        .iter()
        .rev()
        .find(|c| c.role != Role::Me)
        .map(|c| c.event.at);

    let my_review = facts.my_reviews.iter().max_by_key(|r| r.at).copied();
    let my_last_at = activity
        .iter()
        .filter(|c| c.role == Role::Me)
        .map(|c| c.event.at)
        .chain(my_review.map(|r| r.at))
        .max();
    let after_me = |at: Timestamp| my_last_at.is_none_or(|mine| at > mine);

    let commits_since_my_review = my_review.map_or(0, |review| {
        sorted
            .iter()
            .filter(|e| e.kind == ActivityKind::Commit && e.at > review.at)
            .count() as u32
    });

    let mentions_me = sorted.iter().any(|e| e.mentions_viewer);
    let first_new_mention = activity
        .iter()
        .find(|c| c.event.mentions_viewer && c.role != Role::Me && after_me(c.event.at))
        .map(|c| c.event.at);

    let last_side = activity.last().map(|c| c.side);
    // When the current side's run of activity started, i.e. when the ball landed.
    let run_start = |side: Side| -> Option<Timestamp> {
        activity
            .iter()
            .rev()
            .take_while(|c| c.side == side)
            .last()
            .map(|c| c.event.at)
    };
    let fallback = activity
        .iter()
        .rev()
        .find(|c| c.event.kind == ActivityKind::ReadyForReview)
        .map_or(facts.created_at, |c| c.event.at);
    let latest_request_for = |target: &dyn Fn(&Reviewer) -> bool| {
        sorted
            .iter()
            .rev()
            .find(|e| {
                e.kind == ActivityKind::ReviewRequested && e.requested.as_ref().is_some_and(target)
            })
            .map(|e| e.at)
    };

    let blocking_label = facts
        .labels
        .iter()
        .any(|l| BLOCKING_LABELS.iter().any(|b| l.eq_ignore_ascii_case(b)));
    let broken = facts.ci == CiState::Fail || facts.mergeable == Mergeable::Conflicting;
    let mergeable = facts.review_decision == Some(ReviewDecision::Approved)
        && matches!(facts.ci, CiState::Pass | CiState::None)
        && facts.mergeable == Mergeable::Clean;
    let last_at = last_activity.as_ref().map_or(facts.created_at, |a| a.at);

    let (turn, since) = if facts.is_draft || blocking_label {
        (Turn::Blocked, Some(last_at))
    } else if viewer_is_author {
        if last_side == Some(Side::Reviewer) {
            (Turn::Me, run_start(Side::Reviewer))
        } else if broken {
            (Turn::Me, Some(last_at))
        } else if mergeable {
            (Turn::Merge, Some(last_at))
        } else {
            (Turn::Reviewers, run_start(Side::Author))
        }
    } else {
        let directly_requested = facts
            .requested
            .iter()
            .any(|r| matches!(r, Reviewer::User(login) if viewer.is(login)));
        let requested_team = facts.requested.iter().find_map(|r| match r {
            Reviewer::Team(team) if viewer.has_team(team) => Some(team.as_str()),
            _ => None,
        });
        let awaiting_rereview = my_review.is_some_and(|r| r.state != ReviewState::Approved)
            && last_side == Some(Side::Author)
            && after_me(last_at);

        if let Some(at) = first_new_mention {
            (Turn::Me, Some(at))
        } else if broken {
            (Turn::Blocked, Some(last_at))
        } else if directly_requested {
            let since = latest_request_for(&|r| matches!(r, Reviewer::User(l) if viewer.is(l)));
            (Turn::Me, since.or_else(|| run_start(Side::Author)))
        } else if awaiting_rereview {
            let first_response = activity
                .iter()
                .find(|c| c.side == Side::Author && after_me(c.event.at))
                .map(|c| c.event.at);
            (Turn::Me, first_response)
        } else if let Some((team, mode)) = requested_team
            .filter(|_| my_review.is_none())
            .map(|team| (team, viewer.team_mode(facts.repo)))
            .filter(|(_, mode)| *mode != TeamMode::Ignore)
        {
            let turn = if mode == TeamMode::Mine {
                Turn::Me
            } else {
                Turn::Team
            };
            let since = latest_request_for(
                &|r| matches!(r, Reviewer::Team(t) if t.eq_ignore_ascii_case(team)),
            );
            (turn, since)
        } else if mergeable {
            (Turn::Merge, Some(last_at))
        } else if last_side == Some(Side::Reviewer) {
            (Turn::Author, run_start(Side::Reviewer))
        } else {
            (Turn::Reviewers, run_start(Side::Author))
        }
    };

    Derived {
        turn,
        waiting_since: since.unwrap_or(fallback),
        last_activity,
        mentions_me,
        my_review,
        commits_since_my_review,
        others_active_at,
    }
}

fn role(event: &Event, author: Option<&str>, viewer: &Viewer) -> Role {
    match event.actor.as_deref() {
        Some(login) if viewer.is(login) => Role::Me,
        // Checked before bots, so a bot's activity on its own PR counts as the author's.
        Some(login) if author.is_some_and(|a| same_login(a, login)) => Role::Author,
        _ if event.actor_is_bot => Role::Bot,
        Some(login) if viewer.is_bot(login) => Role::Bot,
        // Commits whose email matches no account are almost always the author's.
        None if matches!(event.kind, ActivityKind::Commit) => Role::Author,
        _ => Role::Reviewer,
    }
}

/// Logins are case-insensitive, and the API names a bot `dependabot` in some
/// places and `dependabot[bot]` in others.
fn same_login(a: &str, b: &str) -> bool {
    let strip = |s: &str| s.strip_suffix("[bot]").unwrap_or(s).to_owned();
    strip(a).eq_ignore_ascii_case(&strip(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "me";
    const AUTHOR: &str = "alice";

    fn t(day: u32, hour: u32) -> Timestamp {
        format!("2026-09-{day:02}T{hour:02}:00:00Z")
            .parse()
            .unwrap()
    }

    fn viewer() -> Viewer {
        Viewer {
            login: ME.to_owned(),
            bots: vec!["ci-user".to_owned()],
            teams: vec!["acme/platform".to_owned()],
            team_mode: TeamMode::Team,
            team_repos: Vec::new(),
        }
    }

    #[derive(Clone)]
    struct Pr {
        repo: &'static str,
        author: &'static str,
        is_draft: bool,
        labels: Vec<String>,
        review_decision: Option<ReviewDecision>,
        ci: CiState,
        mergeable: Mergeable,
        requested: Vec<Reviewer>,
        my_reviews: Vec<MyReview>,
        events: Vec<Event>,
    }

    impl Pr {
        fn by(author: &'static str) -> Self {
            Pr {
                repo: "acme/api",
                author,
                is_draft: false,
                labels: Vec::new(),
                review_decision: Some(ReviewDecision::ReviewRequired),
                ci: CiState::Pass,
                mergeable: Mergeable::Clean,
                requested: Vec::new(),
                my_reviews: Vec::new(),
                events: Vec::new(),
            }
        }

        fn event(mut self, actor: &str, kind: ActivityKind, at: Timestamp) -> Self {
            self.events.push(Event {
                actor: Some(actor.to_owned()),
                actor_is_bot: false,
                kind,
                at,
                mentions_viewer: false,
                requested: None,
            });
            self
        }

        fn commit(self, at: Timestamp) -> Self {
            let author = self.author;
            self.event(author, ActivityKind::Commit, at)
        }

        fn comment(self, actor: &str, at: Timestamp) -> Self {
            self.event(actor, ActivityKind::Comment, at)
        }

        fn review(mut self, actor: &str, state: ReviewState, at: Timestamp) -> Self {
            if actor == ME {
                self.my_reviews.push(MyReview { state, at });
            }
            self.event(actor, ActivityKind::Review(state), at)
        }

        fn request(mut self, actor: &str, reviewer: Reviewer, at: Timestamp) -> Self {
            self.requested.push(reviewer.clone());
            self = self.event(actor, ActivityKind::ReviewRequested, at);
            self.events.last_mut().unwrap().requested = Some(reviewer);
            self
        }

        fn mention(mut self, actor: &str, at: Timestamp) -> Self {
            self = self.comment(actor, at);
            self.events.last_mut().unwrap().mentions_viewer = true;
            self
        }

        fn derive_for(&self, viewer: &Viewer) -> Derived {
            let facts = Facts {
                repo: self.repo,
                author: Some(self.author),
                is_draft: self.is_draft,
                labels: &self.labels,
                review_decision: self.review_decision,
                ci: self.ci,
                mergeable: self.mergeable,
                requested: &self.requested,
                created_at: t(1, 0),
                my_reviews: &self.my_reviews,
            };
            derive(&facts, &self.events, viewer)
        }

        fn derive(&self) -> Derived {
            self.derive_for(&viewer())
        }
    }

    fn user(login: &str) -> Reviewer {
        Reviewer::User(login.to_owned())
    }

    // --- Someone else's PR ---

    #[test]
    fn direct_request_is_my_turn_since_the_request() {
        let d = Pr::by(AUTHOR)
            .commit(t(2, 9))
            .request(AUTHOR, user(ME), t(2, 10))
            .derive();
        assert_eq!(d.turn, Turn::Me);
        assert_eq!(d.waiting_since, t(2, 10));
    }

    #[test]
    fn author_pushing_after_my_change_request_is_my_turn_again() {
        // GitHub removed me from the requested reviewers when I reviewed.
        let d = Pr::by(AUTHOR)
            .commit(t(2, 9))
            .review(ME, ReviewState::ChangesRequested, t(3, 9))
            .commit(t(4, 9))
            .commit(t(4, 10))
            .derive();
        assert_eq!(d.turn, Turn::Me);
        assert_eq!(
            d.waiting_since,
            t(4, 9),
            "since the author's first response"
        );
        assert_eq!(d.commits_since_my_review, 2);
        assert_eq!(
            d.my_review.map(|r| r.state),
            Some(ReviewState::ChangesRequested)
        );
    }

    #[test]
    fn author_replying_to_my_comments_is_my_turn() {
        let d = Pr::by(AUTHOR)
            .review(ME, ReviewState::Commented, t(3, 9))
            .comment(AUTHOR, t(3, 12))
            .derive();
        assert_eq!(d.turn, Turn::Me);
        assert_eq!(d.commits_since_my_review, 0);
    }

    #[test]
    fn pushes_after_my_approval_are_not_my_turn() {
        let d = Pr::by(AUTHOR)
            .review(ME, ReviewState::Approved, t(3, 9))
            .commit(t(4, 9))
            .derive();
        assert_eq!(d.turn, Turn::Reviewers);
        assert_eq!(d.commits_since_my_review, 1, "still visible as new changes");
    }

    #[test]
    fn waiting_on_the_author_after_my_review() {
        let d = Pr::by(AUTHOR)
            .commit(t(2, 9))
            .review(ME, ReviewState::ChangesRequested, t(3, 9))
            .comment("bob", t(3, 11))
            .derive();
        assert_eq!(d.turn, Turn::Author);
        assert_eq!(
            d.waiting_since,
            t(3, 9),
            "since the start of the reviewers' run"
        );
        let last = d.last_activity.unwrap();
        assert_eq!((last.who.as_str(), last.role), ("bob", Role::Reviewer));
    }

    #[test]
    fn bots_do_not_pass_the_ball() {
        let mut pr = Pr::by(AUTHOR)
            .review(ME, ReviewState::Commented, t(3, 9))
            .commit(t(4, 9))
            .comment("ci-user", t(4, 10))
            .comment("dependabot", t(4, 11));
        pr.events.last_mut().unwrap().actor_is_bot = true;
        let d = pr.derive();
        assert_eq!(d.turn, Turn::Me);
        assert_eq!(d.last_activity.unwrap().role, Role::Author);
    }

    #[test]
    fn a_bots_own_activity_on_its_pr_is_the_authors() {
        // The API names the PR author `dependabot` but its commits `dependabot[bot]`.
        let mut pr = Pr::by("dependabot")
            .commit(t(2, 9))
            .comment("dependabot[bot]", t(2, 10))
            .comment("github-actions[bot]", t(2, 11));
        pr.events[0].actor = Some("dependabot[bot]".to_owned());
        for event in &mut pr.events {
            event.actor_is_bot = true;
        }
        let d = pr.derive();
        let last = d.last_activity.unwrap();
        assert_eq!((last.role, last.at), (Role::Author, t(2, 10)));
        assert_eq!(d.turn, Turn::Reviewers);
    }

    #[test]
    fn a_reviewer_requesting_someone_else_does_not_count_as_activity() {
        let d = Pr::by(AUTHOR)
            .review(ME, ReviewState::Commented, t(3, 9))
            .comment(AUTHOR, t(3, 10))
            .request("bob", user("carol"), t(3, 11))
            .derive();
        assert_eq!(d.turn, Turn::Me);
        assert_eq!(d.last_activity.unwrap().who, AUTHOR);
    }

    #[test]
    fn failing_ci_blocks_even_when_requested() {
        let mut pr = Pr::by(AUTHOR).request(AUTHOR, user(ME), t(2, 10));
        pr.ci = CiState::Fail;
        assert_eq!(pr.derive().turn, Turn::Blocked);

        let mut pr = Pr::by(AUTHOR).request(AUTHOR, user(ME), t(2, 10));
        pr.mergeable = Mergeable::Conflicting;
        assert_eq!(pr.derive().turn, Turn::Blocked);
    }

    #[test]
    fn drafts_and_blocking_labels_are_blocked() {
        let mut pr = Pr::by(AUTHOR).request(AUTHOR, user(ME), t(2, 10));
        pr.is_draft = true;
        assert_eq!(pr.derive().turn, Turn::Blocked);

        let mut pr = Pr::by(AUTHOR).request(AUTHOR, user(ME), t(2, 10));
        pr.labels = vec!["Do Not Merge".to_owned()];
        assert_eq!(pr.derive().turn, Turn::Blocked);
    }

    #[test]
    fn approved_and_green_is_ready_to_merge() {
        let mut pr = Pr::by(AUTHOR).review("bob", ReviewState::Approved, t(3, 9));
        pr.review_decision = Some(ReviewDecision::Approved);
        assert_eq!(pr.derive().turn, Turn::Merge);
    }

    #[test]
    fn team_requests_are_their_own_turn_unless_configured() {
        let pr = Pr::by(AUTHOR).request(AUTHOR, Reviewer::Team("acme/platform".into()), t(2, 10));
        let d = pr.derive();
        assert_eq!(d.turn, Turn::Team);
        assert_eq!(d.waiting_since, t(2, 10));

        let counts = Viewer {
            team_mode: TeamMode::Mine,
            ..viewer()
        };
        assert_eq!(pr.derive_for(&counts).turn, Turn::Me);

        let other = Pr::by(AUTHOR).request(AUTHOR, Reviewer::Team("acme/web".into()), t(2, 10));
        assert_eq!(other.derive().turn, Turn::Reviewers, "not one of my teams");
    }

    #[test]
    fn team_mode_per_repo_most_specific_wins() {
        let pr = Pr::by(AUTHOR).request(AUTHOR, Reviewer::Team("acme/platform".into()), t(2, 10));
        let viewer = Viewer {
            team_repos: vec![
                (Glob::new("acme/*"), TeamMode::Mine),
                (Glob::new("acme/leg*"), TeamMode::Ignore),
                (Glob::new("acme/legacy-web"), TeamMode::Team),
            ],
            ..viewer()
        };
        let turn_in = |repo: &'static str| Pr { repo, ..pr.clone() }.derive_for(&viewer).turn;
        assert_eq!(turn_in("acme/api"), Turn::Me);
        assert_eq!(
            turn_in("acme/legacy"),
            Turn::Reviewers,
            "ignored: as if not requested"
        );
        assert_eq!(turn_in("acme/legacy-web"), Turn::Team, "exact beats globs");
        assert_eq!(turn_in("other/repo"), Turn::Team, "the default");
        assert_eq!(
            viewer.team_mode("ACME/API"),
            TeamMode::Mine,
            "case-insensitive"
        );
    }

    #[test]
    fn team_request_after_i_reviewed_is_not_team_turn() {
        let d = Pr::by(AUTHOR)
            .request(AUTHOR, Reviewer::Team("acme/platform".into()), t(2, 10))
            .review(ME, ReviewState::Approved, t(3, 9))
            .derive();
        assert_ne!(d.turn, Turn::Team);
    }

    #[test]
    fn new_mentions_are_my_turn() {
        let d = Pr::by(AUTHOR)
            .review(ME, ReviewState::Approved, t(3, 9))
            .mention("bob", t(4, 9))
            .derive();
        assert_eq!(d.turn, Turn::Me);
        assert_eq!(d.waiting_since, t(4, 9));
        assert!(d.mentions_me);
    }

    #[test]
    fn mentions_i_already_responded_to_are_not_my_turn() {
        let d = Pr::by(AUTHOR)
            .mention("bob", t(3, 9))
            .comment(ME, t(3, 10))
            .derive();
        assert_ne!(d.turn, Turn::Me);
        assert!(d.mentions_me);
    }

    #[test]
    fn tracks_the_latest_activity_by_someone_else() {
        let d = Pr::by(AUTHOR)
            .commit(t(2, 9))
            .comment("ci-user", t(2, 11))
            .review(ME, ReviewState::Commented, t(3, 9))
            .derive();
        assert_eq!(d.others_active_at, Some(t(2, 9)), "not mine, not the bot's");
        assert_eq!(Pr::by(AUTHOR).derive().others_active_at, None);
    }

    #[test]
    fn logins_compare_case_insensitively() {
        let d = Pr::by(AUTHOR)
            .request(AUTHOR, user("ME"), t(2, 10))
            .derive();
        assert_eq!(d.turn, Turn::Me);
    }

    // --- My own PR ---

    #[test]
    fn reviewer_feedback_on_my_pr_is_my_turn() {
        let d = Pr::by(ME)
            .commit(t(2, 9))
            .review("bob", ReviewState::ChangesRequested, t(3, 9))
            .comment("carol", t(3, 10))
            .derive();
        assert_eq!(d.turn, Turn::Me);
        assert_eq!(d.waiting_since, t(3, 9));
    }

    #[test]
    fn my_pr_waiting_on_reviewers() {
        let d = Pr::by(ME)
            .commit(t(2, 9))
            .request(ME, user("bob"), t(2, 10))
            .derive();
        assert_eq!(d.turn, Turn::Reviewers);
        assert_eq!(d.waiting_since, t(2, 9));
        assert_eq!(d.last_activity.unwrap().role, Role::Me);
    }

    #[test]
    fn my_pr_with_failing_ci_is_my_turn() {
        let mut pr = Pr::by(ME).commit(t(2, 9));
        pr.ci = CiState::Fail;
        assert_eq!(pr.derive().turn, Turn::Me);
    }

    #[test]
    fn my_approved_pr_is_ready_to_merge() {
        let mut pr = Pr::by(ME)
            .commit(t(2, 9))
            .review("bob", ReviewState::Approved, t(3, 9))
            .commit(t(3, 10));
        pr.review_decision = Some(ReviewDecision::Approved);
        assert_eq!(pr.derive().turn, Turn::Merge);
    }

    // --- Waiting-since fallbacks ---

    #[test]
    fn no_activity_falls_back_to_creation() {
        let d = Pr::by(AUTHOR).derive();
        assert_eq!(d.turn, Turn::Reviewers);
        assert_eq!(d.waiting_since, t(1, 0));
        assert!(d.last_activity.is_none());
    }

    #[test]
    fn events_are_ordered_by_time_not_input_order() {
        let d = Pr::by(AUTHOR)
            .comment(AUTHOR, t(4, 9))
            .review(ME, ReviewState::Commented, t(3, 9))
            .derive();
        assert_eq!(d.turn, Turn::Me);
    }
}
