mod auth;
mod cache;
mod cli;
mod clipboard;
mod columns;
mod completion;
mod config;
mod fetch;
mod filter;
mod git;
mod github;
mod glob;
mod logging;
mod model;
mod output;
mod profile;
mod progress;
mod sort;
mod state;
#[cfg(test)]
mod test_support;
mod tui;
mod views;

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::{CommandFactory, Parser};
use color_eyre::eyre::{Result, WrapErr, bail, eyre};

use cli::{Cli, Command, ConfigCommand, CountArgs, GlobalArgs, ListArgs};
use config::Config;
use filter::Filter;
use model::{Pr, Viewer};
use progress::{Progress, Reporter};
use sort::SortSpec;
use views::View;

/// Like `println!`, but a closed pipe (`ghpr paths | head -1`) isn't an error.
macro_rules! out {
    ($($arg:tt)*) => {
        write_out(&format!("{}\n", format_args!($($arg)*)))?
    };
}

fn write_out(text: &str) -> Result<()> {
    match std::io::stdout().lock().write_all(text.as_bytes()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other.wrap_err("failed to write output"),
    }
}

/// PRs fetched per search in the interactive UI.
const UI_LIMIT: u32 = 100;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Answers the shell's completion requests (`COMPLETE=zsh ghpr ...`) and exits.
    clap_complete::CompleteEnv::with_factory(Cli::command).complete();
    color_eyre::config::HookBuilder::default()
        .display_location_section(false)
        .display_env_section(false)
        .install()?;
    let cli = Cli::parse();
    let _log = logging::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        args = ?std::env::args().skip(1).collect::<Vec<_>>(),
        cwd = %std::env::current_dir().unwrap_or_default().display(),
        "start"
    );

    let result = match cli.command {
        Some(Command::List(args)) => list(&cli.global, args).await,
        Some(Command::Count(args)) => count(&cli.global, args).await,
        Some(Command::Config(command)) => config_command(&cli.global, command),
        Some(Command::Paths) => paths(&cli.global),
        None => interactive(&cli.global, cli.view.as_deref()).await,
    };
    match &result {
        Ok(()) => tracing::info!("exit"),
        Err(e) => tracing::error!(error = format!("{e:#}"), "exit with error"),
    }
    result
}

fn load_config(global: &GlobalArgs) -> Result<Config> {
    let here = profile::Whereabouts::current(global.profile.clone());
    let config = config::load(global.config.as_deref(), &here)?;
    match &config.profile {
        Some(p) => tracing::info!(profile = %p.name, reason = %p.reason, "profile"),
        None => tracing::info!("no profile"),
    }
    Ok(config)
}

fn profile_name(config: &Config) -> Option<&str> {
    config.profile.as_ref().map(|p| p.name.as_str())
}

struct Setup {
    config: Config,
    view: View,
    viewer: Viewer,
    cache_path: Option<PathBuf>,
    filter_text: String,
    filter: Filter,
    sort: SortSpec,
}

fn setup(global: &GlobalArgs, view: Option<&str>) -> Result<Setup> {
    let config = load_config(global)?;
    let view = views::resolve(view, &global.repos, &config, git::current_repo)?;
    let viewer = Viewer {
        login: String::new(),
        bots: config.bots.clone(),
        teams: config.team_reviews.teams.clone(),
        team_mode: config.team_reviews.default_mode(),
        team_repos: config.team_reviews.repo_modes(),
    };
    let cache_path = cache_path(&view, profile_name(&config));
    // Parsed up front, so a typo fails before any slow network requests.
    let filter_text = global.filter.clone().unwrap_or_else(|| view.filter.clone());
    let filter = Filter::parse(&filter_text).map_err(|e| eyre!("invalid filter: {e}"))?;
    let sort_text = global.sort.as_deref().unwrap_or(&view.sort);
    let sort = SortSpec::parse(sort_text).map_err(|e| eyre!("invalid sort: {e}"))?;
    Ok(Setup {
        config,
        view,
        viewer,
        cache_path,
        filter_text,
        filter,
        sort,
    })
}

fn cache_path(view: &View, profile: Option<&str>) -> Option<PathBuf> {
    cache::default_dir(profile)
        .ok()
        .map(|dir| cache::path(&dir, view))
}

fn load_state(
    now: jiff::Timestamp,
    profile: Option<&str>,
) -> Result<(state::LocalState, Option<PathBuf>)> {
    match state::default_path(profile) {
        Ok(path) => Ok((state::load(&path, now)?, Some(path))),
        Err(_) => Ok((state::LocalState::new(now), None)),
    }
}

async fn interactive(global: &GlobalArgs, view: Option<&str>) -> Result<()> {
    let setup = setup(global, view)?;
    let now = jiff::Timestamp::now();
    let profile = profile_name(&setup.config).map(str::to_owned);
    let (local, state_path) = load_state(now, profile.as_deref())?;

    // One tab per view. Views that can't be resolved here (like `repo` outside a
    // checkout) are left out; the one asked for already resolved above.
    let mut tabs: Vec<(tui::TabSettings, tui::TabSource)> = Vec::new();
    let mut active = 0;
    let initial = setup.view.name.clone();
    for name in views::names(&setup.config) {
        let (view, filter, sort) = if name == initial {
            active = tabs.len();
            (
                setup.view.clone(),
                setup.filter_text.clone(),
                setup.sort.clone(),
            )
        } else {
            let Ok(view) =
                views::resolve(Some(&name), &global.repos, &setup.config, git::current_repo)
            else {
                continue;
            };
            let sort = SortSpec::parse(&view.sort).map_err(|e| eyre!("view {name}: {e}"))?;
            (view.clone(), view.filter.clone(), sort)
        };
        let settings = tui::TabSettings {
            view_name: name,
            filter,
            sort,
        };
        let source = tui::TabSource {
            cache_path: cache_path(&view, profile.as_deref()),
            view,
        };
        tabs.push((settings, source));
    }
    let (tab_settings, tab_sources): (Vec<_>, Vec<_>) = tabs.into_iter().unzip();

    let settings = tui::Settings {
        tabs: tab_settings,
        active,
        refresh_interval: setup.config.refresh_interval,
        offline: global.offline,
        weights: setup.config.score,
        state: local,
        columns: setup.config.columns.clone(),
        profile: profile.clone(),
    };
    let mut app = tui::App::new(settings, now);
    if !global.no_cache {
        for (i, source) in tab_sources.iter().enumerate() {
            if let Some(cached) = source.cache_path.as_deref().and_then(cache::load) {
                app = app.with_cache(i, cached.prs, cached.viewer, cached.fetched_at);
            }
        }
    }
    if global.offline && app.tab().fetched_at.is_none() {
        bail!("no cached results for view {initial:?}; run without --offline first");
    }

    let client = if global.offline {
        None
    } else {
        Some(Arc::new(github::Client::new(auth::token(
            setup.config.profile.as_ref(),
        )?)?))
    };
    let source = tui::Source {
        client,
        tabs: tab_sources,
        viewer: setup.viewer,
        limit: UI_LIMIT,
        state_path,
    };
    tui::run(app, source).await
}

async fn list(global: &GlobalArgs, args: ListArgs) -> Result<()> {
    let setup = setup(global, args.view.as_deref())?;

    let (mut prs, viewer, truncated): (Vec<Pr>, String, _) = if global.offline {
        let Some(cached) = setup.cache_path.as_deref().and_then(cache::load) else {
            bail!(
                "no cached results for view {:?}; run without --offline first",
                setup.view.name
            );
        };
        eprintln!(
            "cached {} ago",
            output::age(jiff::Timestamp::now(), cached.fetched_at)
        );
        (cached.prs, cached.viewer, Vec::new())
    } else {
        let client = github::Client::new(auth::token(setup.config.profile.as_ref())?)?;
        let fetched = fetch_with_progress(&client, &setup.view, setup.viewer, args.limit).await?;
        if let Some(path) = &setup.cache_path
            && let Err(e) = cache::save(path, &fetched.prs, &fetched.viewer, jiff::Timestamp::now())
        {
            eprintln!("warning: could not save cache: {e:#}");
        }
        (fetched.prs, fetched.viewer, fetched.truncated)
    };

    let now = jiff::Timestamp::now();
    // Read-only here: listing doesn't mark anything as seen.
    let (local, _) = load_state(now, profile_name(&setup.config))?;
    let ctx = filter::Context {
        viewer: &viewer,
        now,
        local: Some(&local),
    };
    prs.retain(|pr| setup.filter.shows(pr, &ctx));
    setup.sort.sort(&mut prs, &setup.config.score, now);

    let text = if args.output.json {
        output::json(&prs)?
    } else if let Some(template) = &args.output.format {
        output::format(&prs, template, &setup.config.score, now)?
    } else {
        let width = std::io::stdout()
            .is_terminal()
            .then(|| crossterm::terminal::size().ok())
            .flatten()
            .map(|(cols, _)| cols as usize);
        output::table(&prs, &setup.config.columns, &setup.config.score, now, width)
    };

    write_out(&text)?;

    for t in &truncated {
        eprintln!(
            "fetched {} of {} PRs matching `{}`; raise --limit to see more",
            t.fetched, t.total, t.query
        );
    }
    Ok(())
}

/// Fetch a view, showing progress on stderr when it's a terminal (and nothing
/// when output goes to a script).
async fn fetch_with_progress(
    client: &github::Client,
    view: &View,
    viewer: Viewer,
    limit: u32,
) -> Result<fetch::Fetched> {
    if !std::io::stderr().is_terminal() {
        return fetch::fetch(client, view, viewer, limit, &Reporter::none()).await;
    }

    // The latest step and when it happened; redrawn on every step and every
    // second, so a slow request still shows time passing.
    let started = std::time::Instant::now();
    let latest: Arc<Mutex<(String, std::time::Instant)>> =
        Arc::new(Mutex::new(("connecting".to_owned(), started)));
    let draw = {
        let latest = Arc::clone(&latest);
        move || {
            let (step, at) = latest.lock().unwrap().clone();
            let mut line = format!(
                "{step}… {}",
                progress::elapsed(started.elapsed().as_secs() as i64)
            );
            let quiet = at.elapsed().as_secs() as i64;
            if quiet >= progress::STALL_SECS {
                line.push_str(&format!(" (no response for {})", progress::elapsed(quiet)));
            }
            eprint!("\r\x1b[K{line}");
        }
    };
    let reporter = {
        let (latest, draw) = (Arc::clone(&latest), draw.clone());
        Reporter::new(move |p: Progress| {
            *latest.lock().unwrap() = (p.describe(), std::time::Instant::now());
            draw();
        })
    };

    let fetching = fetch::fetch(client, view, viewer, limit, &reporter);
    tokio::pin!(fetching);
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
    let result = loop {
        tokio::select! {
            result = &mut fetching => break result,
            _ = ticker.tick() => draw(),
        }
    };
    eprint!("\r\x1b[K");
    result
}

async fn count(global: &GlobalArgs, args: CountArgs) -> Result<()> {
    let setup = setup(global, args.view.as_deref())?;
    let now = jiff::Timestamp::now();

    // Status bars run this often; don't hit GitHub while the cache is fresh.
    let cached = setup
        .cache_path
        .as_deref()
        .filter(|_| !global.no_cache)
        .and_then(cache::load);
    let fresh = cached
        .as_ref()
        .is_some_and(|c| now.duration_since(c.fetched_at) < setup.config.refresh_interval);
    let (prs, viewer) = match cached {
        Some(c) if fresh || global.offline => (c.prs, c.viewer),
        _ if global.offline => {
            bail!(
                "no cached results for view {:?}; run without --offline first",
                setup.view.name
            )
        }
        _ => {
            let client = github::Client::new(auth::token(setup.config.profile.as_ref())?)?;
            let fetched = fetch_with_progress(&client, &setup.view, setup.viewer, UI_LIMIT).await?;
            if let Some(path) = &setup.cache_path {
                let _ = cache::save(path, &fetched.prs, &fetched.viewer, now);
            }
            (fetched.prs, fetched.viewer)
        }
    };

    let (local, _) = load_state(now, profile_name(&setup.config))?;
    let ctx = filter::Context {
        viewer: &viewer,
        now,
        local: Some(&local),
    };
    out!(
        "{}",
        prs.iter().filter(|pr| setup.filter.shows(pr, &ctx)).count()
    );
    Ok(())
}

/// Where everything lives for the profile in effect here.
fn paths(global: &GlobalArgs) -> Result<()> {
    let config = load_config(global)?;
    let profile = profile_name(&config);
    let config_path = match &global.config {
        Some(path) => path.clone(),
        None => config::default_path()?,
    };
    let show =
        |p: Result<PathBuf>| p.map_or_else(|e| format!("({e})"), |p| p.display().to_string());
    match &config.profile {
        Some(p) => {
            let account = match (&p.gh_user, &p.token_env) {
                (_, Some(var)) => format!("token from ${var}"),
                (Some(user), None) => format!("gh account {user}"),
                (None, None) => "GH_TOKEN, GITHUB_TOKEN, or gh's active account".to_owned(),
            };
            out!("profile  {} ({}; {account})", p.name, p.reason);
        }
        None => out!("profile  none (GH_TOKEN, GITHUB_TOKEN, or gh's active account)"),
    }
    out!("config   {}", config_path.display());
    out!("cache    {}", show(cache::default_dir(profile)));
    out!("state    {}", show(state::default_path(profile)));
    out!(
        "logs     {}",
        logging::dir().map_or_else(|| "(unavailable)".to_owned(), |d| d.display().to_string())
    );
    Ok(())
}

fn config_command(global: &GlobalArgs, command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Path => {
            let path = match &global.config {
                Some(path) => path.clone(),
                None => config::default_path()?,
            };
            out!("{}", path.display());
        }
        ConfigCommand::Check => {
            let config = load_config(global)?;
            out!("config OK ({} custom views)", config.views.len());
            if let Some(p) = &config.profile {
                out!("profile: {} ({})", p.name, p.reason);
            }
        }
    }
    Ok(())
}
