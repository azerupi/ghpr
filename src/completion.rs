//! Shell completion for filter, sort, and view arguments.
//!
//! Enabled by sourcing `COMPLETE=<shell> ghpr` (see the README); the shell then
//! calls back into ghpr, which answers from the config and the cached PRs.

use std::ffi::OsStr;

use clap_complete::engine::CompletionCandidate;

use crate::filter::complete;
use crate::profile::Whereabouts;
use crate::{cache, config, git, state, views};

/// Complete the word at the end of a `-q` value, using the default view's
/// cached PRs for values like repositories and labels.
pub fn filter(current: &OsStr) -> Vec<CompletionCandidate> {
    let text = current.to_string_lossy();
    let now = jiff::Timestamp::now();
    let config = load_config();
    let profile = config.profile.as_ref().map(|p| p.name.as_str());
    let cached = views::resolve(None, &[], &config, git::current_repos)
        .ok()
        .and_then(|view| cache::load(&cache::path(&cache::default_dir(profile).ok()?, &view)));
    let (prs, viewer) = cached.map_or((Vec::new(), String::new()), |c| (c.prs, c.viewer));
    let local = state::default_path(profile)
        .ok()
        .and_then(|p| state::load(&p, now).ok());

    let Some(completion) = complete::filter(
        &text,
        text.chars().count(),
        &prs,
        &viewer,
        now,
        local.as_ref(),
    ) else {
        return Vec::new();
    };
    candidates(&text, &completion)
}

pub fn sort(current: &OsStr) -> Vec<CompletionCandidate> {
    let text = current.to_string_lossy();
    complete::sort(&text, text.chars().count()).map_or_else(Vec::new, |c| candidates(&text, &c))
}

pub fn views() -> Vec<CompletionCandidate> {
    views::names(&load_config())
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

/// The config for the profile in effect here. Completion can't see a
/// `--profile` on the command line being completed, only `GHPR_PROFILE`.
fn load_config() -> config::Config {
    let here = Whereabouts::current(std::env::var("GHPR_PROFILE").ok());
    let path = std::env::var_os("GHPR_CONFIG");
    config::load(path.as_deref().map(AsRef::as_ref), &here).unwrap_or_default()
}

/// Shells replace the whole argument, so each candidate is the full text with
/// the word under completion replaced.
fn candidates(text: &str, completion: &complete::Completion) -> Vec<CompletionCandidate> {
    let before: String = text.chars().take(completion.start).collect();
    completion
        .candidates
        .iter()
        .map(|c| {
            let help = match c.count {
                Some(n) if c.description.is_empty() => format!("{n} PRs"),
                Some(n) => format!("{} ({n})", c.description),
                None => c.description.clone(),
            };
            CompletionCandidate::new(format!("{before}{}", c.replacement)).help(Some(help.into()))
        })
        .collect()
}
