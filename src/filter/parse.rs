//! Parsing filter text like `repo:acme/* -draft ci:fail,pending waiting:>2d`.

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use super::{Cmp, Filter, LastRole, MineState, Pred, SizeClass, Term, TimeCond, User};
use crate::glob::Glob;
use crate::model::{CiState, ReviewDecision, Turn};

/// A `key:value` filter, for the parser's error messages and for completion.
pub struct KeyInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub values: ValueKind,
}

/// What a key's values look like, which decides what completion suggests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// A fixed list; see [`enum_values`].
    Enum,
    Repo,
    Org,
    Author,
    Reviewer,
    Team,
    Label,
    Base,
    Head,
    Time,
    Number,
    Size,
}

pub const KEYS: &[KeyInfo] = &[
    key("turn", "whose turn it is", ValueKind::Enum),
    key("repo", "repository, globs like acme/*", ValueKind::Repo),
    key("org", "repository owner", ValueKind::Org),
    key("author", "PR author, or @me", ValueKind::Author),
    key(
        "reviewer",
        "requested reviewer, or @me",
        ValueKind::Reviewer,
    ),
    key("team", "requested team", ValueKind::Team),
    key("label", "label name", ValueKind::Label),
    key("ci", "CI status", ValueKind::Enum),
    key("review", "review decision", ValueKind::Enum),
    key("mine", "your own review", ValueKind::Enum),
    key("last", "who acted last", ValueKind::Enum),
    key(
        "waiting",
        "time in its current turn (>2d, <3h, 1d..1w)",
        ValueKind::Time,
    ),
    key(
        "updated",
        "time since last update, or dates",
        ValueKind::Time,
    ),
    key("age", "time since it was opened, or dates", ValueKind::Time),
    key(
        "size",
        "lines changed (<100) or xs/s/m/l/xl",
        ValueKind::Size,
    ),
    key("files", "files changed", ValueKind::Number),
    key("threads", "unresolved review threads", ValueKind::Number),
    key("base", "target branch", ValueKind::Base),
    key("head", "source branch", ValueKind::Head),
];

const fn key(name: &'static str, description: &'static str, values: ValueKind) -> KeyInfo {
    KeyInfo {
        name,
        description,
        values,
    }
}

/// Bare words with a special meaning.
pub const FLAGS: &[(&str, &str)] = &[
    ("draft", "draft PRs"),
    ("conflict", "PRs with merge conflicts"),
    ("bot", "PRs opened by bots"),
    ("mentioned", "PRs that mention you"),
    ("new", "activity since you last looked"),
    ("snoozed", "snoozed PRs (hidden otherwise)"),
    ("muted", "muted PRs (hidden otherwise)"),
];

/// Names and descriptions of a [`ValueKind::Enum`] key's values.
pub fn enum_values(key: &str) -> Option<Vec<(&'static str, &'static str)>> {
    fn names<T>(table: &[(&'static str, T, &'static str)]) -> Vec<(&'static str, &'static str)> {
        table.iter().map(|(name, _, desc)| (*name, *desc)).collect()
    }
    Some(match key {
        "turn" => names(TURNS),
        "last" => names(LAST),
        "ci" => names(CI),
        "review" => names(REVIEW),
        "mine" => names(MINE),
        _ => return None,
    })
}

pub fn parse(text: &str, tz: &TimeZone) -> Result<Filter, String> {
    let terms = tokenize(text)?
        .into_iter()
        .map(|token| term(&token, tz))
        .collect::<Result<_, _>>()?;
    Ok(Filter { terms })
}

/// Split on whitespace, keeping quoted sections (`label:"needs review"`) together.
fn tokenize(text: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in text.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if in_quotes {
        return Err("unclosed quote".to_owned());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn term(token: &str, tz: &TimeZone) -> Result<Term, String> {
    let (negated, body) = match token.strip_prefix('-') {
        Some(rest) if !rest.is_empty() => (true, rest),
        _ => (false, token),
    };

    let pred = match body.split_once(':') {
        Some((key, value)) if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphabetic()) => {
            key_value(&key.to_ascii_lowercase(), value, tz)?
        }
        _ => match body.to_ascii_lowercase().as_str() {
            "draft" => Pred::Draft,
            "conflict" => Pred::Conflict,
            "bot" => Pred::Bot,
            "mentioned" => Pred::Mentioned,
            "new" => Pred::New,
            "snoozed" => Pred::Snoozed,
            "muted" => Pred::Muted,
            _ => Pred::Text(body.to_lowercase()),
        },
    };
    Ok(Term { negated, pred })
}

fn key_value(key: &str, value: &str, tz: &TimeZone) -> Result<Pred, String> {
    if value.is_empty() {
        return Err(format!("{key}: needs a value"));
    }
    let values = || value.split(',').filter(|v| !v.is_empty());
    let globs = || values().map(Glob::new).collect();
    let users = || values().map(user).collect();

    Ok(match key {
        "repo" => Pred::Repo(globs()),
        "org" => Pred::Org(globs()),
        "author" => Pred::Author(users()),
        "reviewer" => Pred::Reviewer(users()),
        "team" => Pred::Team(globs()),
        "label" => Pred::Label(globs()),
        "base" => Pred::Base(globs()),
        "head" => Pred::Head(globs()),
        "turn" => Pred::Turn(one_of(key, values(), TURNS)?),
        "last" => Pred::Last(one_of(key, values(), LAST)?),
        "ci" => Pred::Ci(one_of(key, values(), CI)?),
        "review" => Pred::Review(one_of(key, values(), REVIEW)?),
        "mine" => Pred::Mine(one_of(key, values(), MINE)?),
        "age" => Pred::Age(
            values()
                .map(|v| time_cond(key, v, tz))
                .collect::<Result<_, _>>()?,
        ),
        "updated" => Pred::Updated(
            values()
                .map(|v| time_cond(key, v, tz))
                .collect::<Result<_, _>>()?,
        ),
        "waiting" => Pred::Waiting(
            values()
                .map(|v| time_cond(key, v, tz))
                .collect::<Result<_, _>>()?,
        ),
        "size" => Pred::Size(
            values()
                .map(|v| match size_class(v) {
                    Some(class) => Ok(class.range()),
                    None => cmp(key, v, number),
                })
                .collect::<Result<_, _>>()?,
        ),
        "files" => Pred::Files(
            values()
                .map(|v| cmp(key, v, number))
                .collect::<Result<_, _>>()?,
        ),
        "threads" => Pred::Threads(
            values()
                .map(|v| cmp(key, v, number))
                .collect::<Result<_, _>>()?,
        ),
        _ => {
            return Err(format!(
                "unknown filter {key:?} (keys: {}; flags: {})",
                KEYS.iter().map(|k| k.name).collect::<Vec<_>>().join(", "),
                FLAGS.iter().map(|(f, _)| *f).collect::<Vec<_>>().join(", ")
            ));
        }
    })
}

const TURNS: &[(&str, Turn, &str)] = &[
    ("me", Turn::Me, "you need to act"),
    ("team", Turn::Team, "requested from your team"),
    ("merge", Turn::Merge, "approved and green"),
    ("author", Turn::Author, "waiting on the author"),
    ("reviewers", Turn::Reviewers, "waiting on reviewers"),
    ("blocked", Turn::Blocked, "draft, failing, or conflicting"),
];
const LAST: &[(&str, LastRole, &str)] = &[
    ("me", LastRole::Me, "you acted last"),
    ("author", LastRole::Author, "the author acted last"),
    ("reviewer", LastRole::Reviewer, "a reviewer acted last"),
    ("none", LastRole::None, "no activity yet"),
];
const CI: &[(&str, CiState, &str)] = &[
    ("pass", CiState::Pass, "all checks passed"),
    ("fail", CiState::Fail, "a check failed"),
    ("pending", CiState::Pending, "checks still running"),
    ("none", CiState::None, "no checks"),
];
const REVIEW: &[(&str, Option<ReviewDecision>, &str)] = &[
    ("approved", Some(ReviewDecision::Approved), "approved"),
    (
        "changes",
        Some(ReviewDecision::ChangesRequested),
        "changes requested",
    ),
    (
        "required",
        Some(ReviewDecision::ReviewRequired),
        "review required",
    ),
    ("none", None, "no review policy"),
];
const MINE: &[(&str, MineState, &str)] = &[
    ("unreviewed", MineState::Unreviewed, "you haven't reviewed"),
    ("stale", MineState::Stale, "new commits since your review"),
    ("approved", MineState::Approved, "you approved"),
    ("changes", MineState::Changes, "you requested changes"),
    ("commented", MineState::Commented, "you commented"),
];

fn one_of<'a, T: Copy>(
    key: &str,
    values: impl Iterator<Item = &'a str>,
    options: &[(&str, T, &str)],
) -> Result<Vec<T>, String> {
    values
        .map(|v| {
            options
                .iter()
                .find(|(name, _, _)| name.eq_ignore_ascii_case(v))
                .map(|(_, value, _)| *value)
                .ok_or_else(|| {
                    let names: Vec<&str> = options.iter().map(|(n, _, _)| *n).collect();
                    format!("{key}: unknown value {v:?} (expected {})", names.join(", "))
                })
        })
        .collect()
}

fn user(value: &str) -> User {
    if value.eq_ignore_ascii_case("@me") {
        User::Me
    } else {
        User::Login(value.trim_start_matches('@').to_owned())
    }
}

fn size_class(value: &str) -> Option<SizeClass> {
    Some(match value.to_ascii_lowercase().as_str() {
        "xs" => SizeClass::Xs,
        "s" => SizeClass::S,
        "m" => SizeClass::M,
        "l" => SizeClass::L,
        "xl" => SizeClass::Xl,
        _ => return None,
    })
}

/// `>5`, `>=5`, `<5`, `<=5`, `5`, or `5..10` (inclusive).
fn cmp<T>(key: &str, value: &str, parse: impl Fn(&str) -> Option<T>) -> Result<Cmp<T>, String> {
    let bad = || format!("{key}: can't understand {value:?}");
    let parsed = if let Some((lo, hi)) = value.split_once("..") {
        Cmp::Range(parse(lo).ok_or_else(bad)?, parse(hi).ok_or_else(bad)?)
    } else if let Some(v) = value.strip_prefix(">=") {
        Cmp::Ge(parse(v).ok_or_else(bad)?)
    } else if let Some(v) = value.strip_prefix("<=") {
        Cmp::Le(parse(v).ok_or_else(bad)?)
    } else if let Some(v) = value.strip_prefix('>') {
        Cmp::Gt(parse(v).ok_or_else(bad)?)
    } else if let Some(v) = value.strip_prefix('<') {
        Cmp::Lt(parse(v).ok_or_else(bad)?)
    } else {
        Cmp::Eq(parse(value).ok_or_else(bad)?)
    };
    Ok(parsed)
}

fn number(value: &str) -> Option<u32> {
    value.parse().ok()
}

/// A duration ago (`>2d`) or calendar dates (`2026-09-01..2026-09-20`).
fn time_cond(key: &str, value: &str, tz: &TimeZone) -> Result<TimeCond, String> {
    let looks_like_date = value.trim_start_matches(['<', '>', '=']).len() >= 10
        && value.trim_start_matches(['<', '>', '=']).as_bytes()[4] == b'-';
    if looks_like_date {
        let dates = cmp(key, value, |v| v.parse::<Date>().ok())?;
        let start = |d: Date| -> Result<Timestamp, String> {
            tz.to_timestamp(d.to_datetime(jiff::civil::Time::midnight()))
                .map_err(|e| format!("{key}: {e}"))
        };
        let next = |d: Date| -> Result<Timestamp, String> {
            start(d.tomorrow().map_err(|e| format!("{key}: {e}"))?)
        };
        // Each date covers its whole day, so ranges are half-open [from, until).
        let (from, until) = match dates {
            Cmp::Eq(d) => (Some(start(d)?), Some(next(d)?)),
            Cmp::Gt(d) => (Some(next(d)?), None),
            Cmp::Ge(d) => (Some(start(d)?), None),
            Cmp::Lt(d) => (None, Some(start(d)?)),
            Cmp::Le(d) => (None, Some(next(d)?)),
            Cmp::Range(a, b) => (Some(start(a)?), Some(next(b)?)),
        };
        return Ok(TimeCond::Between { from, until });
    }

    let ago = cmp(key, value, duration_secs)?;
    if let Cmp::Eq(_) = ago {
        return Err(format!(
            "{key}: use >, <, or a range with a duration (e.g. {key}:>{value})"
        ));
    }
    Ok(TimeCond::Ago(ago))
}

/// `30m`, `2h`, `3d`, `2w`, `1y`, in seconds.
fn duration_secs(value: &str) -> Option<i64> {
    let split = value.find(|c: char| !c.is_ascii_digit())?;
    let (n, unit) = value.split_at(split);
    let n: i64 = n.parse().ok()?;
    let unit = match unit {
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        "y" => 365 * 86_400,
        _ => return None,
    };
    Some(n * unit)
}
