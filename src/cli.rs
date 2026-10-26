use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use clap_complete::{ArgValueCandidates, ArgValueCompleter};

use crate::completion;

use crate::model::RepoName;

/// Find the GitHub pull requests that need your attention.
#[derive(Debug, Parser)]
#[command(version, about, args_conflicts_with_subcommands = true)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    /// View to open (built-in: for-me, mine, repo, org; or one defined in the config).
    #[arg(add = ArgValueCandidates::new(completion::views))]
    pub view: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Restrict to this repository (OWNER/REPO). Can be repeated.
    #[arg(short = 'R', long = "repo", global = true, value_name = "OWNER/REPO")]
    pub repos: Vec<RepoName>,

    /// Use this config file instead of the default location.
    #[arg(long, global = true, env = "GHPR_CONFIG", value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Use this profile instead of picking one by directory (see the README).
    #[arg(long, global = true, env = "GHPR_PROFILE", value_name = "NAME")]
    pub profile: Option<String>,

    /// Only show cached results; don't contact GitHub.
    #[arg(long, global = true, conflicts_with = "no_cache")]
    pub offline: bool,

    /// Ignore cached results and wait for fresh ones.
    #[arg(long, global = true)]
    pub no_cache: bool,

    /// Filter, e.g. 'turn:me -draft waiting:>2d' (replaces the view's own filter).
    #[arg(short = 'q', long = "filter", global = true, value_name = "FILTER",
          add = ArgValueCompleter::new(completion::filter))]
    pub filter: Option<String>,

    /// Sort keys, e.g. 'turn,-waiting' (replaces the view's own sort).
    #[arg(long, global = true, value_name = "KEYS", add = ArgValueCompleter::new(completion::sort))]
    pub sort: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print pull requests without starting the interactive UI.
    List(ListArgs),

    /// Print how many pull requests a view shows, e.g. for a status bar.
    ///
    /// Uses the cache while it's fresh (see `refresh_interval`).
    Count(CountArgs),

    /// Set up or inspect the configuration.
    #[command(subcommand)]
    Config(ConfigCommand),

    /// Show the profile in effect here, and where config, cache, state, and logs live.
    Paths,
}

#[derive(Debug, Args)]
pub struct CountArgs {
    /// View to count (default: the configured default view).
    #[arg(add = ArgValueCandidates::new(completion::views))]
    pub view: Option<String>,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// View to list (built-in: for-me, mine, repo, org; or one defined in the config).
    #[arg(add = ArgValueCandidates::new(completion::views))]
    pub view: Option<String>,

    /// Maximum number of pull requests to fetch (GitHub search stops at 1000).
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub limit: u32,

    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
#[group(multiple = false)]
pub struct OutputArgs {
    /// Print an aligned table (default).
    #[arg(long)]
    pub table: bool,

    /// Print JSON.
    #[arg(long)]
    pub json: bool,

    /// Print one line per PR using a template, e.g. '{repo}#{number} {title}'.
    #[arg(long, value_name = "TEMPLATE")]
    pub format: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Set up accounts, profiles, teams, the default view, and bots, step by
    /// step. Starts from the file's current values and backs it up first.
    Init,
    /// Print the path of the config file.
    Path,
    /// Load the config file and report any errors.
    Check,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn output_flags_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["ghpr", "list", "--json", "--table"]).is_err());
    }

    #[test]
    fn bare_view_opens_the_ui() {
        let cli = Cli::try_parse_from(["ghpr", "mine"]).unwrap();
        assert_eq!(cli.view.as_deref(), Some("mine"));
        assert!(cli.command.is_none());

        let cli = Cli::try_parse_from(["ghpr", "list", "mine"]).unwrap();
        assert!(cli.view.is_none());
        assert!(matches!(cli.command, Some(Command::List(_))));
    }

    #[test]
    fn repo_flag_is_global_and_repeatable() {
        let cli = Cli::try_parse_from(["ghpr", "list", "-R", "a/b", "--repo", "c/d"]).unwrap();
        assert_eq!(cli.global.repos.len(), 2);
    }
}
