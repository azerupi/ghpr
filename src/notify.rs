//! Desktop notifications for what changed between refreshes: a PR became
//! your turn, left the merge queue, merged, ...
//!
//! [`Notifier`] decides (no I/O, so it's testable); [`send`] shows a notice.
//! Each notice is sent once per session: what was announced is remembered by
//! PR, event, and when the PR landed in its current state, so a turn that
//! flickers (GitHub computes merge state lazily) or a PR that drops out of
//! a search and comes back doesn't notify again, while a new review request
//! or reply does.

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};

use color_eyre::eyre::{Result, bail};
use serde::Deserialize;

use crate::filter::{Context, Filter};
use crate::model::{MergeState, Mergeable, Pr, QueueEntry, QueueState, Reviewer, Turn};

/// What can notify, as named in the config's `[notify] events`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// A PR became your turn: a review request, a reply, a mention, CI
    /// breaking on your PR, ...
    MyTurn,
    /// A review was requested from one of your teams.
    Team,
    /// One of your PRs left the merge queue: merged, or removed.
    Queue,
    /// One of your PRs is approved and ready to merge.
    ReadyToMerge,
    /// One of your PRs has merge conflicts.
    Conflict,
    /// One of your PRs was merged or closed.
    Merged,
    /// A PR showed up in the view.
    Opened,
}

/// On unless the config says otherwise: what nearly always needs you.
pub const DEFAULT_EVENTS: &[Event] = &[Event::MyTurn, Event::Queue];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub url: String,
    pub urgent: bool,
}

/// More notices than this from one refresh become a single summary.
const BATCH: usize = 3;

/// What's remembered about a PR from the last refresh, for when it's gone
/// from the next one.
#[derive(Debug, Clone)]
struct Last {
    label: String,
    title: String,
    url: String,
    mine: bool,
    queue: Option<QueueEntry>,
    /// Passed the view's filter (so not snoozed or muted).
    shown: bool,
    /// Still in the view, just no longer queued.
    listed: bool,
}

/// A PR that disappeared from the view and is worth asking about: was it
/// merged, closed, or does it just no longer match?
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub id: String,
}

pub struct Notifier {
    events: Vec<Event>,
    /// The view's own filter (not whatever is typed on screen).
    filter: Filter,
    sent: HashSet<(String, Event, i64)>,
    last: HashMap<String, Last>,
    /// Gone PRs being looked up.
    pending: HashMap<String, Last>,
    /// The first refresh only records what's there.
    primed: bool,
}

impl Notifier {
    pub fn new(events: Vec<Event>, filter: Filter) -> Self {
        Notifier {
            events,
            filter,
            sent: HashSet::new(),
            last: HashMap::new(),
            pending: HashMap::new(),
            primed: false,
        }
    }

    fn wants(&self, event: Event) -> bool {
        self.events.contains(&event)
    }

    /// Take in a refresh of the view. Returns what to announce, and the PRs
    /// that disappeared and are worth looking up.
    pub fn refresh(&mut self, prs: &[Pr], ctx: &Context) -> (Vec<Notice>, Vec<Lookup>) {
        let mut notices = Vec::new();
        let mut gone_lookups = Vec::new();
        let mut present = HashSet::new();
        for pr in prs {
            present.insert(pr.id.clone());
            let previous = self.last.get(&pr.id).cloned();
            let mine = pr.author.as_deref() == Some(ctx.viewer);
            // Conflicts come and go; a clean PR may conflict again later.
            if pr.mergeable == Mergeable::Clean && pr.merge_state != MergeState::Conflict {
                self.sent.remove(&(pr.id.clone(), Event::Conflict, 0));
            }
            let shown = self.filter.shows(pr, ctx);
            self.last.insert(
                pr.id.clone(),
                Last {
                    label: label(pr),
                    title: pr.title.clone(),
                    url: pr.url.clone(),
                    mine,
                    queue: pr.queue,
                    shown,
                    listed: false,
                },
            );
            if !shown {
                continue;
            }
            // Every applicable key is recorded, but each PR gets at most one
            // notice per refresh: the first one here.
            let mut candidates: Vec<((Event, i64), Notice)> = Vec::new();
            if let Some(entry) = previous.as_ref().and_then(|p| p.queue)
                && mine
            {
                let key = (Event::Queue, entry.enqueued_at.as_second());
                match pr.queue {
                    // Out of the queue but still listed: maybe removed, but
                    // search lags behind, so it may well have just merged.
                    None if self.primed && self.wants(Event::Queue) => {
                        if !self.pending.contains_key(&pr.id) {
                            let mut last = previous.clone().expect("checked above");
                            last.listed = true;
                            self.pending.insert(pr.id.clone(), last);
                            gone_lookups.push(Lookup { id: pr.id.clone() });
                        }
                    }
                    None => {}
                    Some(now) if now.state == QueueState::Unmergeable => candidates.push((
                        key,
                        notice(
                            "Leaving the merge queue",
                            pr,
                            "its checks failed or it conflicts",
                            true,
                        ),
                    )),
                    Some(_) => {}
                }
            }
            let since = pr.waiting_since.as_second();
            match pr.turn {
                Turn::Me => candidates.push((
                    (Event::MyTurn, since),
                    notice("Your turn", pr, &reason(pr, ctx.viewer), false),
                )),
                Turn::Team => {
                    let team = pr.requested_reviewers.iter().find_map(|r| match r {
                        Reviewer::Team(team) => Some(team.as_str()),
                        Reviewer::User(_) => None,
                    });
                    let why = team.map_or(String::new(), |t| format!("requested from {t}"));
                    candidates.push(((Event::Team, since), notice("Team review", pr, &why, false)));
                }
                Turn::Merge if mine => candidates.push((
                    (Event::ReadyToMerge, since),
                    notice("Ready to merge", pr, "approved and green", false),
                )),
                _ => {}
            }
            if mine
                && (pr.mergeable == Mergeable::Conflicting
                    || pr.merge_state == MergeState::Conflict)
            {
                let why = format!("conflicts with {}", pr.base_ref);
                candidates.push((
                    (Event::Conflict, 0),
                    notice("Merge conflict", pr, &why, false),
                ));
            }
            let by = format!("by {}", pr.author.as_deref().unwrap_or("ghost"));
            candidates.push(((Event::Opened, 0), notice("New PR", pr, &by, false)));

            let mut chosen = None;
            for ((event, at), candidate) in candidates {
                let fresh = self.sent.insert((pr.id.clone(), event, at));
                if fresh && self.primed && self.wants(event) && chosen.is_none() {
                    chosen = Some(candidate);
                }
            }
            notices.extend(chosen);
        }

        let mut lookups = gone_lookups;
        let gone: Vec<String> = self
            .last
            .keys()
            .filter(|id| !present.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            let last = self.last.remove(&id).expect("listed above");
            let worth = last.mine
                && last.shown
                && (self.wants(Event::Merged)
                    || (last.queue.is_some() && self.wants(Event::Queue)));
            if self.primed && worth && !self.pending.contains_key(&id) {
                lookups.push(Lookup { id: id.clone() });
                self.pending.insert(id, last);
            }
        }
        self.primed = true;
        (batch(notices), lookups)
    }

    /// What a gone PR's lookup found: `OPEN`, `MERGED`, or `CLOSED`. Only
    /// PRs whose event is wanted are looked up, so any result is news.
    pub fn resolved(&mut self, id: &str, state: Result<&str, &str>) -> Option<Notice> {
        let last = self.pending.remove(id)?;
        let ended = (Event::Merged, 0);
        let (title, urgent, (event, at)) = match (state.ok()?, last.queue) {
            ("MERGED", Some(_)) => ("Merged from the queue", false, ended),
            ("MERGED", None) => ("Merged", false, ended),
            ("CLOSED", _) => ("Closed", false, ended),
            // Keyed like "leaving the merge queue", so it's said only once.
            ("OPEN", Some(entry)) if last.listed => (
                "Removed from the merge queue",
                true,
                (Event::Queue, entry.enqueued_at.as_second()),
            ),
            // Still open, just no longer in the view.
            _ => return None,
        };
        if !self.sent.insert((id.to_owned(), event, at)) {
            return None;
        }
        Some(Notice {
            title: format!("{title} · {}", last.label),
            body: last.title,
            url: last.url,
            urgent,
        })
    }
}

fn label(pr: &Pr) -> String {
    format!("{}#{}", pr.repo, pr.number)
}

fn notice(what: &str, pr: &Pr, why: &str, urgent: bool) -> Notice {
    let body = if why.is_empty() {
        pr.title.clone()
    } else {
        format!("{} — {why}", pr.title)
    };
    Notice {
        title: format!("{what} · {}", label(pr)),
        body,
        url: pr.url.clone(),
        urgent,
    }
}

/// Why a PR is your turn, in a few words.
fn reason(pr: &Pr, viewer: &str) -> String {
    let mine = pr.author.as_deref() == Some(viewer);
    if mine {
        if pr.queue.is_some_and(|q| q.state == QueueState::Unmergeable) {
            return "it's leaving the merge queue".to_owned();
        }
        if pr.mergeable == Mergeable::Conflicting || pr.merge_state == MergeState::Conflict {
            return format!("conflicts with {}", pr.base_ref);
        }
        if pr.ci == crate::model::CiState::Fail {
            return "CI is failing".to_owned();
        }
        if pr.merge_state == MergeState::Behind {
            return format!("behind {}", pr.base_ref);
        }
    } else if pr.my_review.is_none()
        && pr
            .requested_reviewers
            .iter()
            .any(|r| matches!(r, Reviewer::User(login) if login.eq_ignore_ascii_case(viewer)))
    {
        let by = pr.author.as_deref().unwrap_or("ghost");
        return format!("review requested by {by}");
    }
    match &pr.last_activity {
        Some(a) => format!("{} {}", a.who, a.kind.verb()),
        None => String::new(),
    }
}

/// Many notices at once become one, listing the first few.
pub fn batch(notices: Vec<Notice>) -> Vec<Notice> {
    if notices.len() <= BATCH {
        return notices;
    }
    let mut lines: Vec<String> = notices
        .iter()
        .take(BATCH)
        .map(|n| n.title.clone())
        .collect();
    lines.push(format!("…and {} more", notices.len() - BATCH));
    vec![Notice {
        title: format!("{} PR updates", notices.len()),
        body: lines.join("\n"),
        url: String::new(),
        urgent: notices.iter().any(|n| n.urgent),
    }]
}

/// Show a notice on the desktop. Blocks until the notifier exits; its output
/// goes nowhere, so it can't scribble over the UI.
pub fn send(notice: &Notice) -> Result<()> {
    let body = if notice.url.is_empty() {
        notice.body.clone()
    } else {
        format!("{}\n{}", notice.body, notice.url)
    };
    let mut command = if cfg!(target_os = "macos") {
        // Title and body as arguments, so quotes in them can't break the script.
        let mut c = Command::new("osascript");
        c.args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
            &notice.title,
            &body,
        ]);
        c
    } else {
        let mut c = Command::new("notify-send");
        c.args([
            "--app-name=ghpr",
            if notice.urgent {
                "--urgency=critical"
            } else {
                "--urgency=normal"
            },
            &notice.title,
            &body,
        ]);
        c
    };
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => bail!("the notifier exited with {status}"),
        Err(e) => bail!("couldn't run the notifier ({e}); install notify-send"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CiState, Role};
    use crate::state::{LocalState, Snooze};
    use crate::test_support::{now, pr};

    fn ctx(state: &LocalState) -> Context<'_> {
        Context {
            viewer: "me",
            now: now(),
            local: Some(state),
        }
    }

    fn notifier(events: &[Event]) -> Notifier {
        Notifier::new(events.to_vec(), Filter::default())
    }

    fn titles(notices: &[Notice]) -> Vec<&str> {
        notices.iter().map(|n| n.title.as_str()).collect()
    }

    /// Someone else's PR that isn't my turn yet.
    fn theirs(number: u64) -> Pr {
        let mut p = pr(number);
        p.turn = Turn::Reviewers;
        p
    }

    fn mine(number: u64) -> Pr {
        let mut p = pr(number);
        p.author = Some("me".into());
        p.turn = Turn::Reviewers;
        p
    }

    #[test]
    fn the_first_refresh_only_records() {
        let state = LocalState::new(now());
        let mut n = notifier(&[Event::MyTurn, Event::Opened]);
        let (notices, _) = n.refresh(&[pr(1)], &ctx(&state));
        assert!(notices.is_empty(), "already your turn when ghpr started");
        let (notices, _) = n.refresh(&[pr(1)], &ctx(&state));
        assert!(notices.is_empty(), "nothing changed");
    }

    #[test]
    fn becoming_your_turn_notifies_once() {
        let state = LocalState::new(now());
        let mut n = notifier(&[Event::MyTurn]);
        n.refresh(&[theirs(1)], &ctx(&state));
        let mut p = pr(1);
        p.requested_reviewers = vec![Reviewer::User("me".into())];
        let (notices, _) = n.refresh(std::slice::from_ref(&p), &ctx(&state));
        assert_eq!(titles(&notices), ["Your turn · acme/api#1"]);
        assert_eq!(
            notices[0].body,
            "Change number 1 — review requested by alice"
        );

        // A flicker (merge state computed lazily) and back: same waiting_since.
        let mut flicker = p.clone();
        flicker.turn = Turn::Merge;
        n.refresh(&[flicker], &ctx(&state));
        let (notices, _) = n.refresh(std::slice::from_ref(&p), &ctx(&state));
        assert!(notices.is_empty(), "announced already");

        // Gone past the search limit and back, same: nothing new.
        n.refresh(&[], &ctx(&state));
        assert!(
            n.refresh(std::slice::from_ref(&p), &ctx(&state))
                .0
                .is_empty()
        );

        // A new reply restarts the turn: that's news.
        p.waiting_since = now();
        let (notices, _) = n.refresh(&[p], &ctx(&state));
        assert_eq!(notices.len(), 1);
    }

    #[test]
    fn new_prs_that_need_you_notify() {
        let state = LocalState::new(now());
        let mut n = notifier(&[Event::MyTurn, Event::Opened]);
        n.refresh(&[], &ctx(&state));
        let (notices, _) = n.refresh(&[pr(1), theirs(2)], &ctx(&state));
        assert_eq!(
            titles(&notices),
            ["Your turn · acme/api#1", "New PR · acme/api#2"],
            "one notice per PR: your turn wins over new"
        );
    }

    #[test]
    fn snoozed_and_muted_prs_stay_quiet() {
        let mut state = LocalState::new(now());
        state.set_muted("PR_1", true);
        state.snooze(
            "PR_2",
            Snooze::Until(now() + jiff::SignedDuration::from_hours(24)),
        );
        let mut n = notifier(&[Event::MyTurn]);
        n.refresh(&[], &ctx(&state));
        let (notices, _) = n.refresh(&[pr(1), pr(2), pr(3)], &ctx(&state));
        assert_eq!(titles(&notices), ["Your turn · acme/api#3"]);
    }

    #[test]
    fn the_views_own_filter_decides() {
        let state = LocalState::new(now());
        let mut n = Notifier::new(vec![Event::MyTurn], Filter::parse("repo:acme/web").unwrap());
        n.refresh(&[], &ctx(&state));
        assert!(
            n.refresh(&[pr(1)], &ctx(&state)).0.is_empty(),
            "acme/api is filtered out"
        );
    }

    #[test]
    fn queue_exits_notify_and_merges_are_looked_up() {
        let state = LocalState::new(now());
        let mut n = notifier(&[Event::Queue]);
        let entry = QueueEntry {
            position: 1,
            state: QueueState::AwaitingChecks,
            enqueued_at: now(),
            eta_secs: None,
        };
        let mut queued = mine(1);
        queued.queue = Some(entry);
        let mut also = mine(2);
        also.queue = Some(entry);
        n.refresh(&[queued.clone(), also.clone()], &ctx(&state));

        // #1 is about to be ejected, #2 is gone from the view.
        queued.queue = Some(QueueEntry {
            state: QueueState::Unmergeable,
            ..entry
        });
        let (notices, lookups) = n.refresh(std::slice::from_ref(&queued), &ctx(&state));
        assert_eq!(titles(&notices), ["Leaving the merge queue · acme/api#1"]);
        assert_eq!(lookups, [Lookup { id: "PR_2".into() }]);
        let merged = n.resolved("PR_2", Ok("MERGED")).unwrap();
        assert_eq!(merged.title, "Merged from the queue · acme/api#2");
        assert!(n.resolved("PR_2", Ok("MERGED")).is_none(), "once");

        // Then out of the queue but still listed: looked up, not guessed,
        // and as "leaving" was said already, "removed" isn't said again.
        queued.queue = None;
        let (notices, lookups) = n.refresh(&[queued], &ctx(&state));
        assert!(notices.is_empty());
        assert_eq!(lookups, [Lookup { id: "PR_1".into() }]);
        assert_eq!(n.resolved("PR_1", Ok("OPEN")), None);
    }

    #[test]
    fn leaving_the_queue_is_looked_up_not_guessed() {
        let state = LocalState::new(now());
        let entry = QueueEntry {
            position: 1,
            state: QueueState::Mergeable,
            enqueued_at: now(),
            eta_secs: Some(60),
        };
        for (found, title) in [
            ("MERGED", "Merged from the queue · acme/api#1"),
            ("OPEN", "Removed from the merge queue · acme/api#1"),
        ] {
            let mut n = notifier(&[Event::Queue]);
            let mut p = mine(1);
            p.queue = Some(entry);
            n.refresh(std::slice::from_ref(&p), &ctx(&state));
            // Search lags: a just-merged PR can still be listed as open.
            p.queue = None;
            let (notices, lookups) = n.refresh(std::slice::from_ref(&p), &ctx(&state));
            assert!(notices.is_empty(), "no guess");
            assert_eq!(lookups.len(), 1);
            let notice = n.resolved("PR_1", Ok(found)).unwrap();
            assert_eq!(notice.title, title);
            assert_eq!(notice.urgent, found == "OPEN");
            let (notices, lookups) = n.refresh(&[], &ctx(&state));
            assert!(notices.is_empty() && lookups.is_empty(), "nothing more");
        }
    }

    #[test]
    fn gone_prs_still_open_or_unwanted_are_quiet() {
        let state = LocalState::new(now());
        let mut n = notifier(&[Event::Merged]);
        n.refresh(&[mine(1), theirs(2)], &ctx(&state));
        let (_, lookups) = n.refresh(&[], &ctx(&state));
        assert_eq!(lookups, [Lookup { id: "PR_1".into() }], "only your own PRs");
        assert!(n.resolved("PR_1", Ok("OPEN")).is_none());
        assert!(
            n.resolved("PR_1", Ok("MERGED")).is_none(),
            "no longer pending"
        );

        let mut n = notifier(&[Event::MyTurn]);
        n.refresh(&[mine(1)], &ctx(&state));
        assert!(
            n.refresh(&[], &ctx(&state)).1.is_empty(),
            "merges not wanted"
        );
    }

    #[test]
    fn your_prs_turning_ready_or_conflicting() {
        let state = LocalState::new(now());
        let mut n = notifier(&[Event::ReadyToMerge, Event::Conflict]);
        n.refresh(&[mine(1), mine(2)], &ctx(&state));
        let mut ready = mine(1);
        ready.turn = Turn::Merge;
        let mut conflicting = mine(2);
        conflicting.mergeable = Mergeable::Conflicting;
        let (notices, _) = n.refresh(&[ready, conflicting.clone()], &ctx(&state));
        assert_eq!(
            titles(&notices),
            ["Ready to merge · acme/api#1", "Merge conflict · acme/api#2"]
        );
        // Resolved, then conflicting again later: that's news again.
        n.refresh(&[mine(2)], &ctx(&state));
        assert_eq!(n.refresh(&[conflicting], &ctx(&state)).0.len(), 1);
    }

    #[test]
    fn reasons_say_why_its_your_turn() {
        let mut p = mine(1);
        p.ci = CiState::Fail;
        assert_eq!(reason(&p, "me"), "CI is failing");
        let mut p = pr(2);
        p.last_activity.as_mut().unwrap().role = Role::Author;
        assert_eq!(reason(&p, "me"), "alice pushed a commit");
    }

    #[test]
    fn many_notices_become_one() {
        let one = |i: u64| notice("Your turn", &pr(i), "", i == 2);
        assert_eq!(batch(vec![one(1), one(2)]).len(), 2);
        let all = batch((1..=5).map(one).collect());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "5 PR updates");
        assert!(all[0].body.ends_with("…and 2 more"));
        assert!(all[0].urgent);
    }
}
