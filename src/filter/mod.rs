//! Client-side filtering of pull requests.
//!
//! Terms are ANDed; `-` negates a term, and `a,b` means either value.

pub mod complete;
mod parse;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::glob::Glob;
use crate::model::{CiState, Pr, ReviewDecision, ReviewState, Reviewer, Role, Turn};
use crate::state::{Flags, LocalState};

#[derive(Debug, Clone, Default)]
pub struct Filter {
    terms: Vec<Term>,
}

#[derive(Debug, Clone)]
struct Term {
    negated: bool,
    pred: Pred,
}

#[derive(Debug, Clone)]
enum Pred {
    Repo(Vec<Glob>),
    Org(Vec<Glob>),
    Author(Vec<User>),
    Reviewer(Vec<User>),
    Team(Vec<Glob>),
    Label(Vec<Glob>),
    Base(Vec<Glob>),
    Head(Vec<Glob>),
    Turn(Vec<Turn>),
    Last(Vec<LastRole>),
    Ci(Vec<CiState>),
    Review(Vec<Option<ReviewDecision>>),
    Mine(Vec<MineState>),
    Age(Vec<TimeCond>),
    Updated(Vec<TimeCond>),
    Waiting(Vec<TimeCond>),
    Size(Vec<Cmp<u32>>),
    Files(Vec<Cmp<u32>>),
    Threads(Vec<Cmp<u32>>),
    Draft,
    Conflict,
    Bot,
    Mentioned,
    New,
    Snoozed,
    Muted,
    /// Bare words: matched against title, branch, repo, and author; `#123` matches the number.
    Text(String),
}

#[derive(Debug, Clone)]
enum User {
    Me,
    Login(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastRole {
    Me,
    Author,
    Reviewer,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MineState {
    Unreviewed,
    /// Reviewed, with commits since.
    Stale,
    Approved,
    Changes,
    Commented,
}

#[derive(Debug, Clone, Copy)]
enum SizeClass {
    Xs,
    S,
    M,
    L,
    Xl,
}

impl SizeClass {
    fn range(self) -> Cmp<u32> {
        match self {
            SizeClass::Xs => Cmp::Lt(10),
            SizeClass::S => Cmp::Range(10, 99),
            SizeClass::M => Cmp::Range(100, 399),
            SizeClass::L => Cmp::Range(400, 999),
            SizeClass::Xl => Cmp::Ge(1000),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Cmp<T> {
    Eq(T),
    Gt(T),
    Ge(T),
    Lt(T),
    Le(T),
    /// Inclusive on both ends.
    Range(T, T),
}

impl<T: PartialOrd> Cmp<T> {
    fn matches(&self, v: T) -> bool {
        match self {
            Cmp::Eq(x) => v == *x,
            Cmp::Gt(x) => v > *x,
            Cmp::Ge(x) => v >= *x,
            Cmp::Lt(x) => v < *x,
            Cmp::Le(x) => v <= *x,
            Cmp::Range(lo, hi) => v >= *lo && v <= *hi,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TimeCond {
    /// Compared against how many seconds ago the timestamp was.
    Ago(Cmp<i64>),
    Between {
        from: Option<Timestamp>,
        until: Option<Timestamp>,
    },
}

impl TimeCond {
    fn matches(&self, at: Timestamp, now: Timestamp) -> bool {
        match self {
            TimeCond::Ago(cmp) => cmp.matches(now.as_second() - at.as_second()),
            TimeCond::Between { from, until } => {
                from.is_none_or(|f| at >= f) && until.is_none_or(|u| at < u)
            }
        }
    }
}

/// What filter evaluation needs beyond the PR itself.
pub struct Context<'a> {
    /// Resolves `@me`.
    pub viewer: &'a str,
    pub now: Timestamp,
    /// Seen, snoozed, and muted; without it nothing is any of those.
    pub local: Option<&'a LocalState>,
}

impl Context<'_> {
    fn flags(&self, pr: &Pr) -> Flags {
        self.local
            .map_or_else(Flags::default, |s| s.flags(pr, self.now))
    }
}

impl Filter {
    pub fn parse(text: &str) -> Result<Self, String> {
        parse::parse(text, &TimeZone::system())
    }

    pub fn matches(&self, pr: &Pr, ctx: &Context) -> bool {
        self.terms
            .iter()
            .all(|term| term.pred.matches(pr, ctx) != term.negated)
    }

    /// Whether the PR is shown: it matches, and it isn't snoozed or muted unless
    /// the filter asks about those (`snoozed`, `-muted`, ...).
    pub fn shows(&self, pr: &Pr, ctx: &Context) -> bool {
        let mentions = |p: fn(&Pred) -> bool| self.terms.iter().any(|t| p(&t.pred));
        let flags = ctx.flags(pr);
        self.matches(pr, ctx)
            && (flags.snoozed.is_none() || mentions(|p| matches!(p, Pred::Snoozed)))
            && (!flags.muted || mentions(|p| matches!(p, Pred::Muted)))
    }
}

impl Pred {
    fn matches(&self, pr: &Pr, ctx: &Context) -> bool {
        let is_user = |user: &User, login: &str| match user {
            User::Me => login.eq_ignore_ascii_case(ctx.viewer),
            User::Login(l) => login.eq_ignore_ascii_case(l),
        };
        let requested_users = || {
            pr.requested_reviewers.iter().filter_map(|r| match r {
                Reviewer::User(login) => Some(login.as_str()),
                Reviewer::Team(_) => None,
            })
        };
        match self {
            Pred::Repo(globs) => globs.iter().any(|g| g.matches(&pr.repo.to_string())),
            Pred::Org(globs) => globs.iter().any(|g| g.matches(&pr.repo.owner)),
            Pred::Author(users) => {
                let author = pr.author.as_deref().unwrap_or("ghost");
                users.iter().any(|u| is_user(u, author))
            }
            Pred::Reviewer(users) => users.iter().any(|u| {
                requested_users().any(|login| is_user(u, login))
                    || (matches!(u, User::Me) && pr.my_review.is_some())
            }),
            Pred::Team(globs) => pr.requested_reviewers.iter().any(|r| match r {
                Reviewer::Team(team) => globs.iter().any(|g| {
                    g.matches(team)
                        || team
                            .split_once('/')
                            .is_some_and(|(_, slug)| g.matches(slug))
                }),
                Reviewer::User(_) => false,
            }),
            Pred::Label(globs) => pr.labels.iter().any(|l| globs.iter().any(|g| g.matches(l))),
            Pred::Base(globs) => globs.iter().any(|g| g.matches(&pr.base_ref)),
            Pred::Head(globs) => globs.iter().any(|g| g.matches(&pr.head_ref)),
            Pred::Turn(turns) => turns.contains(&pr.turn),
            Pred::Last(roles) => {
                let role = match pr.last_activity.as_ref().map(|a| a.role) {
                    Some(Role::Me) => LastRole::Me,
                    Some(Role::Author) => LastRole::Author,
                    Some(Role::Reviewer) => LastRole::Reviewer,
                    Some(Role::Bot) | None => LastRole::None,
                };
                roles.contains(&role)
            }
            Pred::Ci(states) => states.contains(&pr.ci),
            Pred::Review(decisions) => decisions.contains(&pr.review_decision),
            Pred::Mine(states) => states.iter().any(|state| {
                let mine = pr.my_review.map(|r| r.state);
                match state {
                    MineState::Unreviewed => mine.is_none(),
                    MineState::Stale => mine.is_some() && pr.commits_since_my_review > 0,
                    MineState::Approved => mine == Some(ReviewState::Approved),
                    MineState::Changes => mine == Some(ReviewState::ChangesRequested),
                    MineState::Commented => mine == Some(ReviewState::Commented),
                }
            }),
            Pred::Age(conds) => conds.iter().any(|c| c.matches(pr.created_at, ctx.now)),
            Pred::Updated(conds) => conds.iter().any(|c| c.matches(pr.updated_at, ctx.now)),
            Pred::Waiting(conds) => conds.iter().any(|c| c.matches(pr.waiting_since, ctx.now)),
            Pred::Size(cmps) => cmps.iter().any(|c| c.matches(pr.additions + pr.deletions)),
            Pred::Files(cmps) => cmps.iter().any(|c| c.matches(pr.changed_files)),
            Pred::Threads(cmps) => cmps.iter().any(|c| c.matches(pr.unresolved_threads)),
            Pred::Draft => pr.is_draft,
            Pred::Conflict => pr.mergeable == crate::model::Mergeable::Conflicting,
            Pred::Bot => pr.author_is_bot,
            Pred::Mentioned => pr.mentions_me,
            Pred::New => ctx.flags(pr).new,
            Pred::Snoozed => ctx.flags(pr).snoozed.is_some(),
            Pred::Muted => ctx.flags(pr).muted,
            Pred::Text(word) => {
                if let Some(n) = word.strip_prefix('#').and_then(|n| n.parse::<u64>().ok()) {
                    return pr.number == n;
                }
                [
                    pr.title.to_lowercase(),
                    pr.head_ref.to_lowercase(),
                    pr.repo.to_string().to_lowercase(),
                    pr.author.as_deref().unwrap_or("").to_lowercase(),
                ]
                .iter()
                .any(|field| field.contains(word.as_str()))
            }
        }
    }
}

/// Whether a value is part of the filter text, as `key:value` or `-key:value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    None,
    Included,
    Excluded,
}

/// How `key:value` appears in the filter text.
pub fn mark(text: &str, key: &str, value: &str) -> Mark {
    let tokens = raw_tokens(text);
    let has = |negated| {
        tokens.iter().any(|t| {
            key_values(t, key, negated).is_some_and(|vs| vs.iter().any(|v| same_value(v, value)))
        })
    };
    if has(false) {
        Mark::Included
    } else if has(true) {
        Mark::Excluded
    } else {
        Mark::None
    }
}

/// Add `value` to the `key:` term (or the `-key:` term when `negated`), or take
/// it out if it's already there. Values of one key are joined with commas, so
/// including two repos means either of them. Including a value removes it from
/// the excluded ones, and the other way round.
pub fn toggle_value(text: &str, key: &str, value: &str, negated: bool) -> String {
    let mut tokens = raw_tokens(text);
    let quoted = if value.contains(' ') {
        format!("\"{value}\"")
    } else {
        value.to_owned()
    };
    let prefix = format!("{}{key}:", if negated { "-" } else { "" });

    // Out of the opposite term first.
    edit_values(&mut tokens, key, !negated, |values| {
        values.retain(|v| !same_value(v, value))
    });

    let exists = tokens.iter().any(|t| key_values(t, key, negated).is_some());
    if exists {
        edit_values(&mut tokens, key, negated, |values| {
            if values.iter().any(|v| same_value(v, value)) {
                values.retain(|v| !same_value(v, value));
            } else {
                values.push(quoted.clone());
            }
        });
    } else {
        tokens.push(format!("{prefix}{quoted}"));
    }
    tokens.join(" ")
}

/// Apply `edit` to the values of the first `key:` (or `-key:`) term, dropping
/// the term if no values are left.
fn edit_values(
    tokens: &mut Vec<String>,
    key: &str,
    negated: bool,
    edit: impl FnOnce(&mut Vec<String>),
) {
    let Some(i) = tokens
        .iter()
        .position(|t| key_values(t, key, negated).is_some())
    else {
        return;
    };
    let mut values = key_values(&tokens[i], key, negated).unwrap();
    edit(&mut values);
    if values.is_empty() {
        tokens.remove(i);
    } else {
        tokens[i] = format!(
            "{}{key}:{}",
            if negated { "-" } else { "" },
            values.join(",")
        );
    }
}

/// The comma-separated values of a `key:` (or `-key:`) token, quotes kept.
fn key_values(token: &str, key: &str, negated: bool) -> Option<Vec<String>> {
    let body = match token.strip_prefix('-') {
        Some(rest) if negated => rest,
        Some(_) => return None,
        None if negated => return None,
        None => token,
    };
    let (k, values) = body.split_once(':')?;
    k.eq_ignore_ascii_case(key).then(|| {
        values
            .split(',')
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

fn same_value(raw: &str, value: &str) -> bool {
    raw.trim_matches('"').eq_ignore_ascii_case(value)
}

/// Whitespace-separated words, keeping quoted sections (and their quotes) together.
fn raw_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in text.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
        }
        if c.is_whitespace() && !in_quotes {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Add `term` to the filter text, or remove it if it's already there.
/// Used by the one-key toggles.
pub fn toggle_term(text: &str, term: &str) -> String {
    let mut words: Vec<&str> = text.split_whitespace().collect();
    if let Some(i) = words.iter().position(|w| *w == term) {
        words.remove(i);
    } else {
        words.push(term);
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Mergeable, MyReview};
    use crate::test_support::{now, pr};

    fn parse(text: &str) -> Filter {
        parse::parse(text, &TimeZone::UTC).unwrap()
    }

    fn error(text: &str) -> String {
        parse::parse(text, &TimeZone::UTC).unwrap_err()
    }

    fn ctx() -> Context<'static> {
        Context {
            viewer: "me",
            now: now(),
            local: None,
        }
    }

    fn matches(text: &str, pr: &Pr) -> bool {
        parse(text).matches(pr, &ctx())
    }

    #[test]
    fn empty_filter_matches_everything() {
        assert!(matches("", &pr(1)));
        assert!(matches("   ", &pr(1)));
    }

    #[test]
    fn repo_and_org_globs() {
        let p = pr(1); // acme/api
        assert!(matches("repo:acme/api", &p));
        assert!(matches("repo:ACME/*", &p));
        assert!(matches("repo:*/a?i", &p));
        assert!(!matches("repo:acme/web", &p));
        assert!(matches("repo:acme/web,acme/api", &p), "comma means or");
        assert!(matches("org:acme", &p));
        assert!(!matches("-org:acme", &p));
    }

    #[test]
    fn author_and_reviewer_resolve_me() {
        let mut p = pr(1); // by alice
        assert!(matches("author:alice", &p));
        assert!(matches("author:@Alice", &p));
        assert!(!matches("author:@me", &p));
        p.author = Some("me".into());
        assert!(matches("author:@me", &p));

        let mut p = pr(2);
        p.requested_reviewers = vec![
            Reviewer::User("bob".into()),
            Reviewer::Team("acme/platform".into()),
        ];
        assert!(matches("reviewer:bob", &p));
        assert!(!matches("reviewer:@me", &p));
        assert!(matches("team:platform", &p));
        assert!(matches("team:acme/*", &p));
        p.my_review = Some(MyReview {
            state: ReviewState::Approved,
            at: now(),
        });
        assert!(matches("reviewer:@me", &p), "reviewing counts too");
    }

    #[test]
    fn enums_and_flags() {
        let mut p = pr(1); // turn me, ci pass, approved
        assert!(matches("turn:me", &p));
        assert!(matches("turn:author,me", &p));
        assert!(matches("-turn:blocked", &p));
        assert!(matches("ci:pass review:approved last:author", &p));
        assert!(!matches("ci:fail", &p));
        assert!(!matches("draft", &p));
        assert!(matches("-draft -conflict -bot -mentioned", &p));
        p.is_draft = true;
        p.mergeable = Mergeable::Conflicting;
        p.author_is_bot = true;
        p.review_decision = None;
        p.last_activity = None;
        assert!(matches("draft conflict bot review:none last:none", &p));
    }

    #[test]
    fn my_review_states() {
        let mut p = pr(1);
        assert!(matches("mine:unreviewed", &p));
        p.my_review = Some(MyReview {
            state: ReviewState::ChangesRequested,
            at: now(),
        });
        assert!(matches("mine:changes", &p));
        assert!(!matches("mine:stale", &p));
        p.commits_since_my_review = 2;
        assert!(matches("mine:stale", &p));
    }

    #[test]
    fn numbers_and_sizes() {
        let mut p = pr(1); // +42 -8 = 50 lines, 3 files
        assert!(matches("size:<100", &p));
        assert!(matches("size:50", &p));
        assert!(matches("size:40..60", &p));
        assert!(!matches("size:>50", &p));
        assert!(matches("size:>=50", &p));
        assert!(matches("size:s", &p));
        assert!(!matches("size:xs,m", &p));
        assert!(matches("files:3 threads:0", &p));
        p.unresolved_threads = 2;
        assert!(matches("threads:>0", &p));
    }

    #[test]
    fn durations_ago() {
        let p = pr(1); // created 8d ago, updated 27h ago, waiting 2d
        assert!(matches("age:>7d", &p));
        assert!(matches("updated:<2d", &p));
        assert!(matches("updated:>1d", &p));
        assert!(matches("waiting:>=2d", &p));
        assert!(!matches("waiting:>3d", &p));
        assert!(matches("waiting:1d..3d", &p));
        assert!(matches("age:<2w", &p));
    }

    #[test]
    fn dates_cover_whole_days() {
        let p = pr(1); // updated 2026-09-27T09:00Z
        assert!(matches("updated:2026-09-27", &p));
        assert!(!matches("updated:2026-09-26", &p));
        assert!(matches("updated:>2026-09-26", &p));
        assert!(!matches("updated:>2026-09-27", &p));
        assert!(matches("updated:<=2026-09-27", &p));
        assert!(!matches("updated:<2026-09-27", &p));
        assert!(matches("updated:2026-09-01..2026-09-27", &p));
    }

    #[test]
    fn text_matches_title_branch_repo_author_and_number() {
        let p = pr(1234); // "Change number 1234", head "fix", acme/api, alice
        assert!(matches("change", &p));
        assert!(matches("CHANGE NUMBER", &p), "words are ANDed");
        assert!(matches("fix", &p));
        assert!(matches("alice", &p));
        assert!(matches("#1234", &p));
        assert!(!matches("#12", &p));
        assert!(!matches("unrelated", &p));
        assert!(matches("-unrelated", &p));
        assert!(matches("\"number 1234\"", &p), "quoted phrase");
    }

    #[test]
    fn quoted_values() {
        let mut p = pr(1);
        p.labels = vec!["needs review".into()];
        assert!(matches("label:\"needs review\"", &p));
        assert!(matches("label:needs*", &p));
    }

    #[test]
    fn helpful_errors() {
        assert!(error("colour:red").contains("unknown filter \"colour\""));
        assert!(error("turn:you").contains("expected me, team"));
        assert!(error("size:big").contains("size"));
        assert!(error("waiting:2d").contains("use >, <"));
        assert!(error("waiting:>2x").contains("waiting"));
        assert!(error("repo:").contains("needs a value"));
        assert!(error("label:\"oops").contains("unclosed quote"));
        assert!(error("updated:2026-13-01").contains("updated"));
    }

    #[test]
    fn lone_dash_is_text() {
        assert!(!matches("-", &pr(1)));
    }

    #[test]
    fn local_state_flags_and_default_hiding() {
        use crate::state::Snooze;
        let mut state = LocalState::new("2026-09-01T00:00:00Z".parse().unwrap());
        let (a, b, c) = (pr(1), pr(2), pr(3));
        state.mark_seen(&a.id, now());
        state.snooze(
            &b.id,
            Snooze::Until("2026-10-01T00:00:00Z".parse().unwrap()),
        );
        state.set_muted(&c.id, true);
        let ctx = Context {
            viewer: "me",
            now: now(),
            local: Some(&state),
        };
        let shown = |text: &str| -> Vec<u64> {
            [&a, &b, &c]
                .into_iter()
                .filter(|p| parse(text).shows(p, &ctx))
                .map(|p| p.number)
                .collect()
        };
        assert_eq!(shown(""), [1], "snoozed and muted are hidden");
        assert_eq!(shown("snoozed"), [2]);
        assert_eq!(shown("muted"), [3]);
        assert_eq!(shown("-muted"), [1], "b is still hidden as snoozed");
        assert_eq!(
            shown("new"),
            Vec::<u64>::new(),
            "b and c are new but hidden"
        );
        assert_eq!(shown("new snoozed"), [2]);
    }

    #[test]
    fn toggling_values_merges_alternatives() {
        let t = toggle_value("", "repo", "a/b", false);
        assert_eq!(t, "repo:a/b");
        let t = toggle_value(&t, "repo", "c/d", false);
        assert_eq!(t, "repo:a/b,c/d");
        assert!(!parse(&t).matches(&pr(1), &ctx()), "acme/api is neither");
        let t = toggle_value(&t, "repo", "A/B", false);
        assert_eq!(
            t, "repo:c/d",
            "case-insensitive; taking the last out removes the term"
        );
        let t = toggle_value(&t, "repo", "c/d", false);
        assert_eq!(t, "");
    }

    #[test]
    fn including_and_excluding_move_values_between_terms() {
        let t = toggle_value("turn:me ci:pass", "ci", "pass", true);
        assert_eq!(t, "turn:me -ci:pass");
        assert_eq!(mark(&t, "ci", "pass"), Mark::Excluded);
        let t = toggle_value(&t, "ci", "pass", false);
        assert_eq!(t, "turn:me ci:pass");
        assert_eq!(mark(&t, "ci", "pass"), Mark::Included);
        assert_eq!(mark(&t, "ci", "fail"), Mark::None);
        assert_eq!(mark(&t, "turn", "me"), Mark::Included);
    }

    #[test]
    fn quoted_values_round_trip() {
        let t = toggle_value("-draft", "label", "needs review", false);
        assert_eq!(t, "-draft label:\"needs review\"");
        assert_eq!(mark(&t, "label", "needs review"), Mark::Included);
        parse(&t);
        assert_eq!(toggle_value(&t, "label", "needs review", false), "-draft");
    }

    #[test]
    fn toggling_terms() {
        assert_eq!(toggle_term("", "-draft"), "-draft");
        assert_eq!(toggle_term("repo:a/b", "-draft"), "repo:a/b -draft");
        assert_eq!(
            toggle_term("repo:a/b  -draft turn:me", "-draft"),
            "repo:a/b turn:me"
        );
    }
}
