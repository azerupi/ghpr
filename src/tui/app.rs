//! UI state and how it reacts to input. No I/O happens here: `update` returns
//! [`Cmd`]s for the event loop to carry out, which keeps it easy to test.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use jiff::{SignedDuration, Timestamp};
use ratatui::widgets::TableState;

use std::collections::BTreeSet;

use super::facets;
use super::input::LineInput;
use crate::columns::Column;
use crate::filter::complete::{self, Completion};
use crate::filter::{self, Filter, toggle_term};
use crate::model::{Pr, ScoreWeights, Turn};
use crate::sort::SortSpec;
use crate::state::{Flags, LocalState, Snooze};

/// How long a transient status message stays visible.
const FLASH_FOR: SignedDuration = SignedDuration::from_secs(4);

/// One-key filter toggles: key, filter term, description.
pub const TOGGLES: &[(char, &str, &str)] = &[
    ('m', "turn:me", "only my turn"),
    ('n', "new", "only new"),
    ('d', "-draft", "hide drafts"),
    ('b', "-bot", "hide bots' PRs"),
    ('c', "-ci:fail", "hide failing CI"),
    ('Z', "snoozed", "only snoozed"),
];

/// Snooze choices: key, label, duration (`None`: until someone acts on the PR).
pub const SNOOZES: &[(char, &str, Option<SignedDuration>)] = &[
    ('d', "1 day", Some(SignedDuration::from_hours(24))),
    ('3', "3 days", Some(SignedDuration::from_hours(3 * 24))),
    ('w', "1 week", Some(SignedDuration::from_hours(7 * 24))),
    ('u', "until there's new activity", None),
];

/// How the app starts out; mostly from the config and command line.
pub struct Settings {
    pub tabs: Vec<TabSettings>,
    /// Index into `tabs` to start on.
    pub active: usize,
    pub refresh_interval: SignedDuration,
    pub offline: bool,
    pub weights: ScoreWeights,
    pub state: LocalState,
    pub columns: Vec<Column>,
}

pub struct TabSettings {
    pub view_name: String,
    /// Must be valid; it's parsed again here.
    pub filter: String,
    pub sort: SortSpec,
}

/// One view: its data, and how it's filtered, sorted, and scrolled.
pub struct Tab {
    pub view_name: String,
    /// Everything fetched, before filtering.
    all: Vec<Pr>,
    /// What's shown: filtered and sorted.
    pub prs: Vec<Pr>,
    pub table: TableState,
    pub filter_text: String,
    filter: Filter,
    pub sort: SortSpec,
    /// When the data was fetched from GitHub.
    pub fetched_at: Option<Timestamp>,
    /// The data came from the cache and hasn't been refreshed yet.
    pub from_cache: bool,
    pub refreshing: bool,
    pub truncated: bool,
    pub error: Option<String>,
    /// PR ids marked with `v`; actions apply to all of them.
    pub marked: BTreeSet<String>,
    last_attempt: Option<Timestamp>,
}

impl Tab {
    fn new(settings: TabSettings) -> Self {
        Tab {
            filter: Filter::parse(&settings.filter).unwrap_or_default(),
            view_name: settings.view_name,
            all: Vec::new(),
            prs: Vec::new(),
            table: TableState::default(),
            filter_text: settings.filter,
            sort: settings.sort,
            fetched_at: None,
            from_cache: false,
            refreshing: false,
            truncated: false,
            error: None,
            marked: BTreeSet::new(),
            last_attempt: None,
        }
    }

    pub fn selected(&self) -> Option<&Pr> {
        self.table.selected().and_then(|i| self.prs.get(i))
    }

    pub fn total(&self) -> usize {
        self.all.len()
    }

    fn select(&mut self, index: usize) {
        if self.prs.is_empty() {
            self.table.select(None);
        } else {
            self.table.select(Some(index.min(self.prs.len() - 1)));
        }
    }

    fn move_by(&mut self, delta: isize) {
        let current = self.table.selected().unwrap_or(0) as isize;
        self.select((current + delta).max(0) as usize);
    }
}

pub struct App {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub viewer: String,
    pub weights: ScoreWeights,
    pub state: LocalState,
    /// Table columns, in order; the number keys sort by them.
    pub columns: Vec<Column>,
    pub prompt: Option<Prompt>,
    /// The snooze choices are showing.
    pub snoozing: bool,
    /// The facets panel is open, with this row selected.
    pub facets: Option<usize>,
    pub show_detail: bool,
    pub show_help: bool,
    pub now: Timestamp,
    pub flash: Option<(String, Timestamp)>,
    pub offline: bool,
    /// Rows visible in the table, as of the last render. Used for paging.
    pub page_rows: usize,
    pub quit: bool,
    refresh_interval: SignedDuration,
}

/// An open text prompt at the bottom of the screen.
pub struct Prompt {
    pub kind: PromptKind,
    pub input: LineInput,
    /// Why the current text can't be applied, if it can't.
    pub error: Option<String>,
    /// Suggestions for the word under the cursor.
    pub completion: Option<Completion>,
    /// The highlighted suggestion, while the menu is open (Tab opens it).
    pub menu: Option<usize>,
    /// The value before editing, restored by Esc.
    original: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Filter,
    Sort,
}

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Tick(Timestamp),
    Fetched {
        tab: usize,
        result: Result<FetchedData, String>,
    },
    /// The outcome of a [`Cmd`], shown briefly in the status bar.
    Flash(String),
}

#[derive(Debug)]
pub struct FetchedData {
    pub prs: Vec<Pr>,
    pub viewer: String,
    pub truncated: bool,
    pub at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// Refresh the tab with this index.
    Fetch(usize),
    Open(String),
    Copy(String),
    SaveState(LocalState),
}

impl App {
    pub fn new(settings: Settings, now: Timestamp) -> Self {
        let tabs: Vec<Tab> = settings.tabs.into_iter().map(Tab::new).collect();
        App {
            active: settings.active.min(tabs.len().saturating_sub(1)),
            tabs,
            viewer: String::new(),
            weights: settings.weights,
            state: settings.state,
            columns: settings.columns,
            prompt: None,
            snoozing: false,
            facets: None,
            show_detail: true,
            show_help: false,
            now,
            flash: None,
            offline: settings.offline,
            page_rows: 10,
            quit: false,
            refresh_interval: settings.refresh_interval,
        }
    }

    /// Show cached data for a tab until its first fetch completes.
    pub fn with_cache(
        mut self,
        tab: usize,
        prs: Vec<Pr>,
        viewer: String,
        fetched_at: Timestamp,
    ) -> Self {
        if self.viewer.is_empty() {
            self.viewer = viewer;
        }
        let t = &mut self.tabs[tab];
        t.all = prs;
        t.fetched_at = Some(fetched_at);
        t.from_cache = true;
        self.recompute(tab);
        self
    }

    /// Commands to run at startup.
    pub fn init(&mut self) -> Vec<Cmd> {
        self.fetch(self.active)
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn selected(&self) -> Option<&Pr> {
        self.tab().selected()
    }

    pub fn flags(&self, pr: &Pr) -> Flags {
        self.state.flags(pr, self.now)
    }

    pub fn my_turn_count(&self) -> usize {
        self.tab()
            .prs
            .iter()
            .filter(|pr| pr.turn == Turn::Me)
            .count()
    }

    pub fn new_count(&self) -> usize {
        self.tab()
            .prs
            .iter()
            .filter(|pr| self.flags(pr).new)
            .count()
    }

    /// PRs matching the filter but hidden because they're snoozed or muted.
    pub fn hidden_count(&self) -> usize {
        let tab = self.tab();
        let ctx = self.context();
        tab.all
            .iter()
            .filter(|pr| tab.filter.matches(pr, &ctx) && !tab.filter.shows(pr, &ctx))
            .count()
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Cmd> {
        match msg {
            Msg::Key(key) if key.kind == KeyEventKind::Release => Vec::new(),
            Msg::Key(key) if self.prompt.is_some() => {
                self.prompt_key(key);
                Vec::new()
            }
            Msg::Key(key) if self.snoozing => self.snooze_key(key),
            Msg::Key(key) if self.facets.is_some() => self.facet_key(key),
            Msg::Key(key) => self.key(key),
            Msg::Tick(now) => {
                self.now = now;
                if self
                    .flash
                    .as_ref()
                    .is_some_and(|(_, at)| now.duration_since(*at) > FLASH_FOR)
                {
                    self.flash = None;
                }
                // Only the tab on screen refreshes in the background.
                if self.is_stale(self.active) {
                    self.fetch(self.active)
                } else {
                    Vec::new()
                }
            }
            Msg::Fetched { tab, result } => {
                let Some(t) = self.tabs.get_mut(tab) else {
                    return Vec::new();
                };
                t.refreshing = false;
                match result {
                    Ok(data) => {
                        self.state.touch(&data.prs, data.at);
                        t.all = data.prs;
                        t.fetched_at = Some(data.at);
                        t.from_cache = false;
                        t.truncated = data.truncated;
                        t.error = None;
                        self.viewer = data.viewer;
                        self.recompute(tab);
                        vec![Cmd::SaveState(self.state.clone())]
                    }
                    Err(error) => {
                        t.error = Some(error);
                        Vec::new()
                    }
                }
            }
            Msg::Flash(text) => {
                self.flash(text);
                Vec::new()
            }
        }
    }

    fn key(&mut self, key: KeyEvent) -> Vec<Cmd> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if self.show_help {
            // Any key closes the help, except that Ctrl-C still quits.
            self.show_help = false;
            self.quit = ctrl && key.code == KeyCode::Char('c');
            return Vec::new();
        }

        let half_page = (self.page_rows / 2).max(1) as isize;
        let page = self.page_rows.max(1) as isize;
        match key.code {
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('d') if ctrl => self.tab_mut().move_by(half_page),
            KeyCode::Char('u') if ctrl => self.tab_mut().move_by(-half_page),
            KeyCode::Char(c @ '1'..='9') if alt => {
                return self.switch_to(c as usize - '1' as usize);
            }
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc if !self.tab().marked.is_empty() => self.tab_mut().marked.clear(),
            KeyCode::Esc if !self.tab().filter_text.is_empty() => self.set_filter(String::new()),
            KeyCode::Char('j') | KeyCode::Down => self.tab_mut().move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.tab_mut().move_by(-1),
            KeyCode::PageDown => self.tab_mut().move_by(page),
            KeyCode::PageUp => self.tab_mut().move_by(-page),
            KeyCode::Char('g') | KeyCode::Home => self.tab_mut().select(0),
            KeyCode::Char('G') | KeyCode::End => {
                let last = self.tab().prs.len().saturating_sub(1);
                self.tab_mut().select(last);
            }
            KeyCode::Char(']') => {
                return self.switch_to((self.active + 1) % self.tabs.len());
            }
            KeyCode::Char('[') => {
                return self.switch_to((self.active + self.tabs.len() - 1) % self.tabs.len());
            }
            KeyCode::Enter | KeyCode::Tab => self.show_detail = !self.show_detail,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('/') => self.open_prompt(PromptKind::Filter),
            KeyCode::Char('s') => self.open_prompt(PromptKind::Sort),
            KeyCode::Char('S') => {
                let tab = self.tab_mut();
                tab.sort = tab.sort.reversed();
                self.recompute(self.active);
            }
            KeyCode::Char(c @ '1'..='9') => {
                let column = self.columns.get(c as usize - '1' as usize).copied();
                match column.and_then(Column::sort_key) {
                    Some(key) => {
                        let tab = self.tab_mut();
                        tab.sort = tab.sort.toggled(key);
                        self.recompute(self.active);
                    }
                    None => match column {
                        Some(column) => self.flash(format!("can't sort by {column}")),
                        None => self.flash(format!("there's no column {c}")),
                    },
                }
            }
            KeyCode::Char(c) if TOGGLES.iter().any(|(k, _, _)| *k == c) => {
                let (_, term, _) = TOGGLES.iter().find(|(k, _, _)| *k == c).unwrap();
                self.set_filter(toggle_term(&self.tab().filter_text, term));
            }
            KeyCode::Char('r') => {
                if self.offline {
                    self.flash("offline: not refreshing".to_owned());
                }
                return self.fetch(self.active);
            }
            KeyCode::Char('o') => return self.open(|pr| pr.url.clone()),
            KeyCode::Char('O') => return self.open(|pr| format!("{}/files", pr.url)),
            KeyCode::Char('y') => return self.copy(|pr| pr.url.clone()),
            KeyCode::Char('Y') => return self.copy(|pr| format!("{}#{}", pr.repo, pr.number)),
            KeyCode::Char('v') => self.toggle_mark(),
            KeyCode::Char('V') => self.toggle_mark_all(),
            KeyCode::Char('f') => self.facets = Some(0),
            KeyCode::Char(' ') => return self.toggle_seen(),
            KeyCode::Char('N') => return self.mark_all_seen(),
            KeyCode::Char('z') => return self.start_snooze(),
            KeyCode::Char('M') => return self.toggle_mute(),
            _ => {}
        }
        Vec::new()
    }

    fn switch_to(&mut self, index: usize) -> Vec<Cmd> {
        if index >= self.tabs.len() || index == self.active {
            return Vec::new();
        }
        self.active = index;
        // Scores and "waiting" depend on the time, so re-sort what's cached.
        self.recompute(index);
        if self.is_stale(index) {
            self.fetch(index)
        } else {
            Vec::new()
        }
    }

    fn is_stale(&self, index: usize) -> bool {
        self.tabs[index]
            .last_attempt
            .is_none_or(|at| self.now.duration_since(at) >= self.refresh_interval)
    }

    // --- Local state: seen, snoozed, muted ---
    //
    // These act on the marked PRs (`v`), or the selected one if none are marked.

    /// The marked PRs in table order, or the selected one: what actions apply to.
    pub fn targets(&self) -> Vec<&Pr> {
        let tab = self.tab();
        if tab.marked.is_empty() {
            return tab.selected().into_iter().collect();
        }
        tab.prs
            .iter()
            .filter(|pr| tab.marked.contains(&pr.id))
            .collect()
    }

    /// Short description of the targets for status messages: `#12` or `3 PRs`.
    fn describe(targets: &[&Pr]) -> String {
        match targets {
            [pr] => format!("#{}", pr.number),
            _ => format!("{} PRs", targets.len()),
        }
    }

    fn target_ids(&self) -> Vec<String> {
        self.targets().iter().map(|pr| pr.id.clone()).collect()
    }

    fn open(&mut self, url: impl Fn(&Pr) -> String) -> Vec<Cmd> {
        let targets = self.targets();
        if targets.is_empty() {
            return Vec::new();
        }
        let mut cmds: Vec<Cmd> = targets.iter().map(|pr| Cmd::Open(url(pr))).collect();
        let ids = self.target_ids();
        for id in &ids {
            self.state.mark_seen(id, self.now);
        }
        self.recompute(self.active);
        cmds.push(Cmd::SaveState(self.state.clone()));
        cmds
    }

    fn copy(&self, text: impl Fn(&Pr) -> String) -> Vec<Cmd> {
        let targets = self.targets();
        if targets.is_empty() {
            return Vec::new();
        }
        let lines: Vec<String> = targets.iter().map(|pr| text(pr)).collect();
        vec![Cmd::Copy(lines.join("\n"))]
    }

    fn toggle_seen(&mut self) -> Vec<Cmd> {
        let targets = self.targets();
        if targets.is_empty() {
            return Vec::new();
        }
        // Mixed: mark them all seen. All seen already: make them new again.
        let any_new = targets.iter().any(|pr| self.flags(pr).new);
        for id in self.target_ids() {
            if any_new {
                self.state.mark_seen(&id, self.now);
            } else {
                self.state.mark_unseen(&id);
            }
        }
        self.changed_state()
    }

    fn mark_all_seen(&mut self) -> Vec<Cmd> {
        let ids: Vec<String> = self.tab().prs.iter().map(|pr| pr.id.clone()).collect();
        for id in &ids {
            self.state.mark_seen(id, self.now);
        }
        self.flash(format!("marked {} PRs as seen", ids.len()));
        self.changed_state()
    }

    fn start_snooze(&mut self) -> Vec<Cmd> {
        let targets = self.targets();
        if targets.is_empty() {
            return Vec::new();
        }
        if targets.iter().all(|pr| self.flags(pr).snoozed.is_some()) {
            let label = format!("{} no longer snoozed", Self::describe(&targets));
            for id in self.target_ids() {
                self.state.unsnooze(&id);
            }
            self.flash(label);
            self.tab_mut().marked.clear();
            return self.changed_state();
        }
        self.snoozing = true;
        Vec::new()
    }

    fn snooze_key(&mut self, key: KeyEvent) -> Vec<Cmd> {
        self.snoozing = false;
        let KeyCode::Char(c) = key.code else {
            return Vec::new();
        };
        let Some((_, label, duration)) = SNOOZES.iter().find(|(k, _, _)| *k == c) else {
            return Vec::new();
        };
        let snoozes: Vec<(String, Snooze)> = self
            .targets()
            .iter()
            .map(|pr| {
                let snooze = match duration {
                    Some(duration) => Snooze::Until(self.now + *duration),
                    None => Snooze::UntilActivity {
                        after: pr.others_active_at.unwrap_or(pr.created_at),
                    },
                };
                (pr.id.clone(), snooze)
            })
            .collect();
        if snoozes.is_empty() {
            return Vec::new();
        }
        let what = Self::describe(&self.targets());
        for (id, snooze) in snoozes {
            self.state.snooze(&id, snooze);
        }
        self.flash(format!("snoozed {what} for {label} (Z shows snoozed)"));
        self.tab_mut().marked.clear();
        self.changed_state()
    }

    fn toggle_mute(&mut self) -> Vec<Cmd> {
        let targets = self.targets();
        if targets.is_empty() {
            return Vec::new();
        }
        let unmute = targets.iter().all(|pr| self.flags(pr).muted);
        let what = Self::describe(&targets);
        for id in self.target_ids() {
            self.state.set_muted(&id, !unmute);
        }
        self.flash(if unmute {
            format!("unmuted {what}")
        } else {
            format!("muted {what} (filter `muted` shows muted PRs)")
        });
        self.tab_mut().marked.clear();
        self.changed_state()
    }

    fn changed_state(&mut self) -> Vec<Cmd> {
        self.recompute(self.active);
        vec![Cmd::SaveState(self.state.clone())]
    }

    // --- Marking rows (v / V) ---

    fn toggle_mark(&mut self) {
        let tab = self.tab_mut();
        let Some(id) = tab.selected().map(|pr| pr.id.clone()) else {
            return;
        };
        if !tab.marked.remove(&id) {
            tab.marked.insert(id);
        }
        tab.move_by(1);
    }

    fn toggle_mark_all(&mut self) {
        let tab = self.tab_mut();
        if tab.marked.is_empty() {
            tab.marked = tab.prs.iter().map(|pr| pr.id.clone()).collect();
        } else {
            tab.marked.clear();
        }
    }

    // --- Facets panel (f) ---

    pub fn facet_groups(&self) -> Vec<facets::Group> {
        let tab = self.tab();
        facets::groups(&tab.all, &tab.prs, &tab.filter_text)
    }

    fn facet_key(&mut self, key: KeyEvent) -> Vec<Cmd> {
        let Some(selected) = self.facets else {
            return Vec::new();
        };
        let groups = self.facet_groups();
        let n = facets::len(&groups);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.facets = Some((selected + 1).min(n.saturating_sub(1)))
            }
            KeyCode::Char('k') | KeyCode::Up => self.facets = Some(selected.saturating_sub(1)),
            KeyCode::Char('g') | KeyCode::Home => self.facets = Some(0),
            KeyCode::Char('G') | KeyCode::End => self.facets = Some(n.saturating_sub(1)),
            KeyCode::Char(' ') | KeyCode::Char('x') => {
                let Some((facet, value)) = facets::nth(&groups, selected) else {
                    return Vec::new();
                };
                let (facet, value) = (facet, value.to_owned());
                let negated = key.code == KeyCode::Char('x');
                self.set_filter(filter::toggle_value(
                    &self.tab().filter_text,
                    facet,
                    &value,
                    negated,
                ));
                // Counts changed, so values may have moved; follow the one just toggled.
                let groups = self.facet_groups();
                let index = (0..facets::len(&groups))
                    .find(|&i| facets::nth(&groups, i) == Some((facet, value.as_str())));
                self.facets = Some(index.unwrap_or(selected));
            }
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('f') | KeyCode::Char('q') => {
                self.facets = None;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            _ => {}
        }
        Vec::new()
    }

    // --- Prompts ---

    fn open_prompt(&mut self, kind: PromptKind) {
        let original = match kind {
            PromptKind::Filter => self.tab().filter_text.clone(),
            PromptKind::Sort => self.tab().sort.to_text(),
        };
        self.prompt = Some(Prompt {
            kind,
            input: LineInput::new(&original),
            error: None,
            completion: None,
            menu: None,
            original,
        });
        self.refresh_completion();
    }

    /// Editing applies live whenever the text is valid; Enter keeps it, Esc undoes it.
    /// Tab completes, opening a menu when there are several choices.
    fn prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        let kind = prompt.kind;
        let at_end = prompt.input.cursor() == prompt.input.text().chars().count();
        let has_ghost = prompt
            .completion
            .as_ref()
            .and_then(Completion::ghost)
            .is_some();
        match (key.code, prompt.menu) {
            (KeyCode::Tab, _) => self.tab_complete(true),
            (KeyCode::BackTab, _) => self.tab_complete(false),
            (KeyCode::Down, Some(_)) => self.move_menu(true),
            (KeyCode::Up, Some(_)) => self.move_menu(false),
            (KeyCode::Enter, Some(selected)) => self.accept(selected),
            (KeyCode::Esc, Some(_)) => prompt.menu = None,
            (KeyCode::Right, None) if at_end && has_ghost => self.accept(0),
            (KeyCode::Esc, None) => {
                let original = prompt.original.clone();
                self.prompt = None;
                // The original was valid when the prompt opened.
                let _ = self.apply(kind, &original);
            }
            (KeyCode::Enter, None) => {
                let text = prompt.input.text().to_owned();
                match self.apply(kind, &text) {
                    Ok(()) => self.prompt = None,
                    Err(error) => {
                        if let Some(prompt) = self.prompt.as_mut() {
                            prompt.error = Some(error);
                        }
                    }
                }
            }
            _ => {
                let changed = prompt.input.handle(key);
                self.refresh_completion();
                if changed {
                    self.apply_live();
                }
            }
        }
    }

    fn tab_complete(&mut self, forward: bool) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        if prompt.menu.is_some() {
            return self.move_menu(forward);
        }
        let Some(completion) = prompt.completion.clone() else {
            return;
        };
        match completion.candidates.len() {
            0 => {}
            1 => self.accept(0),
            n => {
                // Fill in what all the choices share first, like a shell does.
                if let Some(prefix) = completion.common_prefix() {
                    let text = prompt.input.text();
                    let chars: Vec<char> = text.chars().collect();
                    let before: String = chars[..completion.start].iter().collect();
                    let after: String = chars[completion.end..].iter().collect();
                    let cursor = completion.start + prefix.chars().count();
                    prompt
                        .input
                        .set(&format!("{before}{prefix}{after}"), cursor);
                    self.refresh_completion();
                    self.apply_live();
                }
                if let Some(prompt) = self.prompt.as_mut()
                    && prompt
                        .completion
                        .as_ref()
                        .is_some_and(|c| !c.candidates.is_empty())
                {
                    prompt.menu = Some(if forward { 0 } else { n - 1 });
                }
            }
        }
        self.clamp_menu();
    }

    fn move_menu(&mut self, forward: bool) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        let n = prompt.completion.as_ref().map_or(0, |c| c.candidates.len());
        if let Some(selected) = prompt.menu.as_mut()
            && n > 0
        {
            *selected = if forward {
                (*selected + 1) % n
            } else {
                (*selected + n - 1) % n
            };
        }
    }

    fn accept(&mut self, index: usize) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        let Some(completion) = &prompt.completion else {
            return;
        };
        if index >= completion.candidates.len() {
            return;
        }
        let (text, cursor) = completion.accept(prompt.input.text(), index);
        prompt.input.set(&text, cursor);
        prompt.menu = None;
        self.refresh_completion();
        self.apply_live();
    }

    fn refresh_completion(&mut self) {
        let Some(prompt) = self.prompt.as_ref() else {
            return;
        };
        let (text, cursor) = (prompt.input.text(), prompt.input.cursor());
        let completion = match prompt.kind {
            PromptKind::Filter => complete::filter(
                text,
                cursor,
                &self.tab().all,
                &self.viewer,
                self.now,
                Some(&self.state),
            ),
            PromptKind::Sort => complete::sort(text, cursor),
        };
        if let Some(prompt) = self.prompt.as_mut() {
            prompt.completion = completion;
        }
        self.clamp_menu();
    }

    /// Close the menu if nothing's left to choose, or keep the selection in range.
    fn clamp_menu(&mut self) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        let n = prompt.completion.as_ref().map_or(0, |c| c.candidates.len());
        prompt.menu = match prompt.menu {
            _ if n == 0 => None,
            Some(selected) => Some(selected.min(n - 1)),
            None => None,
        };
    }

    /// Apply the prompt's text as it's typed. A word that's still being completed
    /// (`tu` on the way to `turn:`) is left alone rather than searched for or
    /// reported as an error.
    fn apply_live(&mut self) {
        let Some(prompt) = self.prompt.as_ref() else {
            return;
        };
        let incomplete = prompt
            .completion
            .as_ref()
            .is_some_and(Completion::is_incomplete);
        let (kind, text) = (prompt.kind, prompt.input.text().to_owned());
        let error = if incomplete {
            None
        } else {
            self.apply(kind, &text).err()
        };
        if let Some(prompt) = self.prompt.as_mut() {
            prompt.error = error;
        }
    }

    /// Apply prompt text, leaving the current filter or sort alone if it's invalid.
    fn apply(&mut self, kind: PromptKind, text: &str) -> Result<(), String> {
        let tab = self.tab_mut();
        match kind {
            PromptKind::Filter => {
                tab.filter = Filter::parse(text)?;
                tab.filter_text = text.trim().to_owned();
            }
            PromptKind::Sort => tab.sort = SortSpec::parse(text)?,
        }
        self.recompute(self.active);
        Ok(())
    }

    fn set_filter(&mut self, text: String) {
        match Filter::parse(&text) {
            Ok(filter) => {
                let tab = self.tab_mut();
                tab.filter = filter;
                tab.filter_text = text;
                self.recompute(self.active);
            }
            Err(e) => self.flash(e),
        }
    }

    // --- Helpers ---

    fn fetch(&mut self, index: usize) -> Vec<Cmd> {
        let now = self.now;
        let tab = &mut self.tabs[index];
        if self.offline || tab.refreshing {
            return Vec::new();
        }
        tab.refreshing = true;
        tab.last_attempt = Some(now);
        vec![Cmd::Fetch(index)]
    }

    fn flash(&mut self, text: String) {
        self.flash = Some((text, self.now));
    }

    fn context(&self) -> filter::Context<'_> {
        filter::Context {
            viewer: &self.viewer,
            now: self.now,
            local: Some(&self.state),
        }
    }

    /// Re-filter and re-sort a tab, keeping the same PR selected if it's still shown.
    fn recompute(&mut self, index: usize) {
        let ctx = filter::Context {
            viewer: &self.viewer,
            now: self.now,
            local: Some(&self.state),
        };
        let tab = &mut self.tabs[index];
        let selected_id = tab.selected().map(|pr| pr.id.clone());
        let old_index = tab.table.selected().unwrap_or(0);

        let mut prs: Vec<Pr> = tab
            .all
            .iter()
            .filter(|pr| tab.filter.shows(pr, &ctx))
            .cloned()
            .collect();
        tab.sort.sort(&mut prs, &self.weights, self.now);
        tab.prs = prs;

        let index = selected_id
            .and_then(|id| tab.prs.iter().position(|pr| pr.id == id))
            .unwrap_or(old_index);
        tab.select(index);
        // Marks for PRs that are gone from the view entirely go too.
        let present: BTreeSet<&str> = tab.all.iter().map(|pr| pr.id.as_str()).collect();
        tab.marked.retain(|id| present.contains(id.as_str()));
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::test_support::{now, pr};

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn char(c: char) -> Msg {
        key(KeyCode::Char(c))
    }

    fn ctrl(c: char) -> Msg {
        Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn typing(app: &mut App, text: &str) {
        for c in text.chars() {
            app.update(char(c));
        }
    }

    fn tab_settings(view: &str) -> TabSettings {
        TabSettings {
            view_name: view.into(),
            filter: String::new(),
            sort: SortSpec::parse("number").unwrap(),
        }
    }

    /// One tab per view name. The local state predates the sample PRs' activity,
    /// so they all start out as new.
    pub fn settings_with(views: &[&str], offline: bool) -> Settings {
        Settings {
            tabs: views.iter().map(|v| tab_settings(v)).collect(),
            active: 0,
            refresh_interval: SignedDuration::from_mins(5),
            offline,
            weights: ScoreWeights::default(),
            state: LocalState::new("2026-09-01T00:00:00Z".parse().unwrap()),
            columns: crate::columns::DEFAULT.to_vec(),
        }
    }

    pub fn settings(view: &str, offline: bool) -> Settings {
        settings_with(&[view], offline)
    }

    fn app_with(n: u64) -> App {
        let mut app = App::new(settings("for-me", false), now()).with_cache(
            0,
            (1..=n).map(pr).collect(),
            "me".into(),
            now(),
        );
        app.page_rows = 4;
        app
    }

    fn fetched_for(tab: usize, prs: Vec<Pr>) -> Msg {
        Msg::Fetched {
            tab,
            result: Ok(FetchedData {
                prs,
                viewer: "me".into(),
                truncated: false,
                at: now(),
            }),
        }
    }

    fn fetched(prs: Vec<Pr>) -> Msg {
        fetched_for(0, prs)
    }

    fn selected_number(app: &App) -> Option<u64> {
        app.selected().map(|pr| pr.number)
    }

    fn shown(app: &App) -> Vec<u64> {
        app.tab().prs.iter().map(|pr| pr.number).collect()
    }

    fn saves_state(cmds: &[Cmd]) -> bool {
        cmds.iter().any(|c| matches!(c, Cmd::SaveState(_)))
    }

    #[test]
    fn navigation_stays_in_bounds() {
        let mut app = app_with(10);
        assert_eq!(selected_number(&app), Some(1));
        app.update(key(KeyCode::Up));
        assert_eq!(selected_number(&app), Some(1));
        app.update(char('j'));
        assert_eq!(selected_number(&app), Some(2));
        app.update(ctrl('d'));
        assert_eq!(selected_number(&app), Some(4));
        app.update(key(KeyCode::PageDown));
        assert_eq!(selected_number(&app), Some(8));
        app.update(char('G'));
        assert_eq!(selected_number(&app), Some(10));
        app.update(key(KeyCode::Down));
        assert_eq!(selected_number(&app), Some(10));
        app.update(char('g'));
        assert_eq!(selected_number(&app), Some(1));
    }

    #[test]
    fn empty_list_has_no_selection_and_actions_do_nothing() {
        let mut app = App::new(settings("for-me", false), now());
        app.update(key(KeyCode::Down));
        assert!(app.selected().is_none());
        assert!(app.update(char('o')).is_empty());
        assert!(app.update(char(' ')).is_empty());
        assert!(app.update(char('z')).is_empty());
        assert!(!app.snoozing);
    }

    #[test]
    fn open_and_copy_act_on_the_selection() {
        let mut app = app_with(3);
        app.update(key(KeyCode::Down));
        let url = "https://github.com/acme/api/pull/2";
        let cmds = app.update(char('o'));
        assert_eq!(cmds[0], Cmd::Open(url.into()));
        assert!(saves_state(&cmds), "opening marks it seen");
        assert_eq!(app.update(char('O'))[0], Cmd::Open(format!("{url}/files")));
        assert_eq!(app.update(char('y')), [Cmd::Copy(url.into())]);
        assert_eq!(app.update(char('Y')), [Cmd::Copy("acme/api#2".into())]);
    }

    #[test]
    fn refresh_keeps_the_selected_pr() {
        let mut app = app_with(5);
        app.update(char('j'));
        app.update(char('j'));
        assert_eq!(selected_number(&app), Some(3));

        // PR 1 disappeared, so PR 3 moved up a row; the selection follows it.
        let cmds = app.update(fetched([3, 2, 4].into_iter().map(pr).collect()));
        assert_eq!(selected_number(&app), Some(3));
        assert_eq!(app.tab().table.selected(), Some(1));
        assert!(!app.tab().from_cache);
        assert!(saves_state(&cmds), "records which PRs are still around");
    }

    #[test]
    fn selection_stays_in_place_when_the_pr_disappears() {
        let mut app = app_with(5);
        app.update(char('G'));
        app.update(fetched([1, 2].into_iter().map(pr).collect()));
        assert_eq!(selected_number(&app), Some(2), "clamped to the new end");
    }

    #[test]
    fn fetches_at_start_then_on_the_interval() {
        let mut app = app_with(1);
        assert_eq!(app.init(), [Cmd::Fetch(0)]);
        assert!(app.update(char('r')).is_empty(), "already refreshing");

        app.update(fetched(vec![]));
        let later = |mins| Msg::Tick(now() + SignedDuration::from_mins(mins));
        assert!(app.update(later(4)).is_empty());
        assert_eq!(app.update(later(5)), [Cmd::Fetch(0)]);
    }

    #[test]
    fn failed_fetch_keeps_data_and_shows_error() {
        let mut app = app_with(2);
        app.init();
        app.update(Msg::Fetched {
            tab: 0,
            result: Err("network down".into()),
        });
        assert_eq!(app.tab().prs.len(), 2);
        assert_eq!(app.tab().error.as_deref(), Some("network down"));
        assert!(app.tab().from_cache);
        assert_eq!(app.update(char('r')), [Cmd::Fetch(0)], "manual retry");
    }

    #[test]
    fn offline_never_fetches() {
        let mut app = App::new(settings("for-me", true), now());
        assert!(app.init().is_empty());
        assert!(app.update(char('r')).is_empty());
        assert!(app.flash.is_some());
        assert!(
            app.update(Msg::Tick(now() + SignedDuration::from_hours(1)))
                .is_empty()
        );
    }

    #[test]
    fn flash_messages_expire() {
        let mut app = app_with(1);
        app.update(Msg::Flash("copied".into()));
        app.update(Msg::Tick(now() + SignedDuration::from_secs(2)));
        assert!(app.flash.is_some());
        app.update(Msg::Tick(now() + SignedDuration::from_secs(5)));
        assert!(app.flash.is_none());
    }

    #[test]
    fn help_closes_on_any_key_and_quit_keys_work() {
        let mut app = app_with(1);
        app.update(char('?'));
        assert!(app.show_help);
        app.update(char('q'));
        assert!(!app.show_help);
        assert!(!app.quit, "q closes help first");
        app.update(char('q'));
        assert!(app.quit);

        let mut app = app_with(1);
        app.update(ctrl('c'));
        assert!(app.quit);
    }

    #[test]
    fn esc_clears_the_filter_but_does_not_quit() {
        let mut app = app_with(3);
        app.update(key(KeyCode::Esc));
        assert!(!app.quit);
        app.update(char('/'));
        typing(&mut app, "#2");
        app.update(key(KeyCode::Enter));
        assert_eq!(shown(&app), [2]);
        app.update(key(KeyCode::Esc));
        assert_eq!(shown(&app), [1, 2, 3]);
        assert_eq!(app.tab().filter_text, "");
    }

    #[test]
    fn enter_toggles_detail() {
        let mut app = app_with(1);
        assert!(app.show_detail);
        app.update(key(KeyCode::Enter));
        assert!(!app.show_detail);
    }

    #[test]
    fn filter_prompt_applies_live_and_keeps_the_last_valid_filter() {
        let mut app = app_with(3);
        app.update(char('/'));
        assert!(app.prompt.is_some());
        typing(&mut app, "#3");
        assert_eq!(shown(&app), [3], "applied while typing");

        typing(&mut app, " turn:yo");
        // `turn` and `turn:` were incomplete words, so nothing was applied on the way.
        assert_eq!(
            app.tab().filter_text,
            "#3",
            "invalid text keeps the last valid filter"
        );
        assert!(app.prompt.as_ref().unwrap().error.is_some());
        app.update(key(KeyCode::Enter));
        assert!(app.prompt.is_some(), "Enter doesn't accept invalid text");

        app.update(key(KeyCode::Backspace));
        app.update(key(KeyCode::Backspace));
        typing(&mut app, "me");
        assert!(app.prompt.as_ref().unwrap().error.is_none());
        app.update(key(KeyCode::Enter));
        assert!(app.prompt.is_none());
        assert_eq!(app.tab().filter_text, "#3 turn:me");
    }

    #[test]
    fn keys_in_the_prompt_are_text_not_commands() {
        let mut app = app_with(3);
        app.update(char('/'));
        typing(&mut app, "q");
        assert!(!app.quit);
        assert_eq!(app.prompt.as_ref().unwrap().input.text(), "q");
    }

    #[test]
    fn esc_in_the_prompt_restores_the_previous_value() {
        let mut app = app_with(3);
        app.update(char('/'));
        typing(&mut app, "#1");
        assert_eq!(shown(&app), [1]);
        app.update(key(KeyCode::Esc));
        assert!(app.prompt.is_none());
        assert_eq!(shown(&app), [1, 2, 3]);
    }

    #[test]
    fn sort_prompt_and_keys() {
        let mut app = app_with(3);
        app.update(char('s'));
        assert_eq!(app.prompt.as_ref().unwrap().input.text(), "number");
        app.update(ctrl('u'));
        typing(&mut app, "-number");
        app.update(key(KeyCode::Enter));
        assert_eq!(shown(&app), [3, 2, 1]);

        app.update(char('S'));
        assert_eq!(shown(&app), [1, 2, 3], "S reverses");

        app.update(char('3')); // the # column, already primary: flips
        assert_eq!(app.tab().sort.to_text(), "-number");
        app.update(char('1'));
        assert_eq!(app.tab().sort.to_text(), "turn");
        app.update(char('0'));
        app.update(char('9')); // the waiting column
        assert_eq!(app.tab().sort.to_text(), "waiting");
        app.update(char('/'));
        app.update(key(KeyCode::Esc));
        app.columns = vec![Column::Last];
        app.update(char('1'));
        assert_eq!(app.tab().sort.to_text(), "waiting", "can't sort by last");
        assert!(app.flash.as_ref().unwrap().0.contains("can't sort"));
        app.update(char('2'));
        assert!(app.flash.as_ref().unwrap().0.contains("no column 2"));
    }

    #[test]
    fn toggles_add_and_remove_terms() {
        let mut app = app_with(3);
        let mut draft = pr(4);
        draft.is_draft = true;
        app.update(fetched(vec![pr(1), draft]));
        app.update(char('d'));
        assert_eq!(app.tab().filter_text, "-draft");
        assert_eq!(shown(&app), [1]);
        app.update(char('m'));
        assert_eq!(app.tab().filter_text, "-draft turn:me");
        app.update(char('d'));
        assert_eq!(app.tab().filter_text, "turn:me");
        assert_eq!(shown(&app), [1, 4]);
    }

    fn tab() -> Msg {
        key(KeyCode::Tab)
    }

    fn prompt(app: &App) -> &Prompt {
        app.prompt.as_ref().unwrap()
    }

    #[test]
    fn incomplete_keys_are_not_applied_while_typing() {
        let mut app = app_with(3);
        app.update(char('/'));
        typing(&mut app, "fi");
        assert_eq!(shown(&app), [1, 2, 3], "`fi` could still become `files:`");
        assert!(prompt(&app).error.is_none());
        assert_eq!(
            prompt(&app).completion.as_ref().unwrap().ghost(),
            Some("les:")
        );
        typing(&mut app, "x");
        assert_eq!(
            app.tab().filter_text,
            "fix",
            "a finished word is searched for"
        );
    }

    #[test]
    fn tab_accepts_a_single_match_then_offers_values() {
        let mut app = app_with(3);
        app.update(char('/'));
        typing(&mut app, "tu");
        app.update(tab());
        assert_eq!(prompt(&app).input.text(), "turn:");
        assert!(prompt(&app).menu.is_none());
        assert_eq!(
            prompt(&app).completion.as_ref().unwrap().ghost(),
            Some("me")
        );
        assert!(
            prompt(&app).error.is_none(),
            "an empty value isn't an error yet"
        );

        app.update(key(KeyCode::Right));
        assert_eq!(prompt(&app).input.text(), "turn:me ");
        assert_eq!(app.tab().filter_text, "turn:me");
    }

    #[test]
    fn tab_opens_a_menu_to_cycle_and_pick_from() {
        let mut app = app_with(3);
        app.update(char('/'));
        typing(&mut app, "turn:");
        app.update(tab());
        assert_eq!(prompt(&app).menu, Some(0));
        app.update(tab());
        app.update(key(KeyCode::Down));
        assert_eq!(prompt(&app).menu, Some(2));
        app.update(key(KeyCode::BackTab));
        assert_eq!(prompt(&app).menu, Some(1));
        app.update(key(KeyCode::Up));
        app.update(key(KeyCode::Up));
        let last = prompt(&app).completion.as_ref().unwrap().candidates.len() - 1;
        assert_eq!(prompt(&app).menu, Some(last), "wraps around");

        app.update(key(KeyCode::Esc));
        assert!(prompt(&app).menu.is_none(), "Esc closes only the menu");
        assert!(app.prompt.is_some());

        app.update(tab());
        app.update(key(KeyCode::Down));
        let second = prompt(&app).completion.as_ref().unwrap().candidates[1]
            .replacement
            .clone();
        app.update(key(KeyCode::Enter));
        assert!(
            app.prompt.is_some(),
            "Enter picks from the menu, not the prompt"
        );
        assert_eq!(prompt(&app).input.text(), format!("turn:{second} "));
        assert_eq!(app.tab().filter_text, format!("turn:{second}"));
    }

    #[test]
    fn tab_fills_in_the_shared_prefix_first() {
        let mut app = app_with(3);
        app.update(char('/'));
        typing(&mut app, "revi");
        app.update(tab());
        assert_eq!(prompt(&app).input.text(), "review");
        assert_eq!(prompt(&app).menu, Some(0), "and shows the choices");
    }

    #[test]
    fn typing_narrows_an_open_menu_and_closes_it_when_empty() {
        let mut app = app_with(3);
        app.update(char('/'));
        typing(&mut app, "ci:");
        app.update(tab());
        let before = prompt(&app).completion.as_ref().unwrap().candidates.len();
        typing(&mut app, "p");
        let after = prompt(&app).completion.as_ref().unwrap().candidates.len();
        assert!(after < before);
        assert!(prompt(&app).menu.is_some());
        typing(&mut app, "zz");
        assert!(prompt(&app).menu.is_none());
    }

    #[test]
    fn enter_applies_a_complete_filter_even_if_it_was_deferred() {
        let mut app = app_with(3);
        app.update(char('/'));
        typing(&mut app, "draf");
        assert_eq!(app.tab().filter_text, "");
        app.update(key(KeyCode::Enter));
        // `draf` isn't a flag, so it's a word search that matches nothing.
        assert!(app.prompt.is_none());
        assert_eq!(app.tab().filter_text, "draf");
    }

    #[test]
    fn sort_prompt_completes_keys() {
        let mut app = app_with(3);
        app.update(char('s'));
        app.update(ctrl('u'));
        typing(&mut app, "-num");
        app.update(tab());
        assert_eq!(prompt(&app).input.text(), "-number");
        assert_eq!(app.tab().sort.to_text(), "-number");
    }

    #[test]
    fn filter_uses_the_viewer_for_at_me() {
        let mut app = app_with(2);
        let mut mine = pr(3);
        mine.author = Some("me".into());
        app.update(fetched(vec![pr(1), mine]));
        app.update(char('/'));
        typing(&mut app, "author:@me");
        assert_eq!(shown(&app), [3]);
    }

    // --- Tabs ---

    fn tabbed() -> App {
        App::new(settings_with(&["for-me", "mine", "repo"], false), now()).with_cache(
            0,
            vec![pr(1), pr(2)],
            "me".into(),
            now(),
        )
    }

    #[test]
    fn switching_tabs_fetches_ones_not_loaded_yet() {
        let mut app = tabbed();
        assert_eq!(app.init(), [Cmd::Fetch(0)]);
        assert_eq!(app.update(char(']')), [Cmd::Fetch(1)]);
        assert_eq!(app.active, 1);
        assert!(app.tab().prs.is_empty());
        assert_eq!(app.update(char(']')), [Cmd::Fetch(2)]);
        assert!(
            app.update(char(']')).is_empty(),
            "back to 0, fetched just now"
        );
        assert_eq!(app.active, 0);
        assert!(app.update(char('[')).is_empty(), "2 is still refreshing");
        assert_eq!(app.active, 2);

        let alt = |c| Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT));
        app.update(alt('2'));
        assert_eq!(app.active, 1);
        app.update(alt('9'));
        assert_eq!(app.active, 1, "no such tab");
    }

    #[test]
    fn tabs_keep_their_own_filter_and_data() {
        let mut app = tabbed();
        app.update(char('d'));
        assert_eq!(app.tab().filter_text, "-draft");
        app.update(char(']'));
        assert_eq!(app.tab().filter_text, "");

        // A background tab's fetch doesn't disturb the one on screen.
        app.update(fetched_for(0, vec![pr(7)]));
        assert!(app.tab().prs.is_empty());
        app.update(char('['));
        assert_eq!(shown(&app), [7]);
        assert_eq!(app.tab().filter_text, "-draft");
    }

    #[test]
    fn only_the_visible_tab_refreshes_in_the_background() {
        let mut app = tabbed();
        app.init();
        app.update(fetched_for(0, vec![pr(1)]));
        let later = Msg::Tick(now() + SignedDuration::from_mins(10));
        assert_eq!(app.update(later), [Cmd::Fetch(0)]);
    }

    // --- Seen, snoozed, muted ---

    #[test]
    fn space_toggles_seen_and_n_shows_only_new() {
        let mut app = app_with(3);
        assert_eq!(app.new_count(), 3);
        assert!(saves_state(&app.update(char(' '))));
        assert_eq!(app.new_count(), 2);
        app.update(char('n'));
        assert_eq!(shown(&app), [2, 3]);
        app.update(char('n'));
        app.update(char('g'));
        app.update(char(' '));
        assert_eq!(app.new_count(), 3, "space again marks it unseen");
        app.update(char('N'));
        assert_eq!(app.new_count(), 0);
    }

    #[test]
    fn snoozing_hides_until_unsnoozed() {
        let mut app = app_with(3);
        app.update(char('j'));
        assert!(app.update(char('z')).is_empty());
        assert!(app.snoozing);
        assert!(saves_state(&app.update(char('w'))));
        assert!(!app.snoozing);
        assert_eq!(shown(&app), [1, 3]);
        assert_eq!(
            selected_number(&app),
            Some(3),
            "selection moves to the next row"
        );
        assert_eq!(app.hidden_count(), 1);

        app.update(char('Z'));
        assert_eq!(shown(&app), [2]);
        app.update(char('z'));
        assert!(!app.snoozing, "z on a snoozed PR unsnoozes it");
        app.update(char('Z'));
        assert_eq!(shown(&app), [1, 2, 3]);
    }

    #[test]
    fn snooze_until_activity_wakes_up() {
        let mut app = app_with(2);
        app.update(char('z'));
        app.update(char('u'));
        assert_eq!(shown(&app), [2]);
        let mut active = pr(1);
        active.others_active_at = Some(now());
        app.update(fetched(vec![active, pr(2)]));
        assert_eq!(shown(&app), [1, 2]);
    }

    #[test]
    fn other_keys_cancel_snoozing() {
        let mut app = app_with(2);
        app.update(char('z'));
        assert!(app.update(key(KeyCode::Esc)).is_empty());
        assert!(!app.snoozing);
        assert_eq!(shown(&app), [1, 2]);
        assert!(!app.quit);
    }

    // --- Marking and bulk actions ---

    #[test]
    fn marked_rows_are_what_actions_apply_to() {
        let mut app = app_with(4);
        app.update(char('v'));
        assert_eq!(selected_number(&app), Some(2), "v moves down");
        app.update(char('j'));
        app.update(char('v'));
        let targets: Vec<u64> = app.targets().iter().map(|pr| pr.number).collect();
        assert_eq!(targets, [1, 3]);

        let cmds = app.update(char('o'));
        let opened: Vec<&Cmd> = cmds.iter().filter(|c| matches!(c, Cmd::Open(_))).collect();
        assert_eq!(opened.len(), 2);
        assert_eq!(app.new_count(), 2, "both marked as seen");

        let url = |n| format!("https://github.com/acme/api/pull/{n}");
        assert_eq!(
            app.update(char('y')),
            [Cmd::Copy(format!("{}\n{}", url(1), url(3)))]
        );

        app.update(char('z'));
        app.update(char('d'));
        assert_eq!(shown(&app), [2, 4], "both snoozed");
        assert!(
            app.tab().marked.is_empty(),
            "marks go once they're acted on"
        );
    }

    #[test]
    fn mark_all_and_clear() {
        let mut app = app_with(3);
        app.update(char('V'));
        assert_eq!(app.targets().len(), 3);
        app.update(char('M'));
        assert!(shown(&app).is_empty(), "all muted");
        app.update(char('/'));
        typing(&mut app, "muted");
        app.update(key(KeyCode::Enter));
        app.update(char('V'));
        app.update(char('M'));
        assert!(shown(&app).is_empty(), "all unmuted, so none match `muted`");

        let mut app = app_with(3);
        app.update(char('V'));
        app.update(key(KeyCode::Esc));
        assert!(app.tab().marked.is_empty());
        assert_eq!(app.targets().len(), 1, "back to the selected row");
        app.update(char('V'));
        app.update(char('V'));
        assert!(app.tab().marked.is_empty(), "V again clears");
    }

    #[test]
    fn marks_survive_filtering_but_only_shown_ones_are_targets() {
        let mut app = app_with(3);
        app.update(char('V'));
        app.update(char('/'));
        typing(&mut app, "#2");
        app.update(key(KeyCode::Enter));
        let targets: Vec<u64> = app.targets().iter().map(|pr| pr.number).collect();
        assert_eq!(targets, [2]);
        assert_eq!(app.tab().marked.len(), 3);
        app.update(fetched(vec![pr(2)]));
        assert_eq!(app.tab().marked.len(), 1, "gone from the view: unmarked");
    }

    // --- Facets ---

    fn facet_rows(app: &App) -> Vec<(&'static str, String)> {
        let groups = app.facet_groups();
        (0..facets::len(&groups))
            .map(|i| {
                let (k, v) = facets::nth(&groups, i).unwrap();
                (k, v.to_owned())
            })
            .collect()
    }

    #[test]
    fn facets_include_and_exclude_values() {
        let mut app = app_with(1);
        let mut web = pr(2);
        web.repo = "acme/web".parse().unwrap();
        app.update(fetched(vec![pr(1), web, pr(3)]));
        app.update(char('f'));
        assert_eq!(app.facets, Some(0));
        let rows = facet_rows(&app);
        let web_row = rows
            .iter()
            .position(|r| *r == ("repo", "acme/web".to_owned()))
            .unwrap();
        for _ in 0..web_row {
            app.update(char('j'));
        }
        app.update(char(' '));
        assert_eq!(app.tab().filter_text, "repo:acme/web");
        assert_eq!(shown(&app), [2]);
        assert_eq!(
            facets::nth(&app.facet_groups(), app.facets.unwrap()),
            Some(("repo", "acme/web")),
            "the selection follows the value as counts reorder the list"
        );

        app.update(char('x'));
        assert_eq!(app.tab().filter_text, "-repo:acme/web");
        assert_eq!(shown(&app), [1, 3]);

        app.update(char('q'));
        assert!(app.facets.is_none(), "q closes the panel");
        assert!(!app.quit);
    }

    #[test]
    fn facet_navigation_stays_in_bounds() {
        let mut app = app_with(2);
        app.update(char('f'));
        app.update(char('k'));
        assert_eq!(app.facets, Some(0));
        app.update(char('G'));
        let last = facets::len(&app.facet_groups()) - 1;
        assert_eq!(app.facets, Some(last));
        app.update(char('j'));
        assert_eq!(app.facets, Some(last));
    }

    #[test]
    fn muting_hides_until_the_filter_asks_for_muted() {
        let mut app = app_with(2);
        assert!(saves_state(&app.update(char('M'))));
        assert_eq!(shown(&app), [2]);
        app.update(char('/'));
        typing(&mut app, "muted");
        app.update(key(KeyCode::Enter));
        assert_eq!(shown(&app), [1]);
        app.update(char('M'));
        assert!(
            shown(&app).is_empty(),
            "unmuted, so no longer matches `muted`"
        );
    }
}
