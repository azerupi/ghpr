//! Guided setup: which accounts to use, a profile for each and where it
//! applies, teams, the default view, and bots, written into the config file.
//! Runs on first launch, again after an update adds questions, and any time
//! with `ghpr config init`.

mod accounts;
mod migrate;
mod preview;
mod teams;
mod term;
mod ui;
mod wizard;
mod write;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, bail};
use toml_edit::DocumentMut;

use crate::cli::GlobalArgs;
use crate::profile::Whereabouts;
use crate::{cache, config, git, state};
use wizard::{Env, Intro, Outcome, Plan, Wizard};
use write::Existing;

/// The setup version a config file was last written by is stored in it as
/// `setup_version`. Bump this, and add an entry to [`CHANGES`], whenever setup
/// gains a step worth showing to people who already went through it: they're
/// then offered setup again, with what's new listed.
pub const SETUP_VERSION: u32 = 1;

/// What each setup version added, oldest first.
pub const CHANGES: &[(u32, &str)] = &[(
    1,
    "guided setup: accounts, profiles, teams, default view, bots",
)];

/// Whether to go on to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proceed {
    Continue,
    Quit,
}

/// What to offer before the UI starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prompt {
    Nothing,
    FirstRun,
    Update { from: u32 },
}

/// Offer setup if there's no config file, or it was written by an older
/// setup. Only in a terminal, for the default config file, and online.
pub async fn maybe_prompt(global: &GlobalArgs) -> Result<Proceed> {
    let asked = Asked {
        tty: std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        explicit_config: global.config.is_some(),
        offline: global.offline,
    };
    let path = config::default_path()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        // Loading the config reports it properly.
        Err(_) => return Ok(Proceed::Continue),
    };
    let dismissed = dismissed_path().and_then(|p| std::fs::read_to_string(p).ok());
    let prompt = decide(
        asked,
        text.as_deref(),
        dismissed.as_deref().and_then(|d| d.trim().parse().ok()),
    );
    tracing::info!(?prompt, "setup prompt");
    let (intro, doc) = match prompt {
        Prompt::Nothing => return Ok(Proceed::Continue),
        Prompt::FirstRun => (Intro::FirstRun, None),
        Prompt::Update { from } => (
            Intro::Update { from },
            text.and_then(|t| t.parse::<DocumentMut>().ok()),
        ),
    };
    let outcome = run(&path, doc, Some(intro)).await?;
    finish(&path, outcome)
}

/// `ghpr config init`: setup any time, starting from the file's values.
pub async fn init(path: &Path) -> Result<()> {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        bail!("`ghpr config init` asks questions, so it needs a terminal");
    }
    let doc = match std::fs::read_to_string(path) {
        Ok(text) => Some(
            text.parse::<DocumentMut>()
                .wrap_err_with(|| format!("invalid config file {}", path.display()))?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(e).wrap_err_with(|| format!("could not read {}", path.display()));
        }
    };
    let outcome = run(path, doc, None).await?;
    finish(path, outcome)?;
    Ok(())
}

struct Asked {
    tty: bool,
    explicit_config: bool,
    offline: bool,
}

fn decide(asked: Asked, config: Option<&str>, dismissed: Option<u32>) -> Prompt {
    if !asked.tty || asked.explicit_config || asked.offline {
        return Prompt::Nothing;
    }
    let Some(text) = config else {
        return Prompt::FirstRun;
    };
    // A broken file is for `config::load` to explain, not for setup to replace.
    let Ok(doc) = text.parse::<DocumentMut>() else {
        return Prompt::Nothing;
    };
    let from = Existing::read(&doc).setup_version;
    if from >= SETUP_VERSION || dismissed.is_some_and(|d| d >= SETUP_VERSION) {
        Prompt::Nothing
    } else {
        Prompt::Update { from }
    }
}

/// Run the wizard on `path`, starting from `doc` (the file's contents) if any.
async fn run(path: &Path, doc: Option<DocumentMut>, intro: Option<Intro>) -> Result<Outcome> {
    let env = Env {
        path: path.to_owned(),
        has_file: doc.is_some(),
        doc: doc.unwrap_or_else(write::new_document),
        accounts: accounts::gh_accounts(),
        here: Whereabouts::current(None),
        current_repo: git::current_repo(),
        state_root: state_root(),
        exists: Box::new(|p| p.exists()),
        env_set: Box::new(|var| std::env::var(var).is_ok_and(|v| !v.trim().is_empty())),
    };
    let wizard = Wizard::new(env, intro, jiff::Timestamp::now());
    term::run(wizard).await
}

/// Act on how setup ended, and say what happened (on stdout, so it stays in
/// the scrollback after the full-screen wizard is gone).
fn finish(path: &Path, outcome: Outcome) -> Result<Proceed> {
    tracing::info!(outcome = ?std::mem::discriminant(&outcome), "setup finished");
    match outcome {
        Outcome::Write(plan) => {
            write_plan(path, &plan)?;
            Ok(Proceed::Continue)
        }
        Outcome::Skip => {
            let text = format!("{}setup_version = {SETUP_VERSION}\n", write::STARTER);
            write::write(path, &text)?;
            println!(
                "Wrote a starter config to {}. `ghpr config init` runs setup any time.",
                path.display()
            );
            Ok(Proceed::Continue)
        }
        Outcome::NotNow => Ok(Proceed::Continue),
        Outcome::DontAsk => {
            if let Some(file) = dismissed_path() {
                if let Some(dir) = file.parent() {
                    std::fs::create_dir_all(dir)
                        .wrap_err_with(|| format!("could not create {}", dir.display()))?;
                }
                std::fs::write(&file, format!("{SETUP_VERSION}\n"))
                    .wrap_err_with(|| format!("could not write {}", file.display()))?;
            }
            println!(
                "Setup won't come up again until it has new questions; `ghpr config init` runs it any time."
            );
            Ok(Proceed::Continue)
        }
        Outcome::Cancelled => {
            println!("Setup cancelled; nothing was written.");
            Ok(Proceed::Quit)
        }
    }
}

fn write_plan(path: &Path, plan: &Plan) -> Result<()> {
    let backup = write::write(path, &plan.text)?;
    migrate::apply(
        &plan.moves,
        &plan.profiles,
        cache::default_dir(None).ok().as_deref(),
    )?;
    // The samples become each profile's cache, so the UI opens with data.
    for (profile, sample) in &plan.caches {
        let Ok(dir) = cache::default_dir(Some(profile)) else {
            continue;
        };
        let file = cache::path(&dir, &sample.view);
        if let Err(e) = cache::save(
            &file,
            &sample.all,
            &sample.viewer,
            cached_at(plan, profile, sample),
        ) {
            tracing::warn!(error = %format!("{e:#}"), "couldn't save a sample as cache");
        }
    }

    println!("Wrote {}", path.display());
    if let Some(backup) = backup {
        println!("  previous version backed up to {}", backup.display());
    }
    for m in &plan.moves {
        println!(
            "  moved seen/snoozed/muted state {} → {} ({})",
            m.from.display(),
            m.to.display(),
            m.why
        );
    }
    if !plan.removed.is_empty() {
        println!("  removed profile {}", plan.removed.join(", "));
    }
    for p in &plan.profiles {
        let mut when = Vec::new();
        if p.default {
            when.push("default".to_owned());
        }
        when.extend(p.dirs.iter().cloned());
        when.extend(p.email.iter().map(|e| format!("user.email {e}")));
        if when.is_empty() {
            when.push("--profile only".to_owned());
        }
        println!("  profile {}: {} ({})", p.name, p.source, when.join(", "));
    }
    println!("`ghpr paths` shows which profile applies where you are.");
    Ok(())
}

/// When to say a sample was fetched. One cut short by the sample's limit is
/// dated a refresh interval back: still shown while the UI loads, but never
/// fresh enough for `ghpr count` to trust.
fn cached_at(plan: &Plan, profile: &str, sample: &preview::Sample) -> jiff::Timestamp {
    if !sample.truncated {
        return sample.at;
    }
    let here = Whereabouts {
        explicit: Some(profile.to_owned()),
        ..Whereabouts::default()
    };
    let interval = config::parse_with(&plan.text, &here)
        .map(|c| c.refresh_interval)
        .unwrap_or_else(|_| config::Config::default().refresh_interval);
    sample.at - interval
}

/// The directory holding `state.json` (and `profiles/`).
fn state_root() -> Option<PathBuf> {
    state::default_path(None)
        .ok()
        .and_then(|p| p.parent().map(Path::to_owned))
}

/// Records the setup version you said not to be asked about again. Kept with
/// the state rather than in the config, so declining never rewrites your file.
fn dismissed_path() -> Option<PathBuf> {
    state_root().map(|dir| dir.join("setup-dismissed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked() -> Asked {
        Asked {
            tty: true,
            explicit_config: false,
            offline: false,
        }
    }

    #[test]
    fn offers_setup_for_missing_and_older_configs() {
        assert_eq!(decide(asked(), None, None), Prompt::FirstRun);
        assert_eq!(
            decide(asked(), Some("bots = []"), None),
            Prompt::Update { from: 0 },
            "written before setup existed"
        );
        let current = format!("setup_version = {SETUP_VERSION}");
        assert_eq!(decide(asked(), Some(&current), None), Prompt::Nothing);
        assert_eq!(
            decide(asked(), Some("setup_version = 0"), Some(SETUP_VERSION)),
            Prompt::Nothing,
            "dismissed for this version"
        );
        assert_eq!(
            decide(asked(), Some("setup_version = 0"), Some(SETUP_VERSION - 1)),
            Prompt::Update { from: 0 },
            "dismissed an older version"
        );
        assert_eq!(decide(asked(), Some("not toml ["), None), Prompt::Nothing);
    }

    #[test]
    fn never_prompts_outside_a_terminal_or_for_other_files() {
        for asked in [
            Asked {
                tty: false,
                ..asked()
            },
            Asked {
                explicit_config: true,
                ..asked()
            },
            Asked {
                offline: true,
                ..asked()
            },
        ] {
            assert_eq!(decide(asked, None, None), Prompt::Nothing);
        }
    }

    #[test]
    fn cut_short_samples_are_cached_as_stale() {
        let view =
            crate::views::resolve(Some("mine"), &[], &config::Config::default(), || None).unwrap();
        let mut sample = *wizard::tests::sample(view, vec![]);
        let plan = Plan {
            text: "[profiles.p]\ndefault = true\nrefresh_interval = \"2m\"\n".into(),
            profiles: vec![],
            moves: vec![],
            removed: vec![],
            caches: vec![],
        };
        assert_eq!(cached_at(&plan, "p", &sample), sample.at);
        sample.truncated = true;
        assert_eq!(
            cached_at(&plan, "p", &sample),
            sample.at - jiff::SignedDuration::from_mins(2)
        );
    }

    #[test]
    fn every_version_says_what_it_added() {
        assert_eq!(CHANGES.last().map(|(v, _)| *v), Some(SETUP_VERSION));
        assert!(CHANGES.windows(2).all(|w| w[0].0 < w[1].0));
    }
}
