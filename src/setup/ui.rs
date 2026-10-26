//! Drawing the setup wizard: the step's question and choices, a preview of
//! what the highlighted choice does, and the keys.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::CHANGES;
use super::preview::{describe, missing_dirs, routes, tilde};
use super::wizard::{Intro, Kind, Load, STEPS, Step, Wizard};
use super::write::{ProfileAnswer, Source};
use crate::output;

/// Below this width the preview goes under the choices instead of beside them.
const SIDE_BY_SIDE: u16 = 100;
/// Rows in a view's sample.
const SAMPLE_ROWS: usize = 10;

pub fn render(frame: &mut Frame, w: &mut Wizard) {
    let [title, body, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    render_title(frame, w, title);

    let [choices, preview] = if body.width >= SIDE_BY_SIDE {
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
            .spacing(1)
            .areas(body)
    } else {
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body)
    };
    render_choices(frame, w, choices);
    render_preview(frame, w, preview);
    frame.render_widget(Line::from(hint(w).dark_gray()), hints);
}

fn render_title(frame: &mut Frame, w: &Wizard, area: Rect) {
    let mut spans = vec![" ghpr setup ".bold().reversed(), Span::raw(" ")];
    match w.step.title() {
        Some((n, title)) => spans.push(format!("{n}/{STEPS} {title}").bold()),
        None => spans.push("Welcome".bold()),
    }
    if let Some(i) = w.step.profile() {
        spans.push(" · ".dark_gray());
        spans.push(w.profile(i).name.clone().cyan());
    }
    frame.render_widget(Line::from(spans), area);
}

fn hint(w: &Wizard) -> String {
    let keys = match w.kind() {
        _ if w.step == Step::Intro => "↑↓ choose · Enter confirm · Esc not now · Ctrl-C quit",
        Kind::Select(_) => "↑↓ choose · Enter next · Esc back · Ctrl-C cancel",
        Kind::Multi { n, other: true } if w.cursor == n => {
            "typing · ↑ back to the list · Enter next · Esc back · Ctrl-C cancel"
        }
        Kind::Multi { other: true, .. } => {
            "↑↓ move · Space toggle · Tab type others · Enter next · Esc back · Ctrl-C cancel"
        }
        Kind::Multi { .. } => "↑↓ move · Space toggle · Enter next · Esc back · Ctrl-C cancel",
        Kind::Text => "Enter next · Esc back · Ctrl-C cancel",
        Kind::Review => "↑↓ PgUp PgDn scroll · ←→ choose · Enter confirm · Esc back",
    };
    format!(" {keys}")
}

// ---- choices ------------------------------------------------------------

fn render_choices(frame: &mut Frame, w: &mut Wizard, area: Rect) {
    let width = area.width as usize;
    let (question, help) = question(w);
    let mut head: Vec<Line> = vec![Line::from(question.bold())];
    for text in help {
        head.extend(wrap(&text, width, Style::new().dark_gray()));
    }
    head.push(Line::default());

    let mut foot: Vec<Line> = Vec::new();
    if let Some(error) = &w.error {
        foot.push(Line::default());
        foot.extend(wrap(error, width, Style::new().red()));
    } else if let Some(warning) = &w.warning {
        foot.push(Line::default());
        foot.extend(wrap(warning, width, Style::new().yellow()));
    }

    let [head_area, body, foot_area] = Layout::vertical([
        Constraint::Length(head.len() as u16),
        Constraint::Fill(1),
        Constraint::Length(foot.len() as u16),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(head), head_area);
    frame.render_widget(Paragraph::new(foot), foot_area);

    match w.kind() {
        Kind::Select(_) => {
            let items: Vec<ListItem> = select_items(w).into_iter().map(ListItem::new).collect();
            render_list(frame, w, items, body);
        }
        Kind::Multi { n, other } => {
            let mut items: Vec<ListItem> = multi_items(w)
                .into_iter()
                .map(|(checked, label)| {
                    let mut spans = vec![Span::raw(if checked { "[x] " } else { "[ ] " })];
                    spans.extend(label.spans);
                    ListItem::new(Line::from(spans))
                })
                .collect();
            if other {
                let label = match w.step {
                    Step::Teams(_) => "Others (org/team, …): ",
                    _ => "Others (login, …): ",
                };
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(label),
                    Span::raw(w.other.text().to_owned()).underlined(),
                ])));
            }
            render_list(frame, w, items, body);
            if other && w.cursor == n {
                let label = if matches!(w.step, Step::Teams(_)) {
                    24
                } else {
                    20
                };
                let before: String = w.other.text().chars().take(w.other.cursor()).collect();
                let row = w.cursor.saturating_sub(w.list.offset()) as u16;
                if row < body.height {
                    frame.set_cursor_position((
                        body.x + 2 + label + before.width() as u16,
                        body.y + row,
                    ));
                }
            }
        }
        Kind::Text => {
            let text = w.input.text();
            frame.render_widget(
                Paragraph::new(Line::from(vec!["› ".cyan(), Span::raw(text.to_owned())])),
                body,
            );
            let before: String = text.chars().take(w.input.cursor()).collect();
            frame.set_cursor_position((body.x + 2 + before.width() as u16, body.y));
        }
        Kind::Review => {
            let mut lines: Vec<Line> = review_summary(w, width);
            lines.push(Line::default());
            let buttons = ["Write", "Back", "Cancel"];
            let mut spans = Vec::new();
            for (i, label) in buttons.iter().enumerate() {
                let text = format!(" {label} ");
                spans.push(if i == w.button {
                    text.bold().reversed()
                } else {
                    Span::raw(text)
                });
                spans.push(Span::raw("  "));
            }
            lines.push(Line::from(spans));
            frame.render_widget(Paragraph::new(lines), body);
        }
    }
}

fn render_list(frame: &mut Frame, w: &mut Wizard, items: Vec<ListItem>, area: Rect) {
    w.list.select(Some(w.cursor));
    let list = List::new(items)
        .highlight_symbol("› ")
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always)
        .highlight_style(Style::new().add_modifier(Modifier::BOLD).fg(Color::Cyan));
    frame.render_stateful_widget(list, area, &mut w.list);
}

/// The question and a few lines on what it's for.
fn question(w: &Wizard) -> (String, Vec<String>) {
    let name = |i: usize| w.profile(i).name.clone();
    match w.step {
        Step::Intro => intro_question(w),
        Step::Accounts => (
            "Which GitHub accounts should ghpr use?".into(),
            vec![
                "Each gets a profile with its own token, settings, and seen/snoozed state.".into(),
            ],
        ),
        Step::EnvVar => (
            "Which environment variable holds the token?".into(),
            vec!["For an account gh doesn't manage. ghpr reads it each time it starts.".into()],
        ),
        Step::Name(i) => (
            format!(
                "What should the profile for {} be called?",
                w.profile(i).source
            ),
            vec!["It shows in ghpr's header, and `--profile NAME` picks it by hand.".into()],
        ),
        Step::Default => (
            "Which profile applies when no other one does?".into(),
            vec!["The others apply by directory, or by a repo's git user.email.".into()],
        ),
        Step::Dirs(i) => (
            format!("Which directories is {} for?", name(i)),
            vec![format!(
                "Comma-separated; ~ is your home. ghpr uses {} anywhere under them.",
                name(i)
            )],
        ),
        Step::Email(i) => (
            format!("Which git user.email is {} for? (optional)", name(i)),
            vec![format!(
                "Repos whose git user.email matches use {}, wherever they are.",
                name(i)
            )],
        ),
        Step::Teams(i) => (
            format!("Whose team review requests should {} show?", name(i)),
            vec!["Requests to these teams show up in for-me as turn `team`.".into()],
        ),
        Step::View(i) => (
            format!("Which view should {} open with?", name(i)),
            vec!["The others are a tab away ([ and ]).".into()],
        ),
        Step::Bots(i) => (
            format!("Which accounts are bots, for {}?", name(i)),
            vec!["User accounts run by automation. GitHub App bots are recognized already.".into()],
        ),
        Step::Review => (
            "Write this config?".into(),
            vec!["↑↓ scroll the file; Back changes an answer.".into()],
        ),
    }
}

fn intro_question(w: &Wizard) -> (String, Vec<String>) {
    let mut help = Vec::new();
    let question = match w.intro {
        Some(Intro::Update { from }) => {
            help.push(if from == 0 {
                "Your config was written before ghpr had a guided setup. New since then:".into()
            } else {
                format!("Your config was written by setup version {from}. New since then:")
            });
            help.extend(
                CHANGES
                    .iter()
                    .filter(|(v, _)| *v > from)
                    .map(|(_, change)| format!("  • {change}")),
            );
            "Setup has new questions"
        }
        _ => {
            help.push(format!(
                "There's no config file yet ({}).",
                tilde(&w.env.path, w.env.here.home.as_deref())
            ));
            match &w.env.accounts {
                Ok(accounts) if accounts.len() > 1 => {
                    let active = accounts.iter().find(|a| a.active);
                    let logins: Vec<&str> = accounts.iter().map(|a| a.login.as_str()).collect();
                    help.push(format!(
                        "gh is logged in to {} GitHub accounts ({}).{}",
                        accounts.len(),
                        logins.join(", "),
                        active.map_or(String::new(), |a| format!(
                            " Without setup, ghpr uses gh's active one, {}, everywhere.",
                            a.login
                        ))
                    ));
                }
                Ok(accounts) if accounts.len() == 1 => help.push(format!(
                    "gh is logged in as {}. Setup pins that account, so switching gh's active \
                     account later doesn't switch ghpr's.",
                    accounts[0].login
                )),
                Ok(_) => help.push(
                    "gh isn't logged in to github.com. Run `gh auth login` first, or use a \
                     token from an environment variable."
                        .into(),
                ),
                Err(e) => help.push(format!(
                    "{e}. Install it and run `gh auth login`, or use a token from an \
                     environment variable."
                )),
            }
            "Set up ghpr?"
        }
    };
    (question.into(), help)
}

fn select_items(w: &Wizard) -> Vec<Line<'static>> {
    match w.step {
        Step::Intro => w.intro_choices().iter().map(|c| Line::from(*c)).collect(),
        Step::Default => (0..w.picked().len())
            .map(|i| {
                let p = w.profile(i);
                Line::from(vec![
                    Span::raw(p.name.clone()),
                    format!(" · {}", p.source).dark_gray(),
                ])
            })
            .collect(),
        Step::View(i) => {
            let custom = w.config(i).map(|c| c.views).unwrap_or_default();
            w.view_names(i)
                .into_iter()
                .map(|name| {
                    let note = match name.as_str() {
                        _ if custom.contains_key(&name) => " · from your config",
                        "for-me" => " · what needs you",
                        "mine" => " · your PRs",
                        "repo" => " · the current repo",
                        "org" => " · its organization",
                        _ => "",
                    };
                    Line::from(vec![Span::raw(name), note.dark_gray()])
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn multi_items(w: &Wizard) -> Vec<(bool, Line<'static>)> {
    match w.step {
        Step::Accounts => w
            .rows
            .iter()
            .map(|row| {
                let mut spans = Vec::new();
                match (&row.source, &row.account) {
                    (Some(source), Some(account)) => {
                        spans.push(Span::raw(source.to_string()));
                        if account.active {
                            spans.push(" · gh's active account".dark_gray());
                        }
                    }
                    (Some(Source::Gh(login)), None) => {
                        spans.push(Span::raw(login.clone()));
                        spans.push(" · not logged in to gh".yellow());
                    }
                    (Some(Source::Env(var)), None) if !var.is_empty() => {
                        spans.push(Span::raw(format!("${var}")));
                    }
                    (Some(Source::Unpinned(_)), None) => {
                        spans.push(Span::raw("GH_TOKEN, or gh's active account"));
                    }
                    _ => spans.push(Span::raw("A token from an environment variable…")),
                }
                if let Some(previous) = &row.draft.previous {
                    spans.push(format!(" · profile {previous}").dark_gray());
                }
                (row.picked, Line::from(spans))
            })
            .collect(),
        Step::Teams(i) => {
            let profile = w.profile(i);
            w.row(i)
                .team_options
                .iter()
                .map(|team| {
                    let count = match w.counts.get(&(profile.source.clone(), team.clone())) {
                        Some(Load::Ready(1)) => " · 1 open request".to_owned(),
                        Some(Load::Ready(n)) => format!(" · {n} open requests"),
                        Some(Load::Loading) => " · …".to_owned(),
                        Some(Load::Failed(_)) => " · ?".to_owned(),
                        None => String::new(),
                    };
                    (
                        profile.teams.contains(team),
                        Line::from(vec![Span::raw(team.clone()), count.dark_gray()]),
                    )
                })
                .collect()
        }
        Step::Bots(i) => {
            let profile = w.profile(i);
            let suggestions = w.suggestions(i);
            w.bot_options(i)
                .into_iter()
                .map(|bot| {
                    let seen = suggestions
                        .iter()
                        .find(|s| s.login == bot)
                        .map(|s| format!(" · {}", s.seen_on))
                        .unwrap_or_default();
                    (
                        profile.bots.contains(&bot),
                        Line::from(vec![Span::raw(bot), seen.dark_gray()]),
                    )
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn review_summary(w: &Wizard, width: usize) -> Vec<Line<'static>> {
    let home = w.env.here.home.as_deref();
    let mut lines = Vec::new();
    let mut add = |text: String, style: Style| lines.extend(wrap(&text, width, style));
    add(
        format!("Writes {}.", tilde(&w.env.path, home)),
        Style::new(),
    );
    if w.env.has_file {
        add(
            "Backs up the current file first, next to it (config.toml.bak-<time>).".into(),
            Style::new(),
        );
    }
    match w.plan() {
        Ok(plan) => {
            if !plan.removed.is_empty() {
                add(
                    format!("Removes profile {}.", plan.removed.join(", ")),
                    Style::new().yellow(),
                );
            }
            for m in &plan.moves {
                add(
                    format!(
                        "Moves seen/snoozed/muted state {} → {} ({}).",
                        tilde(&m.from, home),
                        tilde(&m.to, home),
                        m.why
                    ),
                    Style::new(),
                );
            }
        }
        Err(e) => add(format!("Can't write this: {e}"), Style::new().red()),
    }
    lines
}

// ---- preview ------------------------------------------------------------

fn render_preview(frame: &mut Frame, w: &Wizard, area: Rect) {
    let title = if w.step == Step::Review {
        format!(
            " {} ",
            w.env
                .path
                .file_name()
                .map_or("config.toml".into(), |n| n.to_string_lossy())
        )
    } else {
        " Preview ".to_owned()
    };
    let block = Block::bordered()
        .title(title)
        .border_style(Style::new().dark_gray());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = preview_lines(w, inner.width as usize);
    let scroll = if w.step == Step::Review {
        w.scroll
            .min(lines.len().saturating_sub(inner.height as usize) as u16)
    } else {
        0
    };
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
}

fn preview_lines(w: &Wizard, width: usize) -> Vec<Line<'static>> {
    let mut out = Out::new(width);
    match w.step {
        Step::Intro => intro_preview(w, &mut out),
        Step::Accounts => accounts_preview(w, &mut out),
        Step::EnvVar => {
            let var = w.input.text().trim();
            if var.is_empty() {
                out.text("Like WORK_GITHUB_TOKEN.", Style::new().dark_gray());
            } else if (w.env.env_set)(var) {
                out.text(
                    format!("${var} is set in this shell."),
                    Style::new().green(),
                );
            } else {
                out.text(
                    format!("${var} isn't set in this shell. Set it wherever you run ghpr."),
                    Style::new().yellow(),
                );
            }
            out.blank();
            out.text(
                "The config stores the variable's name, never the token.",
                Style::new(),
            );
        }
        Step::Name(i) => name_preview(w, i, &mut out),
        Step::Default => {
            let cursor = w.cursor;
            let draft = w.draft_with(|profiles| {
                for (j, p) in profiles.iter_mut().enumerate() {
                    p.default = j == cursor;
                }
            });
            routes_preview(w, draft, None, &mut out);
        }
        Step::Dirs(i) => {
            let dirs = super::wizard::split_list(w.input.text());
            let draft = w.draft_with(|profiles| profiles[i].dirs = dirs.clone());
            routes_preview(w, draft, Some(&dirs), &mut out);
        }
        Step::Email(i) => {
            let email = w.input.text().trim().to_owned();
            let draft = w.draft_with(|profiles| {
                profiles[i].email = (!email.is_empty()).then_some(email.clone());
            });
            routes_preview(w, draft, None, &mut out);
        }
        Step::Teams(i) => teams_preview(w, i, &mut out),
        Step::View(i) => view_preview(w, i, &mut out),
        Step::Bots(i) => bots_preview(w, i, &mut out),
        Step::Review => {
            for line in w.draft().lines() {
                let style = if line.trim_start().starts_with('#') {
                    Style::new().dark_gray()
                } else if line.starts_with('[') {
                    Style::new().cyan().bold()
                } else {
                    Style::new()
                };
                out.line(Line::styled(line.to_owned(), style));
            }
        }
    }
    out.lines
}

fn intro_preview(w: &Wizard, out: &mut Out) {
    if let Some(Intro::Update { .. }) = w.intro {
        out.text(
            "Setup starts from your current values, and backs up the file before writing. \
             Comments, views, and anything else you added stay as they are.",
            Style::new(),
        );
        out.blank();
        out.text(
            "Not now asks again next time. Don't ask again waits for the next setup update. \
             `ghpr config init` runs setup any time.",
            Style::new().dark_gray(),
        );
        return;
    }
    out.text("Setup asks:", Style::new().bold());
    for item in [
        "which accounts to use, with a profile for each",
        "where each profile applies (directories, git email)",
        "whose team review requests to include",
        "which view to open with",
        "which accounts are bots",
    ] {
        out.text(format!("  • {item}"), Style::new());
    }
    out.blank();
    out.text(
        "Nothing is written until you confirm. Skip writes a short starter config so this \
         doesn't come up again; `ghpr config init` runs setup any time.",
        Style::new().dark_gray(),
    );
}

fn accounts_preview(w: &Wizard, out: &mut Out) {
    if let Err(e) = &w.env.accounts {
        out.text(format!("{e}."), Style::new().red());
        out.blank();
    }
    if let Some(row) = w.rows.get(w.cursor) {
        match (&row.source, &row.account) {
            (Some(Source::Gh(login)), Some(account)) => {
                out.line(Line::from(login.clone().bold()));
                out.text(
                    format!("Token from `gh auth token --user {login}`."),
                    Style::new(),
                );
                if let Some(scopes) = &account.scopes {
                    out.text(format!("Scopes: {scopes}."), Style::new().dark_gray());
                }
                if account.active {
                    out.text(
                        "gh's active account: what ghpr uses everywhere today.",
                        Style::new(),
                    );
                }
                if !account.can_read_org() {
                    out.text(
                        format!(
                            "No read:org scope, so its teams can't be listed. \
                             `gh auth refresh -s read:org` (as {login}) adds it."
                        ),
                        Style::new().yellow(),
                    );
                }
            }
            (Some(Source::Gh(login)), None) => out.text(
                format!(
                    "gh has no token for {login}, so this profile can't fetch anything \
                     until you `gh auth login` as {login}."
                ),
                Style::new().yellow(),
            ),
            (Some(Source::Unpinned(_)), _) => out.text(
                "This profile doesn't pin an account: it takes GH_TOKEN, GITHUB_TOKEN, or \
                 gh's active account.",
                Style::new(),
            ),
            (Some(Source::Env(var)), _) if !var.is_empty() => out.text(
                format!("Token from ${var}, read each time ghpr starts."),
                Style::new(),
            ),
            _ => out.text(
                "Reads the token from an environment variable each time ghpr starts: for \
                 an account gh doesn't manage, or a token with other scopes.",
                Style::new(),
            ),
        }
        if let Some(previous) = &row.draft.previous {
            out.text(
                format!("In your config as profile {previous}."),
                Style::new().dark_gray(),
            );
        }
    }
    out.blank();
    let picked = w.picked().len();
    match picked {
        0 => out.text("Nothing selected yet.", Style::new().yellow()),
        1 => out.text(
            "1 selected → 1 profile, used everywhere. Its token is pinned, whichever gh \
             account is active.",
            Style::new(),
        ),
        n => out.text(
            format!(
                "{n} selected → {n} profiles. Next you name them and choose where each \
                 applies; everything after that is asked per profile."
            ),
            Style::new(),
        ),
    }
    let dropped: Vec<&str> = w
        .rows
        .iter()
        .filter(|r| !r.picked)
        .filter_map(|r| r.draft.previous.as_deref())
        .collect();
    if !dropped.is_empty() {
        out.text(
            format!(
                "Unchecked, so removed from the config: {}.",
                dropped.join(", ")
            ),
            Style::new().yellow(),
        );
    }
}

fn name_preview(w: &Wizard, i: usize, out: &mut Out) {
    let typed = w.input.text().trim();
    let profile = w.profile(i);
    let home = w.env.here.home.as_deref();
    out.text(format!("The profile for {}.", profile.source), Style::new());
    if let Some(old) = profile
        .previous
        .as_deref()
        .filter(|old| *old != typed && !typed.is_empty())
    {
        out.text(
            format!(
                "Renames profile {old} to {typed}: its seen/snoozed/muted state moves along; \
                 its cache is fetched again."
            ),
            Style::new().yellow(),
        );
    }
    if let Some(root) = &w.env.state_root
        && !typed.is_empty()
    {
        out.blank();
        out.text(
            format!(
                "State: {}/",
                tilde(&root.join("profiles").join(typed), home)
            ),
            Style::new().dark_gray(),
        );
        out.text(
            format!("`ghpr --profile {typed}` uses it anywhere."),
            Style::new().dark_gray(),
        );
    }
}

fn routes_preview(
    w: &Wizard,
    (text, profiles): (String, Vec<ProfileAnswer>),
    dirs: Option<&[String]>,
    out: &mut Out,
) {
    out.text("Where each profile applies:", Style::new().bold());
    let home = w.env.here.home.as_deref();
    for route in routes(&text, &profiles, &w.env.here) {
        let mut spans = vec![Span::raw(format!("  {} → ", route.place))];
        match route.outcome {
            Ok(Some((name, why))) => {
                spans.push(name.cyan().bold());
                spans.push(format!(" ({why})").dark_gray());
            }
            Ok(None) => spans.push("no profile (GH_TOKEN or gh's active account)".yellow()),
            Err(e) => spans.push(e.red()),
        }
        out.wrapped(spans);
    }
    if let Some(dirs) = dirs {
        let missing = missing_dirs(dirs, home, &*w.env.exists);
        if !missing.is_empty() {
            out.blank();
            out.text(
                format!("Doesn't exist (yet): {}.", missing.join(", ")),
                Style::new().yellow(),
            );
        }
    }
    out.blank();
    out.text(
        "First match wins: --profile, git config ghpr.profile, directories (longest), \
         email, then the default.",
        Style::new().dark_gray(),
    );
}

fn teams_preview(w: &Wizard, i: usize, out: &mut Out) {
    let profile = w.profile(i);
    match w.teams.get(&profile.source) {
        Some(Load::Loading) => out.text(
            format!("{} Listing your teams…", spinner(w.now)),
            Style::new().cyan(),
        ),
        Some(Load::Failed(e)) => {
            out.text(
                format!("Couldn't list your teams: {}", super::teams::short(e)),
                Style::new().yellow(),
            );
            out.text(
                "Type them under Others as ORG/TEAM-SLUG, comma-separated.",
                Style::new(),
            );
        }
        Some(Load::Ready(list)) => {
            match list.teams.len() {
                0 => out.text("You're not on any team this token can see.", Style::new()),
                1 => out.text("You're on 1 team.", Style::new()),
                n => out.text(format!("You're on {n} teams."), Style::new()),
            }
            for note in &list.notes {
                out.text(format!("Skipped {note}."), Style::new().yellow());
            }
        }
        None => {}
    }
    out.blank();
    if let Some(team) = w.row(i).team_options.get(w.cursor) {
        match w.counts.get(&(profile.source.clone(), team.clone())) {
            Some(Load::Ready(n)) => out.text(
                format!(
                    "{team}: {n} open PR{} waiting on a review from the team.",
                    if *n == 1 { "" } else { "s" }
                ),
                Style::new(),
            ),
            Some(Load::Failed(e)) => out.text(
                format!(
                    "{team}: couldn't count its requests ({})",
                    super::teams::short(e)
                ),
                Style::new().yellow(),
            ),
            _ => {}
        }
    }
    out.text(
        "Turn `team` ranks below requests to you; `count_as_mine = true` under \
         [team_reviews] makes them yours.",
        Style::new().dark_gray(),
    );
}

fn view_preview(w: &Wizard, i: usize, out: &mut Out) {
    let names = w.view_names(i);
    let Some(name) = names.get(w.cursor) else {
        return;
    };
    let profile = w.profile(i);
    let teams = profile.teams.len();
    let slot = w.samples.get(&(profile.source.clone(), name.clone()));
    let config = w.config(i).ok();
    let repo = w.env.current_repos.clone();
    let view = slot.and_then(|s| s.view.clone()).or_else(|| {
        let config = config.as_ref()?;
        crate::views::resolve(Some(name), &[], config, || repo).ok()
    });
    let Some(view) = view else {
        let what = if name == "org" {
            "Opens the organization of the repo you run ghpr in"
        } else {
            "Opens the repo of the directory you run ghpr in"
        };
        out.text(format!("{what}; you're not in one now."), Style::new());
        return;
    };
    let about = describe(&view, if name == "for-me" { teams } else { 0 });
    match slot.map(|s| &s.load) {
        Some(Load::Ready(sample)) => {
            let n = sample.shown.len();
            let count = if sample.truncated {
                format!("{n}+ PRs")
            } else if n == 1 {
                "1 PR".to_owned()
            } else {
                format!("{n} PRs")
            };
            out.line(Line::from(vec![
                name.clone().bold(),
                format!(" · {count}").dark_gray(),
            ]));
            out.text(about, Style::new().dark_gray());
            out.blank();
            if sample.shown.is_empty() {
                out.text("Nothing in this view right now.", Style::new().dark_gray());
            } else {
                let (columns, weights) = config
                    .map(|c| (c.columns, c.score))
                    .unwrap_or_else(|| (crate::columns::DEFAULT.to_vec(), Default::default()));
                let rows: Vec<_> = sample.shown.iter().take(SAMPLE_ROWS).cloned().collect();
                let table = output::table(&rows, &columns, &weights, sample.at, Some(out.width));
                for (j, line) in table.lines().enumerate() {
                    let style = if j == 0 {
                        Style::new().bold()
                    } else {
                        Style::new()
                    };
                    out.line(Line::styled(line.to_owned(), style));
                }
                if n > SAMPLE_ROWS {
                    out.text(
                        format!("… and {} more", n - SAMPLE_ROWS),
                        Style::new().dark_gray(),
                    );
                }
            }
        }
        Some(Load::Failed(e)) => {
            out.line(Line::from(name.clone().bold()));
            out.text(
                format!("Couldn't fetch a sample: {}", one_line(e)),
                Style::new().yellow(),
            );
            out.text(about, Style::new().dark_gray());
        }
        Some(Load::Loading) | None => {
            out.line(Line::from(vec![
                name.clone().bold(),
                format!(" {} fetching a sample…", spinner(w.now)).cyan(),
            ]));
            out.text(about, Style::new().dark_gray());
        }
    }
}

fn bots_preview(w: &Wizard, i: usize, out: &mut Out) {
    let profile = w.profile(i);
    let options = w.bot_options(i);
    match options.get(w.cursor) {
        Some(bot) => {
            out.line(Line::from(bot.clone().bold()));
            if let Some(s) = w.suggestions(i).into_iter().find(|s| s.login == *bot) {
                out.text(format!("Seen on {}.", s.seen_on), Style::new().dark_gray());
            }
            out.text(
                format!(
                    "Marked as a bot, comments and reviews from {bot} no longer change \
                     whose turn it is."
                ),
                Style::new(),
            );
        }
        None => out.text(
            "Type logins, comma-separated. Comments and reviews from bots don't change \
             whose turn it is.",
            Style::new(),
        ),
    }
    out.blank();
    let loading = w
        .samples
        .iter()
        .any(|((s, _), slot)| *s == profile.source && matches!(slot.load, Load::Loading));
    if loading {
        out.text(
            format!("{} Looking for likely bots in your PRs…", spinner(w.now)),
            Style::new().cyan(),
        );
    } else if w.suggestions(i).is_empty() {
        out.text(
            "No likely bots among the authors and latest commenters of your PRs.",
            Style::new().dark_gray(),
        );
    } else {
        out.text(
            "Suggestions are user accounts in your PRs whose names look automated.",
            Style::new().dark_gray(),
        );
    }
    out.text(
        "GitHub App bots (like dependabot[bot]) are recognized without this.",
        Style::new().dark_gray(),
    );
}

// ---- helpers ------------------------------------------------------------

/// Preview lines, wrapped to the pane.
struct Out {
    width: usize,
    lines: Vec<Line<'static>>,
}

impl Out {
    fn new(width: usize) -> Self {
        Out {
            width,
            lines: Vec::new(),
        }
    }

    fn text(&mut self, text: impl Into<String>, style: Style) {
        self.lines.extend(wrap(&text.into(), self.width, style));
    }

    /// Spans wrapped as one paragraph, keeping each span's style.
    fn wrapped(&mut self, spans: Vec<Span<'static>>) {
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        if text.width() <= self.width {
            self.lines.push(Line::from(spans));
        } else {
            let style = spans.last().map_or(Style::new(), |s| s.style);
            self.text(text, style);
        }
    }

    fn line(&mut self, line: Line<'static>) {
        self.lines.push(line);
    }

    fn blank(&mut self) {
        self.lines.push(Line::default());
    }
}

/// Greedy word wrap, keeping leading spaces. Continuation lines line up
/// after a leading `  • `.
fn wrap(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let width = width.max(10);
    let rest = text.trim_start_matches(' ');
    let lead = " ".repeat(text.len() - rest.len());
    let hang = " ".repeat(lead.len() + if rest.starts_with("• ") { 2 } else { 0 });
    let mut lines = Vec::new();
    let mut line = lead;
    let mut empty = true;
    for word in rest.split(' ') {
        if !empty && line.width() + 1 + word.width() > width {
            lines.push(Line::styled(
                std::mem::replace(&mut line, hang.clone()),
                style,
            ));
            empty = true;
        }
        if !empty {
            line.push(' ');
        }
        // Longer than a line on its own (a path, say): break it anywhere.
        for c in word.chars() {
            if line.width() + c.width().unwrap_or(0) > width && !line.trim().is_empty() {
                lines.push(Line::styled(
                    std::mem::replace(&mut line, hang.clone()),
                    style,
                ));
            }
            line.push(c);
        }
        empty = false;
    }
    lines.push(Line::styled(line, style));
    lines
}

fn one_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).to_owned()
}

fn spinner(now: jiff::Timestamp) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[(now.as_millisecond() / 100).rem_euclid(FRAMES.len() as i64) as usize]
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::model::{Role, Turn};
    use crate::setup::teams::TeamList;
    use crate::setup::wizard::tests::{answer, env, named, press, sample, wizard};
    use crate::setup::wizard::{Data, Wizard};
    use crate::test_support::{now, pr, pr_titled};

    fn draw(w: &mut Wizard, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, w)).unwrap();
        terminal.backend().to_string()
    }

    /// Wide (side by side) and narrow (stacked).
    fn both(name: &str, w: &mut Wizard) {
        insta::assert_snapshot!(format!("{name}_wide"), draw(w, 120, 22));
        insta::assert_snapshot!(format!("{name}_narrow"), draw(w, 80, 34));
    }

    fn me_work() -> Source {
        Source::Gh("me-work".into())
    }

    #[test]
    fn intro_screens() {
        both(
            "intro_first_run",
            &mut Wizard::new(env(None), Some(Intro::FirstRun), now()),
        );
        let mut update = Wizard::new(
            env(Some("bots = []\n")),
            Some(Intro::Update { from: 0 }),
            now(),
        );
        insta::assert_snapshot!("intro_update_wide", draw(&mut update, 120, 16));
    }

    #[test]
    fn accounts_and_names() {
        let mut w = wizard(Some("[profiles.job]\ngh_user = \"me-work\"\n"));
        both("accounts", &mut w);
        press(&mut w, KeyCode::Enter);
        answer(&mut w, "");
        w.input = crate::tui::input::LineInput::new("work");
        both("name_rename", &mut w);
    }

    #[test]
    fn where_profiles_apply() {
        let mut w = wizard(None);
        named(&mut w);
        press(&mut w, KeyCode::Down);
        both("default", &mut w);
        press(&mut w, KeyCode::Enter);
        w.input = crate::tui::input::LineInput::new("~/work, ~/clients");
        both("dirs", &mut w);
        answer(&mut w, "");
        w.input = crate::tui::input::LineInput::new("me@work.example");
        insta::assert_snapshot!("email_wide", draw(&mut w, 120, 16));
        answer(&mut w, "");
        assert!(w.warning.is_some());
        insta::assert_snapshot!("email_warning_wide", draw(&mut w, 120, 16));
    }

    fn at_teams() -> Wizard {
        let mut w = wizard(Some(
            "[profiles.work]\ngh_user = \"me-work\"\ndefault = true\nteam_reviews.teams = [\"acme/old\"]\n",
        ));
        press(&mut w, KeyCode::Char('j'));
        press(&mut w, KeyCode::Char(' '));
        press(&mut w, KeyCode::Enter);
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.step, Step::Teams(0));
        w
    }

    #[test]
    fn team_states() {
        let mut w = at_teams();
        insta::assert_snapshot!("teams_loading_wide", draw(&mut w, 120, 18));
        w.on_data(Data::Teams {
            source: me_work(),
            result: Ok(TeamList {
                teams: vec!["acme/platform".into(), "acme/robotics".into()],
                notes: vec!["locked: SAML SSO required".into()],
            }),
        });
        for (team, result) in [
            ("acme/old", Ok(0)),
            ("acme/platform", Ok(7)),
            ("acme/robotics", Err("timed out".to_owned())),
        ] {
            w.on_data(Data::Count {
                source: me_work(),
                team: team.into(),
                result,
            });
        }
        press(&mut w, KeyCode::Down);
        press(&mut w, KeyCode::Char(' '));
        both("teams_loaded", &mut w);

        let mut w = at_teams();
        w.on_data(Data::Teams {
            source: me_work(),
            result: Err("GitHub error: Your token has not been granted the required scopes".into()),
        });
        press(&mut w, KeyCode::Tab);
        insta::assert_snapshot!("teams_failed_wide", draw(&mut w, 120, 18));
    }

    fn at_view() -> Wizard {
        let mut w = at_teams();
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.step, Step::View(0));
        w
    }

    fn view(w: &Wizard, name: &str) -> crate::views::View {
        w.samples[&(me_work(), name.to_owned())]
            .view
            .clone()
            .unwrap()
    }

    #[test]
    fn view_states() {
        let mut w = at_view();
        insta::assert_snapshot!("view_loading_wide", draw(&mut w, 120, 18));

        let mut mine = pr_titled(12, "Speed up the planner");
        mine.author = Some("me".into());
        mine.turn = Turn::Reviewers;
        let mut by_bot = pr_titled(40, "Bump dependencies");
        by_bot.author = Some("deploy-svc".into());
        by_bot.repo = "acme/web".parse().unwrap();
        by_bot.last_activity.as_mut().unwrap().role = Role::Author;
        let v = view(&w, "for-me");
        w.on_data(Data::Sample {
            source: me_work(),
            view: v.clone(),
            result: Ok(sample(v, vec![pr(1234), mine, by_bot])),
        });
        both("view_loaded", &mut w);

        press(&mut w, KeyCode::Down);
        let v = view(&w, "mine");
        w.on_data(Data::Sample {
            source: me_work(),
            view: v,
            result: Err("request to GitHub failed: connection refused".into()),
        });
        insta::assert_snapshot!("view_failed_wide", draw(&mut w, 120, 14));
        press(&mut w, KeyCode::Down);
        insta::assert_snapshot!("view_repo_outside_a_checkout_wide", draw(&mut w, 120, 14));

        press(&mut w, KeyCode::Up);
        press(&mut w, KeyCode::Up);
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.step, Step::Bots(0));
        both("bots", &mut w);
        press(&mut w, KeyCode::Char(' '));
        press(&mut w, KeyCode::Enter);
        assert_eq!(w.step, Step::Review);
        both("review", &mut w);
    }
}
