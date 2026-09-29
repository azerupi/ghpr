use jiff::Timestamp;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap};
use unicode_width::UnicodeWidthStr;

use super::app::{App, PromptKind, SNOOZES, Tab};
use crate::columns::{self, Column};
use crate::filter::Mark;
use crate::model::{
    ActivityKind, CiState, Mergeable, Pr, ReviewDecision, ReviewState, Reviewer, Turn,
};
use crate::model::{ScoreWeights, breakdown};
use crate::output::age;
use crate::progress::{STALL_SECS, elapsed};
use crate::state::{Flags, Snooze};

/// Waiting longer than this is highlighted in yellow, then red.
const WAIT_WARN_SECS: i64 = 2 * 86_400;
const WAIT_CRIT_SECS: i64 = 5 * 86_400;
const DETAIL_HEIGHT: u16 = 8;

pub fn render(frame: &mut Frame, app: &mut App) {
    let detail_height = if app.show_detail && app.selected().is_some() {
        DETAIL_HEIGHT
    } else {
        0
    };
    let [header, table, detail, status] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(detail_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_header(frame, app, header);
    let table = if app.facets.is_some() {
        let [side, rest] =
            Layout::horizontal([Constraint::Length(FACETS_WIDTH), Constraint::Fill(1)])
                .areas(table);
        render_facets(frame, app, side);
        rest
    } else {
        table
    };
    render_table(frame, app, table);
    if let Some(pr) = app.selected().filter(|_| detail_height > 0) {
        render_detail(frame, pr, app.flags(pr), &app.weights, app.now, detail);
    }
    render_status(frame, app, status);
    render_menu(frame, app, status);
    if app.snoozing {
        render_snooze(frame);
    }
    if app.show_help {
        render_help(frame);
    }
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let [tabs_line, info_line] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

    let mut tabs = vec![" ghpr ".bold().reversed()];
    if let Some(profile) = &app.profile {
        tabs.push(format!(" {profile} ").black().on_cyan());
    }
    tabs.push(Span::raw(" "));
    for (i, tab) in app.tabs.iter().enumerate() {
        let name = if tab.refreshing {
            format!(" {} {} ", spinner(app.now), tab.view_name)
        } else {
            format!(" {} ", tab.view_name)
        };
        tabs.push(if i == app.active {
            name.bold().reversed()
        } else {
            name.dark_gray()
        });
    }
    frame.render_widget(Line::from(tabs), tabs_line);
    frame.render_widget(
        Line::from("[ ] views · ? help ".dark_gray()).right_aligned(),
        tabs_line,
    );

    let tab = app.tab();
    let mine = app.my_turn_count();
    let new = app.new_count();
    let hidden = app.hidden_count();
    let count = if tab.prs.len() == tab.total() {
        format!(" {} PRs", tab.prs.len())
    } else {
        format!(" {} of {} PRs", tab.prs.len(), tab.total())
    };
    let mut spans = vec![Span::raw(count)];
    if mine > 0 {
        spans.push(Span::raw(" · "));
        spans.push(format!("{mine} your turn").green().bold());
    }
    if new > 0 {
        spans.push(Span::raw(" · "));
        spans.push(format!("{new} new").yellow());
    }
    // Marked rows the filter hides don't count: actions only apply to shown ones.
    let marked = tab
        .prs
        .iter()
        .filter(|pr| tab.marked.contains(&pr.id))
        .count();
    if marked > 0 {
        spans.push(Span::raw(" · "));
        spans.push(format!("{marked} selected").cyan().bold());
    }
    if hidden > 0 {
        spans.push(format!(" · {hidden} snoozed/muted hidden").dark_gray());
    }
    if !tab.filter_text.is_empty() {
        spans.push(Span::raw(" · filter: "));
        spans.push(tab.filter_text.clone().cyan());
    }
    spans.push(Span::raw(" · sort: "));
    spans.push(tab.sort.to_text().cyan());
    frame.render_widget(Line::from(spans), info_line);
}

fn render_table(frame: &mut Frame, app: &mut App, area: Rect) {
    // The header row takes one line.
    app.page_rows = area.height.saturating_sub(1) as usize;
    let active = app.active;
    let tab = app.tab();

    if tab.prs.is_empty() {
        let text = if tab.refreshing {
            let step = tab
                .progress
                .as_ref()
                .map_or_else(|| "connecting".to_owned(), |(p, _)| p.describe());
            format!("{} Loading: {step}…", spinner(app.now))
        } else if tab.error.is_some() {
            "Couldn't load pull requests.".to_owned()
        } else if tab.total() > 0 {
            "No pull requests match the filter (Esc clears it).".to_owned()
        } else {
            "No pull requests in this view.".to_owned()
        };
        let paragraph = Paragraph::new(text.dark_gray()).centered();
        let [middle] = Layout::vertical([Constraint::Length(1)])
            .flex(ratatui::layout::Flex::Center)
            .areas(area);
        frame.render_widget(paragraph, middle);
        return;
    }

    // Content widths, capped for columns whose values can get long.
    let widths: Vec<usize> = app
        .columns
        .iter()
        .map(|&column| {
            let cap = match column {
                Column::Repo => 30,
                Column::Author => 16,
                Column::Labels => 24,
                Column::Ci => 2,
                _ => usize::MAX,
            };
            tab.prs
                .iter()
                .map(|pr| column.text(pr, &app.weights, app.now).width())
                .chain([column.header().width()])
                .max()
                .unwrap_or(0)
                .min(cap)
        })
        .collect();
    // The marker column and its spacing take two characters.
    let keep = columns::fit(
        &app.columns,
        &widths,
        (area.width as usize).saturating_sub(2),
        1,
    );
    let shown: Vec<(Column, usize)> = app
        .columns
        .iter()
        .zip(&widths)
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|((c, w), _)| (*c, *w))
        .collect();

    let rows: Vec<Row> = tab
        .prs
        .iter()
        .map(|pr| {
            let flags = app.flags(pr);
            let cells = std::iter::once(Cell::from(marker(flags))).chain(
                shown
                    .iter()
                    .map(|(column, _)| cell(*column, pr, &app.weights, app.now)),
            );
            let row = Row::new(cells.collect::<Vec<_>>());
            let row = match pr.turn {
                Turn::Me => row.bold(),
                _ if pr.is_draft => row.dark_gray(),
                _ => row,
            };
            if tab.marked.contains(&pr.id) {
                row.bg(Color::Indexed(24))
            } else {
                row
            }
        })
        .collect();
    let header = Row::new(
        std::iter::once("")
            .chain(shown.iter().map(|(c, _)| c.header()))
            .collect::<Vec<_>>(),
    )
    .style(Style::new().bold().underlined());
    let constraints: Vec<Constraint> = std::iter::once(Constraint::Length(1))
        .chain(shown.iter().map(|(column, width)| match column {
            Column::Title => Constraint::Fill(1),
            _ => Constraint::Length(*width as u16),
        }))
        .collect();
    let table = Table::new(rows, constraints)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(Color::Indexed(237)));
    frame.render_stateful_widget(table, area, &mut app.tabs[active].table);
}

/// A one-character marker for the PR's local state.
fn marker(flags: Flags) -> Span<'static> {
    if flags.muted {
        "m".dark_gray()
    } else if flags.snoozed.is_some() {
        "z".dark_gray()
    } else if flags.new {
        "★".yellow().bold()
    } else {
        Span::raw(" ")
    }
}

/// One styled cell.
fn cell(column: Column, pr: &Pr, weights: &ScoreWeights, now: Timestamp) -> Cell<'static> {
    match column {
        Column::Turn => Cell::from(Span::styled(pr.turn.label(), turn_style(pr.turn))),
        Column::Title if pr.is_draft => Cell::from(Line::from(vec![
            "draft ".dark_gray().italic(),
            Span::raw(pr.title.clone()),
        ])),
        Column::Size => Cell::from(Line::from(vec![
            format!("+{}", pr.additions).green(),
            Span::raw(" "),
            format!("-{}", pr.deletions).red(),
        ])),
        Column::Ci => Cell::from(ci_symbol(pr.ci)),
        Column::Review => Cell::from(review_span(pr.review_decision)),
        Column::Waiting => {
            let style = match now.duration_since(pr.waiting_since).as_secs() {
                s if s >= WAIT_CRIT_SECS => Style::new().red(),
                s if s >= WAIT_WARN_SECS => Style::new().yellow(),
                _ => Style::new(),
            };
            Cell::from(Span::styled(age(now, pr.waiting_since), style))
        }
        column if column.right_aligned() => {
            Cell::from(Line::from(column.text(pr, weights, now)).right_aligned())
        }
        column => Cell::from(column.text(pr, weights, now)),
    }
}

const FACETS_WIDTH: u16 = 36;

fn render_facets(frame: &mut Frame, app: &App, area: Rect) {
    let groups = app.facet_groups();
    let selected = app.facets.unwrap_or(0);
    let block = Block::new()
        .borders(Borders::RIGHT)
        .title(" Facets · Space include · x exclude ".bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let value_width = (inner.width as usize).saturating_sub(9);
    let mut lines = Vec::new();
    let mut selected_line = 0;
    let mut index = 0;
    for group in &groups {
        lines.push(Line::from(format!(" {}", group.title).bold().underlined()));
        for value in &group.values {
            let mark = match value.mark {
                Mark::Included => "+".green().bold(),
                Mark::Excluded => "−".red().bold(),
                Mark::None => Span::raw(" "),
            };
            let name = if value.value.width() > value_width {
                let cut: String = value
                    .value
                    .chars()
                    .take(value_width.saturating_sub(1))
                    .collect();
                format!("{cut}…")
            } else {
                value.value.clone()
            };
            let mut line = Line::from(vec![
                Span::raw(" "),
                mark,
                Span::raw(format!(" {name:<value_width$} {:>4}", value.count)),
            ]);
            if value.count == 0 && value.mark != Mark::Excluded {
                line = line.dark_gray();
            }
            if index == selected {
                selected_line = lines.len();
                line = line.style(Style::new().bg(Color::Indexed(237)).bold());
            }
            lines.push(line);
            index += 1;
        }
    }
    // Scroll so the selection stays in view, with a little context above it.
    let height = inner.height as usize;
    let offset = selected_line
        .saturating_sub(height.saturating_sub(3))
        .min(lines.len().saturating_sub(height));
    frame.render_widget(Paragraph::new(lines).scroll((offset as u16, 0)), inner);
}

fn turn_style(turn: Turn) -> Style {
    match turn {
        Turn::Me => Style::new().green().bold(),
        Turn::Team => Style::new().cyan(),
        Turn::Merge => Style::new().magenta(),
        Turn::Author => Style::new().yellow(),
        Turn::Reviewers => Style::new().blue(),
        Turn::Blocked => Style::new().dark_gray(),
    }
}

fn ci_symbol(ci: CiState) -> Span<'static> {
    match ci {
        CiState::Pass => "✓".green(),
        CiState::Fail => "✗".red(),
        CiState::Pending => "●".yellow(),
        CiState::None => "·".dark_gray(),
    }
}

fn review_span(decision: Option<ReviewDecision>) -> Span<'static> {
    match decision {
        Some(ReviewDecision::Approved) => "approved".green(),
        Some(ReviewDecision::ChangesRequested) => "changes".red(),
        Some(ReviewDecision::ReviewRequired) => "required".into(),
        None => "-".dark_gray(),
    }
}

fn render_detail(
    frame: &mut Frame,
    pr: &Pr,
    flags: Flags,
    weights: &ScoreWeights,
    now: Timestamp,
    area: Rect,
) {
    let title = Line::from(vec![
        format!(" {}#{} ", pr.repo, pr.number).bold(),
        Span::raw(format!("{} ", pr.title)),
    ]);
    let block = Block::new().borders(Borders::TOP).title(title);
    let dim = |s: String| Span::styled(s, Style::new().dark_gray());
    let ago = |t: Timestamp| format!("{} ago", age(now, t));

    let mut lines = Vec::new();

    lines.push(Line::from(vec![
        Span::raw(pr.author.clone().unwrap_or_else(|| "ghost".to_owned())).bold(),
        Span::raw(format!(
            " wants to merge {} into {}",
            pr.head_ref, pr.base_ref
        )),
        dim(format!(
            " · opened {} · updated {} · +{} -{} in {} files",
            ago(pr.created_at),
            ago(pr.updated_at),
            pr.additions,
            pr.deletions,
            pr.changed_files
        )),
    ]));

    let mut turn_line = vec![
        Span::raw("Turn: "),
        Span::styled(pr.turn.label(), turn_style(pr.turn)),
        dim(format!(" for {}", age(now, pr.waiting_since))),
    ];
    if let Some(last) = &pr.last_activity {
        turn_line.push(Span::raw(format!(
            " · Last: {} ({}) {} {}",
            last.who,
            last.role.label(),
            activity_verb(last.kind),
            ago(last.at)
        )));
    }
    if pr.mentions_me {
        turn_line.push(" · mentions you".cyan());
    }
    if flags.new {
        turn_line.push(" · ★ new since you last looked".yellow());
    }
    match flags.snoozed {
        Some(Snooze::Until(until)) => {
            turn_line.push(dim(format!(" · snoozed for {} more", age(until, now))));
        }
        Some(Snooze::UntilActivity { .. }) => {
            turn_line.push(dim(" · snoozed until there's new activity".to_owned()));
        }
        None => {}
    }
    if flags.muted {
        turn_line.push(dim(" · muted".to_owned()));
    }
    lines.push(Line::from(turn_line));

    let mut review_line = vec![Span::raw("Review: "), review_span(pr.review_decision)];
    match pr.my_review {
        Some(review) => {
            review_line.push(Span::raw(format!(
                " · You {} {}",
                my_review_verb(review.state),
                ago(review.at)
            )));
            if pr.commits_since_my_review > 0 {
                let s = if pr.commits_since_my_review == 1 {
                    ""
                } else {
                    "s"
                };
                review_line
                    .push(format!(", {} new commit{s} since", pr.commits_since_my_review).yellow());
            }
        }
        None => review_line.push(dim(" · You haven't reviewed".to_owned())),
    }
    if !pr.requested_reviewers.is_empty() {
        let names: Vec<&str> = pr
            .requested_reviewers
            .iter()
            .map(|r| match r {
                Reviewer::User(login) | Reviewer::Team(login) => login.as_str(),
            })
            .collect();
        review_line.push(Span::raw(format!(" · Requested: {}", names.join(", "))));
    }
    if pr.unresolved_threads > 0 {
        let s = if pr.unresolved_threads == 1 { "" } else { "s" };
        review_line.push(format!(" · {} unresolved thread{s}", pr.unresolved_threads).yellow());
    }
    lines.push(Line::from(review_line));

    let mut health = vec![
        Span::raw("CI: "),
        ci_symbol(pr.ci),
        Span::raw(format!(" {}", pr.ci.label())),
    ];
    match pr.mergeable {
        Mergeable::Conflicting => health.push(" · merge conflicts".red()),
        Mergeable::Unknown => health.push(dim(" · mergeability unknown".to_owned())),
        Mergeable::Clean => {}
    }
    if !pr.labels.is_empty() {
        health.push(Span::raw(format!(" · Labels: {}", pr.labels.join(", "))));
    }
    lines.push(Line::from(health));
    let parts = breakdown(pr, weights, now);
    let total: i32 = parts.iter().map(|(_, p)| p).sum();
    let mut score_line = vec![Span::raw("Score: "), format!("{total}").bold()];
    for (i, (label, points)) in parts.iter().enumerate() {
        score_line.push(dim(format!(
            "{} {label} {points:+}",
            if i == 0 { " =" } else { "," }
        )));
    }
    lines.push(Line::from(score_line));
    lines.push(Line::from(dim(pr.url.clone())));

    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn activity_verb(kind: ActivityKind) -> &'static str {
    match kind {
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

fn my_review_verb(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Approved => "approved",
        ReviewState::ChangesRequested => "requested changes",
        ReviewState::Commented => "commented",
        ReviewState::Dismissed => "reviewed (dismissed)",
    }
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(prompt) = &app.prompt {
        let label = match prompt.kind {
            PromptKind::Filter => "/",
            PromptKind::Sort => "sort: ",
        };
        let text = prompt.input.text();
        let before: String = text.chars().take(prompt.input.cursor()).collect();
        let after: String = text.chars().skip(prompt.input.cursor()).collect();
        let ghost = prompt
            .completion
            .as_ref()
            .filter(|_| prompt.menu.is_none())
            .and_then(|c| c.ghost())
            .unwrap_or("");
        let mut spans = vec![
            label.bold(),
            Span::raw(before.clone()),
            Span::styled(ghost.to_owned(), Style::new().dark_gray()),
            Span::raw(after),
        ];
        if let Some(error) = &prompt.error {
            spans.push(format!("  {error}").red());
        } else if !ghost.is_empty() {
            spans.push("  Tab to complete".dark_gray());
        }
        frame.render_widget(Line::from(spans), area);
        let x = area.x + (label.width() + before.width()) as u16;
        frame.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
        return;
    }

    let tab = app.tab();
    let left = if let Some(error) = &tab.error {
        Line::from(format!(" {error}").red())
    } else if let Some((text, _)) = &app.flash {
        Line::from(format!(" {text}"))
    } else if tab.truncated {
        Line::from(" some searches hit the result limit".yellow())
    } else {
        Line::default()
    };
    frame.render_widget(left, area);

    let mut right = Vec::new();
    if app.offline {
        right.push("offline · ".yellow());
    }
    if let Some(at) = tab.fetched_at {
        let label = if tab.from_cache { "cached" } else { "updated" };
        right.push(format!("{label} {} ago", age(app.now, at)).dark_gray());
    }
    if tab.refreshing {
        right.extend(refresh_status(tab, app.now));
    }
    right.push(Span::raw(" "));
    frame.render_widget(Line::from(right).right_aligned(), area);
}

/// `⠹ refreshing: details 50/120 · 14s`, plus a warning when nothing has
/// happened for a while.
fn refresh_status(tab: &Tab, now: Timestamp) -> Vec<Span<'static>> {
    let started = tab.last_attempt.unwrap_or(now);
    let step = tab
        .progress
        .as_ref()
        .map_or_else(|| "connecting".to_owned(), |(p, _)| p.describe());
    let mut spans = vec![
        Span::raw(" · "),
        format!("{} refreshing: {step}", spinner(now)).cyan(),
        format!(" · {}", elapsed(now.duration_since(started).as_secs())).dark_gray(),
    ];
    let last_news = tab.progress.as_ref().map_or(started, |(_, at)| *at);
    let quiet = now.duration_since(last_news).as_secs();
    if quiet >= STALL_SECS {
        spans.push(format!(" · no response for {}", elapsed(quiet)).yellow());
    }
    spans
}

fn spinner(now: Timestamp) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[(now.as_millisecond() / 250).rem_euclid(FRAMES.len() as i64) as usize]
}

/// Key, description. An empty key starts a new section titled by the description.
fn render_snooze(frame: &mut Frame) {
    let mut lines: Vec<Line> = SNOOZES
        .iter()
        .map(|(key, label, _)| Line::from(vec![format!("  {key}  ").cyan(), Span::raw(*label)]))
        .collect();
    lines.push(Line::from("  any other key cancels".dark_gray()));
    let area = centered(frame.area(), 36, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Snooze ".bold())),
        area,
    );
}

/// Rows shown in the completion menu before it scrolls.
const MENU_ROWS: usize = 8;

/// The completion menu, just above the prompt and lined up with the word.
fn render_menu(frame: &mut Frame, app: &App, prompt_area: Rect) {
    let Some(prompt) = &app.prompt else { return };
    let (Some(selected), Some(completion)) = (prompt.menu, &prompt.completion) else {
        return;
    };
    let candidates = &completion.candidates;
    let label_width = match prompt.kind {
        PromptKind::Filter => 1,
        PromptKind::Sort => "sort: ".len(),
    };
    let before: String = prompt.input.text().chars().take(completion.start).collect();

    let name_width = candidates
        .iter()
        .map(|c| c.replacement.width())
        .max()
        .unwrap_or(0);
    let desc_width = candidates
        .iter()
        .map(|c| c.description.width())
        .max()
        .unwrap_or(0);
    let rows: Vec<Line> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let count = c.count.map_or(String::new(), |n| format!("{n:>4}"));
            let line = Line::from(vec![
                Span::raw(format!(" {:<name_width$}  ", c.replacement)),
                Span::styled(
                    format!("{:<desc_width$}", c.description),
                    Style::new().dark_gray(),
                ),
                Span::raw(format!("{count} ")),
            ]);
            let line = if c.count == Some(0) {
                line.dark_gray()
            } else {
                line
            };
            if i == selected {
                line.style(Style::new().bg(Color::Indexed(237)).bold())
            } else {
                line
            }
        })
        .collect();

    // Scroll so the selection stays visible.
    let visible = rows.len().min(MENU_ROWS);
    let first = selected.saturating_sub(visible - 1);
    let width = rows.iter().map(Line::width).max().unwrap_or(0) as u16 + 2;
    let height = visible as u16 + 2;
    let screen = frame.area();
    let x = (prompt_area.x + (label_width + before.width()) as u16).saturating_sub(1);
    let area = Rect {
        x: x.min(screen.width.saturating_sub(width)),
        y: prompt_area.y.saturating_sub(height),
        width: width.min(screen.width),
        height: height.min(prompt_area.y),
    };
    let title = if rows.len() > visible {
        format!(" {}/{} ", selected + 1, rows.len())
    } else {
        String::new()
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(rows[first..first + visible].to_vec())
            .block(Block::bordered().title_bottom(Line::from(title).right_aligned())),
        area,
    );
}

const HELP: &[(&str, &str)] = &[
    ("", "Move"),
    ("j/k ↓/↑", "move"),
    ("C-d/u PgDn/Up", "half / full page"),
    ("g/G", "first / last"),
    ("", "Act"),
    ("o / O", "open in browser / its Files tab"),
    ("y / Y", "copy URL / owner/repo#number"),
    ("Enter, Tab", "toggle details"),
    ("v / V", "mark row / all (or none); actions apply to marked"),
    ("f", "facets: counts per repo, author, ...; Space / x"),
    ("r", "refresh now"),
    ("[ ] Alt-1..9", "previous / next view, or view N"),
    ("", "Keep track"),
    ("Space / N", "mark seen (again: unseen) / all seen"),
    ("z", "snooze (again: wake up)"),
    ("M", "mute / unmute"),
    ("", "Filter & sort"),
    ("/", "filter (Enter keeps, Esc undoes)"),
    ("Tab / →", "complete; Tab again cycles the menu"),
    ("Esc", "clear marked rows, then the filter"),
    (
        "m n d b c Z",
        "toggle: my turn, new, -draft, -bot, -ci:fail, snoozed",
    ),
    ("s / S", "sort keys / reverse"),
    ("1-9", "sort by the Nth column (again: reverse)"),
    ("", "Filter syntax: key:value, -negate, a,b = either"),
    (
        "repo org label",
        "globs: repo:acme/* (Tab completes keys and values)",
    ),
    ("author team", "logins or @me (also reviewer); team slugs"),
    ("turn", "me team merge author reviewers blocked"),
    ("ci review mine", "ci:fail review:approved mine:stale"),
    ("age updated", ">2d <3h 1d..1w 2026-09-01; also waiting"),
    (
        "size files",
        "<100 >=5 10..50; size:xs,s,m,l,xl; also threads",
    ),
    ("flags", "draft conflict bot mentioned new snoozed muted"),
    ("words", "title/branch/repo/author; #123"),
    ("", ""),
    ("?  q", "this help · quit"),
];

/// Width of one column of the help popup.
const HELP_COLUMN: u16 = 72;

fn render_help(frame: &mut Frame) {
    let line = |(keys, action): &(&str, &'static str)| {
        if keys.is_empty() {
            Line::from(format!(" {action}").bold().underlined())
        } else {
            Line::from(vec![format!("  {keys:<16}").cyan(), Span::raw(*action)])
        }
    };
    // Keys on the left, filter syntax on the right when there's room for both.
    let split = HELP
        .iter()
        .position(|(keys, action)| keys.is_empty() && action.starts_with("Filter syntax"))
        .unwrap_or(HELP.len());
    let (keys, syntax) = HELP.split_at(split);
    let screen = frame.area();
    let two_columns = screen.width >= 2 * HELP_COLUMN + 3;

    let (width, height) = if two_columns {
        (2 * HELP_COLUMN + 3, keys.len().max(syntax.len()) as u16 + 2)
    } else {
        (HELP_COLUMN + 2, HELP.len() as u16 + 2)
    };
    let area = centered(screen, width, height);
    frame.render_widget(Clear, area);
    let block = Block::bordered().title(" Keys ".bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if two_columns {
        let [left, right] =
            Layout::horizontal([Constraint::Length(HELP_COLUMN + 1), Constraint::Fill(1)])
                .areas(inner);
        frame.render_widget(
            Paragraph::new(keys.iter().map(line).collect::<Vec<_>>()),
            left,
        );
        frame.render_widget(
            Paragraph::new(syntax.iter().map(line).collect::<Vec<_>>()),
            right,
        );
    } else {
        frame.render_widget(
            Paragraph::new(HELP.iter().map(line).collect::<Vec<_>>()),
            inner,
        );
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::model::{MyReview, Role};
    use crate::test_support::{now, pr, pr_titled};
    use crate::tui::app::tests::{settings, settings_with};

    fn draw(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal.backend().to_string()
    }

    fn sample() -> App {
        let mut reviewed = pr_titled(1234, "Fix token refresh race");
        reviewed.my_review = Some(MyReview {
            state: ReviewState::ChangesRequested,
            at: "2026-09-25T12:00:00Z".parse().unwrap(),
        });
        reviewed.commits_since_my_review = 2;
        reviewed.unresolved_threads = 1;
        reviewed.requested_reviewers = vec![Reviewer::Team("acme/platform".into())];

        let mut draft = pr_titled(7, "Experiment with the new scheduler");
        draft.is_draft = true;
        draft.turn = Turn::Blocked;
        draft.ci = CiState::Fail;
        draft.review_decision = None;

        let mut waiting = pr(88);
        waiting.repo = "acme/web".parse().unwrap();
        waiting.author = Some("bob".into());
        waiting.turn = Turn::Author;
        waiting.waiting_since = "2026-09-20T12:00:00Z".parse().unwrap();
        waiting.last_activity.as_mut().unwrap().role = Role::Reviewer;

        let mut settings = settings_with(&["for-me", "mine", "backend"], false);
        // Everything is new except the draft.
        settings.state.mark_seen(&draft.id, now());
        App::new(settings, now()).with_cache(
            0,
            vec![reviewed, draft, waiting],
            "me".into(),
            now() - SignedDuration::from_mins(3),
        )
    }

    #[test]
    fn renders_table_and_detail() {
        insta::assert_snapshot!(draw(&mut sample(), 120, 15));
    }

    #[test]
    fn renders_empty_and_loading_states() {
        let mut app = App::new(settings("mine", false), now());
        app.init();
        insta::assert_snapshot!(draw(&mut app, 60, 5));
    }

    #[test]
    fn renders_filter_prompt_with_error() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = sample();
        for c in "/-draft turn:x".chars() {
            app.update(super::super::app::Msg::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        }
        insta::assert_snapshot!(draw(&mut app, 120, 16));
    }

    fn press(app: &mut App, code: crossterm::event::KeyCode) {
        use crossterm::event::{KeyEvent, KeyModifiers};
        app.update(super::super::app::Msg::Key(KeyEvent::new(
            code,
            KeyModifiers::NONE,
        )));
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, crossterm::event::KeyCode::Char(c));
        }
    }

    #[test]
    fn renders_ghost_completion() {
        let mut app = sample();
        type_text(&mut app, "/-dr");
        insta::assert_snapshot!(draw(&mut app, 100, 16));
    }

    #[test]
    fn renders_completion_menu() {
        let mut app = sample();
        type_text(&mut app, "/-draft turn:");
        press(&mut app, crossterm::event::KeyCode::Tab);
        press(&mut app, crossterm::event::KeyCode::Down);
        insta::assert_snapshot!(draw(&mut app, 100, 16));
    }

    #[test]
    fn renders_snooze_choices() {
        let mut app = sample();
        type_text(&mut app, "z");
        insta::assert_snapshot!(draw(&mut app, 80, 16));
    }

    #[test]
    fn renders_facets_and_marked_rows() {
        let mut app = sample();
        type_text(&mut app, "vf");
        press(&mut app, crossterm::event::KeyCode::Down);
        press(&mut app, crossterm::event::KeyCode::Char('x'));
        insta::assert_snapshot!(draw(&mut app, 120, 24));
    }

    #[test]
    fn narrow_terminals_drop_low_priority_columns() {
        insta::assert_snapshot!(draw(&mut sample(), 70, 16));
    }

    #[test]
    fn renders_configured_columns() {
        let mut app = sample();
        app.columns = ["number", "score", "title", "labels"]
            .iter()
            .map(|c| c.parse().unwrap())
            .collect();
        insta::assert_snapshot!(draw(&mut app, 100, 16));
    }

    #[test]
    fn renders_refresh_progress_and_stalls() {
        use super::super::app::Msg;
        use crate::progress::Progress;
        let mut app = sample();
        app.init();
        app.update(Msg::Progress {
            tab: 0,
            progress: Progress::Details {
                done: 50,
                total: 120,
            },
        });
        app.update(Msg::Tick(now() + jiff::SignedDuration::from_secs(9)));
        let status = draw(&mut app, 120, 16).lines().last().unwrap().to_owned();
        assert!(
            status.contains("refreshing: details 50/120 · 9s"),
            "{status}"
        );
        assert!(!status.contains("no response"), "{status}");

        app.update(Msg::Tick(now() + jiff::SignedDuration::from_secs(80)));
        let status = draw(&mut app, 120, 16).lines().last().unwrap().to_owned();
        assert!(status.contains("· 1m 20s"), "{status}");
        assert!(status.contains("no response for 1m 20s"), "{status}");
        let tabs = draw(&mut app, 120, 16).lines().next().unwrap().to_owned();
        assert!(tabs.contains(" for-me "), "{tabs}");
    }

    #[test]
    fn shows_the_profile() {
        let mut app = sample();
        app.profile = Some("work".into());
        let tabs = draw(&mut app, 100, 16).lines().next().unwrap().to_owned();
        assert!(tabs.starts_with("\" ghpr  work   for-me "), "{tabs}");
    }

    #[test]
    fn renders_help() {
        let mut app = sample();
        app.show_help = true;
        insta::assert_snapshot!("help_wide", draw(&mut app, 160, 32));
        insta::assert_snapshot!("help_narrow", draw(&mut app, 80, 40));
    }

    #[test]
    fn records_visible_rows_for_paging() {
        let mut app = sample();
        draw(&mut app, 120, 20);
        // 20 lines - two header lines - detail - status - table header row
        assert_eq!(app.page_rows, 20 - 2 - DETAIL_HEIGHT as usize - 1 - 1);
    }
}
