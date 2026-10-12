//! The setup questions, as a state machine: keys and fetched data go in,
//! requests for more data come out. Drawing is in `ui`, the terminal in `term`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use jiff::Timestamp;
use ratatui::widgets::ListState;
use toml_edit::DocumentMut;

use super::accounts::Account;
use super::migrate::{self, Move};
use super::preview::Sample;
use super::teams::TeamList;
use super::write::{self, Existing, ProfileAnswer, Source};
use crate::config::{self, Config};
use crate::model::{RepoName, ScoreWeights, Viewer};
use crate::profile::Whereabouts;
use crate::tui::input::LineInput;
use crate::views::{self, View};

/// Everything setup needs from outside, so the wizard itself stays pure.
pub struct Env {
    /// The config file being written.
    pub path: PathBuf,
    /// Its current contents, or a fresh document.
    pub doc: DocumentMut,
    pub has_file: bool,
    /// gh's accounts, or why they couldn't be listed.
    pub accounts: Result<Vec<Account>, String>,
    /// Where setup runs, for the "where does each profile apply" preview.
    pub here: Whereabouts,
    pub current_repos: Vec<RepoName>,
    /// The directory holding `state.json`.
    pub state_root: Option<PathBuf>,
    pub exists: Box<dyn Fn(&Path) -> bool>,
    pub env_set: Box<dyn Fn(&str) -> bool>,
}

/// Why setup was offered before the UI started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intro {
    /// There's no config file.
    FirstRun,
    /// The file was written by an older setup (or before there was one).
    Update { from: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    Intro,
    Accounts,
    /// The name of the environment variable for the "token from an
    /// environment variable" row.
    EnvVar,
    Name(usize),
    Default,
    Dirs(usize),
    Email(usize),
    Teams(usize),
    View(usize),
    Bots(usize),
    Review,
}

impl Step {
    /// Its number and title in the header, `4/7 Team reviews`.
    pub fn title(self) -> Option<(u8, &'static str)> {
        Some(match self {
            Step::Intro => return None,
            Step::Accounts | Step::EnvVar => (1, "Accounts"),
            Step::Name(_) => (2, "Profile name"),
            Step::Default | Step::Dirs(_) | Step::Email(_) => (3, "When to use it"),
            Step::Teams(_) => (4, "Team reviews"),
            Step::View(_) => (5, "Default view"),
            Step::Bots(_) => (6, "Bots"),
            Step::Review => (7, "Review"),
        })
    }

    pub fn profile(self) -> Option<usize> {
        match self {
            Step::Name(i)
            | Step::Dirs(i)
            | Step::Email(i)
            | Step::Teams(i)
            | Step::View(i)
            | Step::Bots(i) => Some(i),
            _ => None,
        }
    }
}

pub const STEPS: u8 = 7;

/// How a step takes input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// One of `n` choices.
    Select(usize),
    /// Checkboxes over `n` choices, plus a text row when `other` is set.
    Multi {
        n: usize,
        other: bool,
    },
    Text,
    Review,
}

/// Data the wizard wants fetched.
#[derive(Debug, Clone)]
pub enum Cmd {
    Teams(Source),
    Count { source: Source, team: String },
    Sample(Box<SampleRequest>),
}

#[derive(Debug, Clone)]
pub struct SampleRequest {
    pub source: Source,
    pub view: View,
    pub viewer: Viewer,
    pub weights: ScoreWeights,
}

/// Fetched data coming back.
#[derive(Debug)]
pub enum Data {
    Teams {
        source: Source,
        result: Result<TeamList, String>,
    },
    Count {
        source: Source,
        team: String,
        result: Result<u32, String>,
    },
    Sample {
        source: Source,
        view: View,
        result: Result<Box<Sample>, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(t) => Some(t),
            _ => None,
        }
    }
}

/// A view's sample: `view` is `None` when the view can't be resolved here
/// (`repo` outside a checkout).
#[derive(Debug, Clone)]
pub struct SampleSlot {
    pub view: Option<View>,
    pub load: Load<Box<Sample>>,
}

/// How setup ended.
#[derive(Debug)]
pub enum Outcome {
    Write(Box<Plan>),
    /// Ctrl-C: nothing written.
    Cancelled,
    /// First run, "Skip": write a starter file so it doesn't ask again.
    Skip,
    /// Ask again next time.
    NotNow,
    /// Don't offer this setup version again.
    DontAsk,
}

/// What to do once the answers are in.
#[derive(Debug)]
pub struct Plan {
    pub text: String,
    pub profiles: Vec<ProfileAnswer>,
    pub moves: Vec<Move>,
    /// Profiles in the file that setup removes.
    pub removed: Vec<String>,
    /// Fetched samples, to save as each profile's cache.
    pub caches: Vec<(String, Sample)>,
}

/// One row of the accounts step.
pub struct Row {
    /// `None` for "a token from an environment variable", until named.
    pub source: Option<Source>,
    pub account: Option<Account>,
    pub picked: bool,
    pub draft: ProfileAnswer,
    /// Teams and bots offered as checkboxes; they only grow, so unchecking
    /// one doesn't make it vanish.
    pub team_options: Vec<String>,
    pub bot_options: Vec<String>,
}

pub struct Wizard {
    pub env: Env,
    pub existing: Existing,
    pub intro: Option<Intro>,
    pub step: Step,
    pub rows: Vec<Row>,
    pub now: Timestamp,

    pub cursor: usize,
    pub list: ListState,
    pub input: LineInput,
    /// The "others" text row of the teams and bots steps.
    pub other: LineInput,
    pub error: Option<String>,
    /// A warning was shown; Enter again goes on anyway.
    pub warning: Option<String>,
    pub scroll: u16,
    /// Review: 0 write, 1 back, 2 cancel.
    pub button: usize,

    pub teams: HashMap<Source, Load<TeamList>>,
    pub counts: HashMap<(Source, String), Load<u32>>,
    pub samples: HashMap<(Source, String), SampleSlot>,

    pub outcome: Option<Outcome>,
}

const PAGE: usize = 10;

impl Wizard {
    pub fn new(env: Env, intro: Option<Intro>, now: Timestamp) -> Wizard {
        let existing = Existing::read(&env.doc);
        let mut rows: Vec<Row> = Vec::new();
        let draft_for = |source: &Source| {
            existing
                .profile(source)
                .cloned()
                .unwrap_or_else(|| ProfileAnswer::new(source.clone(), &existing.top))
        };
        for account in env.accounts.as_ref().map(Vec::as_slice).unwrap_or_default() {
            let source = Source::Gh(account.login.clone());
            rows.push(Row::new(
                Some(source.clone()),
                Some(account.clone()),
                draft_for(&source),
            ));
        }
        for profile in &existing.profiles {
            if !rows
                .iter()
                .any(|r| r.source.as_ref() == Some(&profile.source))
            {
                rows.push(Row::new(
                    Some(profile.source.clone()),
                    None,
                    profile.clone(),
                ));
            }
        }
        let mut env_row = Row::new(
            None,
            None,
            ProfileAnswer::new(Source::Env(String::new()), &existing.top),
        );
        env_row.picked = rows.is_empty();
        env_row.draft.name.clear();
        rows.push(env_row);

        let mut wizard = Wizard {
            env,
            existing,
            intro,
            step: Step::Accounts,
            rows,
            now,
            cursor: 0,
            list: ListState::default(),
            input: LineInput::default(),
            other: LineInput::default(),
            error: None,
            warning: None,
            scroll: 0,
            button: 0,
            teams: HashMap::new(),
            counts: HashMap::new(),
            samples: HashMap::new(),
            outcome: None,
        };
        let first = wizard.steps()[0];
        wizard.enter(first);
        wizard
    }

    // ---- answers --------------------------------------------------------

    /// Indexes into `rows` of the picked ones: profile `i` is `rows[picked()[i]]`.
    pub fn picked(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&r| self.rows[r].picked)
            .collect()
    }

    pub fn profiles(&self) -> Vec<ProfileAnswer> {
        self.picked()
            .into_iter()
            .map(|r| self.rows[r].draft.clone())
            .collect()
    }

    pub fn row(&self, profile: usize) -> &Row {
        &self.rows[self.picked()[profile]]
    }

    fn row_mut(&mut self, profile: usize) -> &mut Row {
        let r = self.picked()[profile];
        &mut self.rows[r]
    }

    pub fn profile(&self, i: usize) -> &ProfileAnswer {
        &self.row(i).draft
    }

    fn profile_mut(&mut self, i: usize) -> &mut ProfileAnswer {
        &mut self.row_mut(i).draft
    }

    /// gh's active account.
    pub fn active(&self) -> Option<&Account> {
        self.env.accounts.as_ref().ok()?.iter().find(|a| a.active)
    }

    /// The answers with `edit` applied (for previews of an answer being
    /// typed), and the file setup would write for them.
    pub fn draft_with(
        &self,
        edit: impl FnOnce(&mut Vec<ProfileAnswer>),
    ) -> (String, Vec<ProfileAnswer>) {
        let mut profiles = self.profiles();
        edit(&mut profiles);
        let mut doc = self.env.doc.clone();
        write::apply(&mut doc, &profiles);
        (doc.to_string(), profiles)
    }

    pub fn draft(&self) -> String {
        self.draft_with(|_| {}).0
    }

    /// The config ghpr would load for profile `i`.
    pub fn config(&self, i: usize) -> color_eyre::Result<Config> {
        let here = Whereabouts {
            explicit: Some(self.profile(i).name.clone()),
            ..Whereabouts::default()
        };
        config::parse_with(&self.draft(), &here)
    }

    /// The views profile `i` can open with.
    pub fn view_names(&self, i: usize) -> Vec<String> {
        match self.config(i) {
            Ok(config) => views::names(&config),
            Err(_) => views::names(&Config::default()),
        }
    }

    pub fn plan(&self) -> Result<Plan, String> {
        let profiles = self.profiles();
        let text = self.draft();
        write::validate(&text, &profiles).map_err(|e| format!("{e:#}"))?;
        let moves = match &self.env.state_root {
            Some(root) => migrate::plan(
                &profiles,
                self.active().map(|a| a.login.as_str()),
                root,
                &*self.env.exists,
            ),
            None => Vec::new(),
        };
        let removed = self
            .existing
            .profiles
            .iter()
            .filter(|old| !profiles.iter().any(|p| p.previous == old.previous))
            .map(|old| old.name.clone())
            .collect();
        let caches = profiles
            .iter()
            .flat_map(|p| {
                self.samples
                    .iter()
                    .filter(move |((source, _), _)| *source == p.source)
                    .filter_map(|(_, slot)| slot.load.ready())
                    .map(|sample| (p.name.clone(), Sample::clone(sample)))
            })
            .collect();
        Ok(Plan {
            text,
            profiles,
            moves,
            removed,
            caches,
        })
    }

    // ---- steps ----------------------------------------------------------

    pub fn steps(&self) -> Vec<Step> {
        let mut steps = Vec::new();
        if self.intro.is_some() {
            steps.push(Step::Intro);
        }
        steps.push(Step::Accounts);
        if self.rows.last().is_some_and(|r| r.picked) {
            steps.push(Step::EnvVar);
        }
        let n = self.picked().len();
        steps.extend((0..n).map(Step::Name));
        if n > 1 {
            steps.push(Step::Default);
            for i in (0..n).filter(|&i| !self.profile(i).default) {
                steps.extend([Step::Dirs(i), Step::Email(i)]);
            }
        }
        steps.extend((0..n).map(Step::Teams));
        steps.extend((0..n).map(Step::View));
        steps.extend((0..n).map(Step::Bots));
        steps.push(Step::Review);
        steps
    }

    pub fn kind(&self) -> Kind {
        match self.step {
            Step::Intro => Kind::Select(self.intro_choices().len()),
            Step::Accounts => Kind::Multi {
                n: self.rows.len(),
                other: false,
            },
            Step::EnvVar | Step::Name(_) | Step::Dirs(_) | Step::Email(_) => Kind::Text,
            Step::Default => Kind::Select(self.picked().len()),
            Step::View(i) => Kind::Select(self.view_names(i).len()),
            Step::Teams(i) => Kind::Multi {
                n: self.row(i).team_options.len(),
                other: true,
            },
            Step::Bots(i) => Kind::Multi {
                n: self.bot_options(i).len(),
                other: true,
            },
            Step::Review => Kind::Review,
        }
    }

    pub fn intro_choices(&self) -> &'static [&'static str] {
        match self.intro {
            Some(Intro::Update { .. }) => {
                &["Set up now", "Not now", "Don't ask again for this version"]
            }
            _ => &["Set up now", "Skip"],
        }
    }

    /// Bot checkboxes: what the profile has, then suggestions from its samples.
    pub fn bot_options(&self, i: usize) -> Vec<String> {
        let mut options = self.row(i).bot_options.clone();
        for s in self.suggestions(i) {
            if !options.iter().any(|o| o.eq_ignore_ascii_case(&s.login)) {
                options.push(s.login);
            }
        }
        options
    }

    pub fn suggestions(&self, i: usize) -> Vec<super::preview::Suggestion> {
        let source = &self.profile(i).source;
        let mut samples: Vec<&Sample> = self
            .samples
            .iter()
            .filter(|((s, _), _)| s == source)
            .filter_map(|(_, slot)| slot.load.ready().map(|s| &**s))
            .collect();
        samples.sort_by(|a, b| a.view.name.cmp(&b.view.name));
        let viewer = samples.first().map_or("", |s| s.viewer.as_str());
        super::preview::bot_suggestions(samples.iter().flat_map(|s| &s.all), viewer)
    }

    /// The default view's current choice, as an index into `view_names`.
    fn view_index(&self, i: usize) -> usize {
        let names = self.view_names(i);
        let current = self.profile(i).default_view.as_deref().unwrap_or("for-me");
        names.iter().position(|n| n == current).unwrap_or(0)
    }

    fn enter(&mut self, step: Step) {
        self.step = step;
        self.error = None;
        self.warning = None;
        self.cursor = 0;
        self.list = ListState::default();
        self.scroll = 0;
        self.button = 0;
        self.other = LineInput::default();
        match step {
            Step::EnvVar => {
                let var = self
                    .rows
                    .last()
                    .and_then(|r| r.source.as_ref())
                    .and_then(Source::token_env)
                    .unwrap_or("");
                self.input = LineInput::new(var);
            }
            Step::Name(i) => self.input = LineInput::new(&self.profile(i).name),
            Step::Dirs(i) => self.input = LineInput::new(&self.profile(i).dirs.join(", ")),
            Step::Email(i) => {
                self.input = LineInput::new(self.profile(i).email.as_deref().unwrap_or(""));
            }
            Step::Default => {
                self.cursor = (0..self.picked().len())
                    .position(|i| self.profile(i).default)
                    .unwrap_or(0);
            }
            Step::View(i) => self.cursor = self.view_index(i),
            Step::Intro | Step::Accounts | Step::Teams(_) | Step::Bots(_) | Step::Review => {}
        }
    }

    fn next(&mut self) -> Vec<Cmd> {
        let cmds = match self.commit() {
            Ok(cmds) => cmds,
            Err(Stay) => return Vec::new(),
        };
        if self.outcome.is_none() {
            let steps = self.steps();
            let at = steps.iter().position(|s| *s == self.step).unwrap_or(0);
            if let Some(&next) = steps.get(at + 1) {
                self.enter(next);
            }
        }
        cmds
    }

    fn back(&mut self) {
        let steps = self.steps();
        match steps.iter().position(|s| *s == self.step) {
            Some(at) if at > 0 => self.enter(steps[at - 1]),
            _ if self.intro.is_some() => self.outcome = Some(Outcome::NotNow),
            _ => self.outcome = Some(Outcome::Cancelled),
        }
    }

    /// Take in the current step's answer. `Err` stays on the step, with
    /// `error` or `warning` saying why.
    fn commit(&mut self) -> Result<Vec<Cmd>, Stay> {
        let text = self.input.text().trim().to_owned();
        match self.step {
            Step::Intro => match self.cursor {
                0 => {}
                1 if self.intro == Some(Intro::FirstRun) => self.outcome = Some(Outcome::Skip),
                1 => self.outcome = Some(Outcome::NotNow),
                _ => self.outcome = Some(Outcome::DontAsk),
            },
            Step::Accounts => {
                if self.picked().is_empty() {
                    return self.fail("Pick at least one account (Space).");
                }
                let picked = self.picked();
                let defaults: Vec<usize> = (0..picked.len())
                    .filter(|&i| self.profile(i).default)
                    .collect();
                let default = match defaults.as_slice() {
                    [] => 0,
                    [first, ..] => *first,
                };
                for i in 0..picked.len() {
                    self.profile_mut(i).default = i == default;
                }
                return Ok(self.team_cmds());
            }
            Step::EnvVar => {
                if text.is_empty() || !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return self.fail("Type a variable name, like WORK_GITHUB_TOKEN.");
                }
                let row = self.rows.last_mut().expect("there's always the env row");
                let source = Source::Env(text);
                if row.draft.name.is_empty()
                    || Some(row.draft.name.as_str())
                        == row.source.as_ref().map(Source::suggested_name).as_deref()
                {
                    row.draft.name = source.suggested_name();
                }
                row.source = Some(source.clone());
                row.draft.source = source;
                return Ok(self.team_cmds());
            }
            Step::Name(i) => {
                if text.is_empty()
                    || !text
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                {
                    return self.fail("Use letters, digits, - and _ (it names a directory too).");
                }
                let taken =
                    (0..self.picked().len()).any(|j| j != i && self.profile(j).name == text);
                if taken {
                    return self.fail(&format!("Another profile is already called {text}."));
                }
                self.profile_mut(i).name = text;
            }
            Step::Default => {
                for i in 0..self.picked().len() {
                    self.profile_mut(i).default = i == self.cursor;
                }
            }
            Step::Dirs(i) => self.profile_mut(i).dirs = split_list(&text),
            Step::Email(i) => {
                if !text.is_empty() && !text.contains('@') {
                    return self.fail("That doesn't look like an email address.");
                }
                let profile = self.profile(i);
                if text.is_empty() && profile.dirs.is_empty() && self.warning.is_none() {
                    self.warning = Some(format!(
                        "With neither directories nor an email, {} only applies with \
                         `--profile {}` or `git config ghpr.profile {}`. Enter keeps it \
                         that way; Esc goes back to add directories.",
                        profile.name, profile.name, profile.name
                    ));
                    return Err(Stay);
                }
                self.profile_mut(i).email = (!text.is_empty()).then_some(text);
            }
            Step::Teams(i) => {
                let typed = split_list(self.other.text());
                if let Some(bad) = typed.iter().find(|t| {
                    t.split_once('/')
                        .is_none_or(|(org, slug)| org.is_empty() || slug.is_empty())
                }) {
                    return self.fail(&format!("{bad:?}: teams are written ORG/TEAM-SLUG."));
                }
                let mut cmds = Vec::new();
                for team in typed {
                    let row = self.row_mut(i);
                    if !row.team_options.contains(&team) {
                        row.team_options.push(team.clone());
                    }
                    if !row.draft.teams.contains(&team) {
                        row.draft.teams.push(team.clone());
                    }
                    cmds.extend(self.count_cmd(i, &team));
                }
                cmds.extend(self.sample_cmds(i));
                return Ok(cmds);
            }
            Step::View(i) => {
                let names = self.view_names(i);
                let name = names.get(self.cursor).cloned();
                self.profile_mut(i).default_view = name.filter(|n| n != "for-me");
            }
            Step::Bots(i) => {
                let typed = split_list(self.other.text());
                let row = self.row_mut(i);
                for bot in typed {
                    if !row.bot_options.contains(&bot) {
                        row.bot_options.push(bot.clone());
                    }
                    if !row.draft.bots.contains(&bot) {
                        row.draft.bots.push(bot);
                    }
                }
            }
            Step::Review => match self.button {
                0 => match self.plan() {
                    Ok(plan) => self.outcome = Some(Outcome::Write(Box::new(plan))),
                    Err(e) => return self.fail(&format!("Can't write this: {e}")),
                },
                1 => {
                    self.back();
                    return Err(Stay);
                }
                _ => self.outcome = Some(Outcome::Cancelled),
            },
        }
        Ok(Vec::new())
    }

    fn fail(&mut self, message: &str) -> Result<Vec<Cmd>, Stay> {
        self.error = Some(message.to_owned());
        Err(Stay)
    }

    // ---- data -----------------------------------------------------------

    /// Start listing teams for every picked account that hasn't been yet.
    fn team_cmds(&mut self) -> Vec<Cmd> {
        let mut cmds = Vec::new();
        for i in 0..self.picked().len() {
            let source = self.profile(i).source.clone();
            if source == Source::Env(String::new()) || self.teams.contains_key(&source) {
                continue;
            }
            self.teams.insert(source.clone(), Load::Loading);
            cmds.push(Cmd::Teams(source));
        }
        cmds
    }

    fn count_cmd(&mut self, i: usize, team: &str) -> Option<Cmd> {
        let source = self.profile(i).source.clone();
        let key = (source.clone(), team.to_owned());
        if self.counts.contains_key(&key) {
            return None;
        }
        self.counts.insert(key, Load::Loading);
        Some(Cmd::Count {
            source,
            team: team.to_owned(),
        })
    }

    /// Fetch a sample of each view for profile `i`, unless one with the same
    /// searches is already there.
    fn sample_cmds(&mut self, i: usize) -> Vec<Cmd> {
        let Ok(config) = self.config(i) else {
            return Vec::new();
        };
        let source = self.profile(i).source.clone();
        let viewer = Viewer {
            login: String::new(),
            bots: config.bots.clone(),
            teams: config.team_reviews.teams.clone(),
            team_mode: config.team_reviews.default_mode(),
            team_repos: config.team_reviews.repo_modes(),
        };
        let mut cmds = Vec::new();
        for name in views::names(&config) {
            let key = (source.clone(), name.clone());
            let repo = self.env.current_repos.clone();
            match views::resolve(Some(&name), &[], &config, || repo) {
                Ok(view) => {
                    let same = self
                        .samples
                        .get(&key)
                        .and_then(|s| s.view.as_ref())
                        .is_some_and(|v| v.searches == view.searches);
                    if same {
                        continue;
                    }
                    self.samples.insert(
                        key,
                        SampleSlot {
                            view: Some(view.clone()),
                            load: Load::Loading,
                        },
                    );
                    cmds.push(Cmd::Sample(Box::new(SampleRequest {
                        source: source.clone(),
                        view,
                        viewer: viewer.clone(),
                        weights: config.score.clone(),
                    })));
                }
                Err(e) => {
                    self.samples.insert(
                        key,
                        SampleSlot {
                            view: None,
                            load: Load::Failed(e.to_string()),
                        },
                    );
                }
            }
        }
        cmds
    }

    pub fn on_data(&mut self, data: Data) -> Vec<Cmd> {
        match data {
            Data::Teams { source, result } => {
                let mut cmds = Vec::new();
                if let Ok(list) = &result {
                    for i in 0..self.picked().len() {
                        if self.profile(i).source != source {
                            continue;
                        }
                        let row = self.row_mut(i);
                        for team in &list.teams {
                            if !row.team_options.contains(team) {
                                row.team_options.push(team.clone());
                            }
                        }
                        row.team_options.sort_by_key(|t| t.to_ascii_lowercase());
                        for team in self.row(i).team_options.clone() {
                            cmds.extend(self.count_cmd(i, &team));
                        }
                    }
                }
                self.teams.insert(
                    source,
                    match result {
                        Ok(list) => Load::Ready(list),
                        Err(e) => Load::Failed(e),
                    },
                );
                cmds
            }
            Data::Count {
                source,
                team,
                result,
            } => {
                self.counts.insert(
                    (source, team),
                    match result {
                        Ok(n) => Load::Ready(n),
                        Err(e) => Load::Failed(e),
                    },
                );
                Vec::new()
            }
            Data::Sample {
                source,
                view,
                result,
            } => {
                if let Some(slot) = self.samples.get_mut(&(source, view.name.clone()))
                    && slot
                        .view
                        .as_ref()
                        .is_some_and(|v| v.searches == view.searches)
                {
                    slot.load = match result {
                        Ok(sample) => Load::Ready(sample),
                        Err(e) => Load::Failed(e),
                    };
                }
                Vec::new()
            }
        }
    }

    pub fn tick(&mut self, now: Timestamp) {
        self.now = now;
    }

    // ---- keys -----------------------------------------------------------

    pub fn handle(&mut self, key: KeyEvent) -> Vec<Cmd> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.outcome = Some(Outcome::Cancelled);
            return Vec::new();
        }
        if key.code == KeyCode::Esc {
            self.back();
            return Vec::new();
        }
        match self.kind() {
            Kind::Select(n) => match key.code {
                KeyCode::Enter => return self.next(),
                code => self.cursor = moved(self.cursor, n, code, true),
            },
            Kind::Multi { n, other } => {
                let rows = n + usize::from(other);
                let on_other = other && self.cursor == n;
                match key.code {
                    KeyCode::Enter => return self.next(),
                    KeyCode::Char(' ') if !on_other => self.toggle(self.cursor),
                    KeyCode::Tab if other => self.cursor = if on_other { 0 } else { n },
                    code @ (KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown) => {
                        self.cursor = moved(self.cursor, rows, code, false);
                    }
                    code
                    @ (KeyCode::Char('j' | 'k' | 'g' | 'G') | KeyCode::Home | KeyCode::End)
                        if !on_other =>
                    {
                        self.cursor = moved(self.cursor, rows, code, true);
                    }
                    _ if on_other && self.other.handle(key) => self.error = None,
                    _ => {}
                }
            }
            Kind::Text => match key.code {
                KeyCode::Enter => return self.next(),
                _ => {
                    if self.input.handle(key) {
                        self.error = None;
                        self.warning = None;
                    }
                }
            },
            Kind::Review => match key.code {
                KeyCode::Enter => return self.next(),
                KeyCode::Char('w') => {
                    self.button = 0;
                    return self.next();
                }
                KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                    self.button = self.button.saturating_sub(1);
                }
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                    self.button = (self.button + 1).min(2);
                }
                KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(PAGE as u16),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(PAGE as u16),
                KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
                _ => {}
            },
        }
        Vec::new()
    }

    fn toggle(&mut self, at: usize) {
        self.error = None;
        match self.step {
            Step::Accounts => {
                if let Some(row) = self.rows.get_mut(at) {
                    row.picked = !row.picked;
                }
            }
            Step::Teams(i) => {
                let Some(team) = self.row(i).team_options.get(at).cloned() else {
                    return;
                };
                toggle_in(&mut self.profile_mut(i).teams, team);
            }
            Step::Bots(i) => {
                let Some(bot) = self.bot_options(i).get(at).cloned() else {
                    return;
                };
                let row = self.row_mut(i);
                if !row.bot_options.contains(&bot) {
                    row.bot_options.push(bot.clone());
                }
                toggle_in(&mut row.draft.bots, bot);
            }
            _ => {}
        }
    }
}

/// Why a step didn't advance; the message is on the wizard.
struct Stay;

impl Row {
    fn new(source: Option<Source>, account: Option<Account>, draft: ProfileAnswer) -> Row {
        Row {
            team_options: draft.teams.clone(),
            bot_options: draft.bots.clone(),
            source,
            account,
            picked: true,
            draft,
        }
    }
}

fn toggle_in(list: &mut Vec<String>, item: String) {
    match list.iter().position(|x| *x == item) {
        Some(at) => {
            list.remove(at);
        }
        None => list.push(item),
    }
}

/// The cursor after a movement key, over `n` rows. `letters` also takes
/// j/k/g/G (not while typing).
fn moved(cursor: usize, n: usize, code: KeyCode, letters: bool) -> usize {
    let last = n.saturating_sub(1);
    match code {
        KeyCode::Up => cursor.saturating_sub(1),
        KeyCode::Down => (cursor + 1).min(last),
        KeyCode::PageUp => cursor.saturating_sub(PAGE),
        KeyCode::PageDown => (cursor + PAGE).min(last),
        KeyCode::Home => 0,
        KeyCode::End => last,
        KeyCode::Char('k') if letters => cursor.saturating_sub(1),
        KeyCode::Char('j') if letters => (cursor + 1).min(last),
        KeyCode::Char('g') if letters => 0,
        KeyCode::Char('G') if letters => last,
        _ => cursor,
    }
}

/// `a, b,,c` → `[a, b, c]`
pub fn split_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::fetch::Fetched;
    use crate::test_support::{now, pr};

    pub fn account(login: &str, active: bool) -> Account {
        Account {
            login: login.to_owned(),
            active,
            scopes: Some("gist, read:org, repo".to_owned()),
        }
    }

    /// Two gh accounts (the work one active), run in ~/code/ghpr, with
    /// existing no-profile state.
    pub fn env(file: Option<&str>) -> Env {
        Env {
            path: "/home/me/.config/ghpr/config.toml".into(),
            has_file: file.is_some(),
            doc: file.map_or_else(write::new_document, |t| t.parse().unwrap()),
            accounts: Ok(vec![account("me-work", true), account("me", false)]),
            here: Whereabouts {
                cwd: "/home/me/code/ghpr".into(),
                home: Some("/home/me".into()),
                ..Whereabouts::default()
            },
            current_repos: Vec::new(),
            state_root: Some("/home/me/.local/state/ghpr".into()),
            exists: Box::new(|p| {
                p == Path::new("/home/me/.local/state/ghpr/state.json")
                    || p == Path::new("/home/me/work")
            }),
            env_set: Box::new(|var| var == "SET_TOKEN"),
        }
    }

    pub fn wizard(file: Option<&str>) -> Wizard {
        Wizard::new(env(file), None, now())
    }

    pub fn press(w: &mut Wizard, code: KeyCode) -> Vec<Cmd> {
        w.handle(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Replace the text input's contents and press Enter.
    pub fn answer(w: &mut Wizard, text: &str) -> Vec<Cmd> {
        w.handle(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        for c in text.chars() {
            press(w, KeyCode::Char(c));
        }
        press(w, KeyCode::Enter)
    }

    /// Through the accounts and names: `work` (me-work) and `personal` (me),
    /// with `personal` the default.
    pub fn named(w: &mut Wizard) {
        press(w, KeyCode::Enter);
        answer(w, "work");
        answer(w, "personal");
        assert_eq!(w.step, Step::Default);
    }

    pub fn sample(view: View, prs: Vec<crate::model::Pr>) -> Box<Sample> {
        let fetched = Fetched {
            prs,
            viewer: "me".into(),
            truncated: Vec::new(),
            stale_since: None,
        };
        Box::new(Sample::new(view, fetched, &ScoreWeights::default(), now()))
    }

    #[test]
    fn walks_through_and_writes_profiles() {
        let mut w = wizard(None);
        assert_eq!(w.step, Step::Accounts);
        let cmds = press(&mut w, KeyCode::Enter);
        let teams: Vec<_> = cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Teams(s) => Some(s.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(teams, ["me-work", "me"], "teams load while you type names");
        assert_eq!(w.step, Step::Name(0));
        assert_eq!(w.input.text(), "me-work", "the login is the suggested name");
        answer(&mut w, "work");
        answer(&mut w, "personal");

        assert_eq!(w.step, Step::Default);
        press(&mut w, KeyCode::Down);
        press(&mut w, KeyCode::Enter);
        assert_eq!(
            w.step,
            Step::Dirs(0),
            "only the non-default profile is asked"
        );
        answer(&mut w, "~/work");
        answer(&mut w, "");
        assert_eq!(w.step, Step::Teams(0));
        for _ in 0..6 {
            press(&mut w, KeyCode::Enter);
        }
        assert_eq!(w.step, Step::Review);
        press(&mut w, KeyCode::Enter);

        let Some(Outcome::Write(plan)) = w.outcome.take() else {
            panic!("expected a plan");
        };
        assert!(
            plan.text.contains(
                "[profiles.work]\ngh_user = \"me-work\"\ndirs = [\"~/work\"]\n\n\
                 [profiles.personal]\ngh_user = \"me\"\ndefault = true\n"
            ),
            "{}",
            plan.text
        );
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(
            plan.moves[0].to,
            Path::new("/home/me/.local/state/ghpr/profiles/work/state.json"),
            "the active account's profile gets the old state"
        );
    }

    #[test]
    fn single_account_is_the_default_everywhere() {
        let mut env = env(None);
        env.accounts = Ok(vec![account("me", true)]);
        let mut w = Wizard::new(env, None, now());
        press(&mut w, KeyCode::Enter);
        answer(&mut w, "personal");
        assert_eq!(w.step, Step::Teams(0), "no default or directory questions");
        assert!(w.profile(0).default);
    }

    #[test]
    fn esc_goes_back_and_ctrl_c_cancels() {
        let mut w = wizard(None);
        press(&mut w, KeyCode::Enter);
        answer(&mut w, "work");
        assert_eq!(w.step, Step::Name(1));
        press(&mut w, KeyCode::Esc);
        assert_eq!(w.step, Step::Name(0));
        assert_eq!(w.input.text(), "work", "the answer given is kept");
        press(&mut w, KeyCode::Esc);
        press(&mut w, KeyCode::Esc);
        assert!(
            matches!(w.outcome, Some(Outcome::Cancelled)),
            "back from the first step"
        );

        let mut w = wizard(None);
        w.handle(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(matches!(w.outcome, Some(Outcome::Cancelled)));
    }

    #[test]
    fn toggles_accounts_and_needs_one() {
        let mut w = wizard(None);
        press(&mut w, KeyCode::Char(' '));
        press(&mut w, KeyCode::Char('j'));
        press(&mut w, KeyCode::Char(' '));
        assert!(w.picked().is_empty());
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.step, Step::Accounts);
        assert!(w.error.as_deref().unwrap().contains("at least one"));
        press(&mut w, KeyCode::Char(' '));
        assert_eq!(w.error, None, "toggling clears the error");
        assert_eq!(w.profiles()[0].source, Source::Gh("me".into()));
    }

    #[test]
    fn environment_variable_accounts() {
        let mut env = env(None);
        env.accounts = Err("the GitHub CLI (gh) isn't installed".into());
        let mut w = Wizard::new(env, None, now());
        assert_eq!(
            w.picked().len(),
            1,
            "the env row is picked when there's nothing else"
        );
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.step, Step::EnvVar);
        answer(&mut w, "bad name");
        assert!(w.error.is_some());
        let cmds = answer(&mut w, "WORK_TOKEN");
        assert!(matches!(&cmds[..], [Cmd::Teams(Source::Env(v))] if v == "WORK_TOKEN"));
        assert_eq!(w.step, Step::Name(0));
        assert_eq!(w.input.text(), "work", "named after the variable");
    }

    #[test]
    fn validates_names() {
        let mut w = wizard(None);
        press(&mut w, KeyCode::Enter);
        answer(&mut w, "my work");
        assert!(w.error.is_some());
        press(&mut w, KeyCode::Backspace);
        assert_eq!(w.error, None, "editing clears the error");
        answer(&mut w, "work");
        answer(&mut w, "work");
        assert!(w.error.as_deref().unwrap().contains("already"));
        assert_eq!(w.step, Step::Name(1));
    }

    #[test]
    fn warns_about_unreachable_profiles() {
        let mut w = wizard(None);
        named(&mut w);
        press(&mut w, KeyCode::Down);
        press(&mut w, KeyCode::Enter);
        answer(&mut w, "");
        answer(&mut w, "");
        assert_eq!(w.step, Step::Email(0));
        assert!(w.warning.as_deref().unwrap().contains("--profile work"));
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.step, Step::Teams(0), "a second Enter goes on");
    }

    #[test]
    fn teams_arrive_get_counted_and_toggle() {
        let mut w = wizard(Some(
            "[team_reviews]\nteams = [\"acme/old\"]\n[profiles.p]\ngh_user = \"me-work\"\ndefault = true\n",
        ));
        assert_eq!(w.profile(0).name, "p", "named as in the file");
        assert_eq!(w.profile(0).teams, ["acme/old"], "top-level teams prefill");
        press(&mut w, KeyCode::Char('j'));
        press(&mut w, KeyCode::Char(' '));
        press(&mut w, KeyCode::Enter);
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.step, Step::Teams(0));

        let cmds = w.on_data(Data::Teams {
            source: Source::Gh("me-work".into()),
            result: Ok(TeamList {
                teams: vec!["acme/robotics".into()],
                notes: vec![],
            }),
        });
        assert_eq!(
            cmds.iter()
                .filter(|c| matches!(c, Cmd::Count { .. }))
                .count(),
            2
        );
        assert_eq!(w.row(0).team_options, ["acme/old", "acme/robotics"]);
        press(&mut w, KeyCode::Char(' '));
        press(&mut w, KeyCode::Down);
        press(&mut w, KeyCode::Char(' '));
        assert_eq!(w.profile(0).teams, ["acme/robotics"]);
        assert_eq!(w.row(0).team_options.len(), 2, "unchecked ones stay listed");

        press(&mut w, KeyCode::Tab);
        for c in "nope".chars() {
            press(&mut w, KeyCode::Char(c));
        }
        press(&mut w, KeyCode::Enter);
        assert!(w.error.as_deref().unwrap().contains("ORG/TEAM-SLUG"));
        answer(&mut w, "acme/new");
        assert_eq!(w.profile(0).teams, ["acme/robotics", "acme/new"]);
        assert_eq!(w.step, Step::View(0));
    }

    #[test]
    fn leaving_teams_fetches_samples_once() {
        let mut env = env(None);
        env.accounts = Ok(vec![account("me", true)]);
        let mut w = Wizard::new(env, None, now());
        press(&mut w, KeyCode::Enter);
        answer(&mut w, "personal");
        let cmds = press(&mut w, KeyCode::Enter);
        let views: Vec<_> = cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Sample(r) => Some(r.view.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(views, ["for-me", "mine"], "repo isn't resolvable here");
        let repo = &w.samples[&(Source::Gh("me".into()), "repo".to_owned())];
        assert!(repo.view.is_none());

        press(&mut w, KeyCode::Esc);
        assert!(press(&mut w, KeyCode::Enter).is_empty(), "nothing changed");

        // A late result for searches that are no longer wanted is dropped.
        let mut stale =
            crate::views::resolve(Some("mine"), &[], &Config::default(), Vec::new).unwrap();
        stale.searches[0].query.push_str(" old");
        w.on_data(Data::Sample {
            source: Source::Gh("me".into()),
            view: stale.clone(),
            result: Ok(sample(stale, vec![pr(1)])),
        });
        let mine = &w.samples[&(Source::Gh("me".into()), "mine".to_owned())];
        assert_eq!(mine.load.ready().map(|_| ()), None);
    }

    #[test]
    fn picks_default_view_and_bots() {
        let mut env = env(None);
        env.accounts = Ok(vec![account("me", true)]);
        let mut w = Wizard::new(env, None, now());
        press(&mut w, KeyCode::Enter);
        answer(&mut w, "personal");
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.step, Step::View(0));
        let view = w.samples[&(Source::Gh("me".into()), "for-me".to_owned())]
            .view
            .clone()
            .unwrap();
        let mut by_bot = pr(5);
        by_bot.author = Some("acme-ci".into());
        w.on_data(Data::Sample {
            source: Source::Gh("me".into()),
            view: view.clone(),
            result: Ok(sample(view, vec![pr(1), by_bot])),
        });
        press(&mut w, KeyCode::Down);
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.profile(0).default_view.as_deref(), Some("mine"));

        assert_eq!(w.step, Step::Bots(0));
        assert_eq!(w.bot_options(0), ["acme-ci"]);
        press(&mut w, KeyCode::Char(' '));
        press(&mut w, KeyCode::Down);
        for c in "deployer".chars() {
            press(&mut w, KeyCode::Char(c));
        }
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.profile(0).bots, ["acme-ci", "deployer"]);
        assert_eq!(w.step, Step::Review);
    }

    #[test]
    fn intro_choices() {
        let mut w = Wizard::new(env(None), Some(Intro::FirstRun), now());
        assert_eq!(w.step, Step::Intro);
        press(&mut w, KeyCode::Down);
        press(&mut w, KeyCode::Enter);
        assert!(matches!(w.outcome, Some(Outcome::Skip)));

        let mut w = Wizard::new(env(Some("")), Some(Intro::Update { from: 0 }), now());
        press(&mut w, KeyCode::Char('G'));
        press(&mut w, KeyCode::Enter);
        assert!(matches!(w.outcome, Some(Outcome::DontAsk)));

        let mut w = Wizard::new(env(None), Some(Intro::FirstRun), now());
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.step, Step::Accounts);
        press(&mut w, KeyCode::Esc);
        press(&mut w, KeyCode::Esc);
        assert!(
            matches!(w.outcome, Some(Outcome::NotNow)),
            "Esc on the intro"
        );
    }

    #[test]
    fn rerun_keeps_unlisted_profiles_and_removes_unchecked_ones() {
        let file = "[profiles.ci]\ntoken_env = \"CI_TOKEN\"\n[profiles.old]\ngh_user = \"gone\"\ndefault = true\n";
        let mut w = wizard(Some(file));
        let labels: Vec<String> = w
            .rows
            .iter()
            .map(|r| r.source.as_ref().map_or("env".into(), Source::to_string))
            .collect();
        assert_eq!(labels, ["me-work", "me", "$CI_TOKEN", "gone", "env"]);
        // Uncheck "gone".
        for _ in 0..3 {
            press(&mut w, KeyCode::Down);
        }
        press(&mut w, KeyCode::Char(' '));
        let plan = w.plan().unwrap();
        assert_eq!(plan.removed, ["old"]);
        assert!(
            plan.text
                .contains("[profiles.ci]\ntoken_env = \"CI_TOKEN\"")
        );
    }
}
