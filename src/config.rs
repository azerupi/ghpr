use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use jiff::SignedDuration;
use serde::Deserialize;

use crate::columns::{self, Column};
use crate::glob::Glob;
use crate::model::{ScoreWeights, TeamMode};

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    /// View used when none is given on the command line.
    pub default_view: Option<String>,
    /// How often the UI refreshes in the background, e.g. `5m` or `90s`.
    #[serde(deserialize_with = "duration")]
    pub refresh_interval: SignedDuration,
    /// User-defined views; these take precedence over built-ins of the same name.
    pub views: BTreeMap<String, ViewConfig>,
    /// Extra accounts treated as bots. GitHub App bots are detected automatically.
    pub bots: Vec<String>,
    pub team_reviews: TeamReviews,
    pub score: ScoreWeights,
    /// Table columns, in order. The title column is never dropped on narrow terminals.
    pub columns: Vec<Column>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default_view: None,
            refresh_interval: SignedDuration::from_mins(5),
            views: BTreeMap::new(),
            bots: Vec::new(),
            team_reviews: TeamReviews::default(),
            score: ScoreWeights::default(),
            columns: columns::DEFAULT.to_vec(),
        }
    }
}

fn duration<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<SignedDuration, D::Error> {
    use serde::de::Error;
    let text = String::deserialize(deserializer)?;
    let duration: SignedDuration = text
        .parse()
        .map_err(|e| D::Error::custom(format!("invalid duration {text:?}: {e}")))?;
    if duration < SignedDuration::from_secs(10) {
        return Err(D::Error::custom(format!(
            "{text:?} is too short (minimum 10s)"
        )));
    }
    Ok(duration)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TeamReviews {
    /// Teams (`org/slug`) whose review requests should show up in `for-me`.
    pub teams: Vec<String>,
    /// Treat a team request as a request to you (turn `me` instead of `team`).
    pub count_as_mine: bool,
    /// Per-repo overrides by pattern (`"acme/platform-*" = true`); the most
    /// specific match wins.
    pub repos: BTreeMap<String, TeamSetting>,
}

/// `true` (counts as yours), `false` (shows as `team`), or `"ignore"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeamSetting(pub TeamMode);

impl<'de> Deserialize<'de> for TeamSetting {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Text(String),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Bool(true) => Ok(TeamSetting(TeamMode::Mine)),
            Raw::Bool(false) => Ok(TeamSetting(TeamMode::Team)),
            Raw::Text(text) if text.eq_ignore_ascii_case("ignore") => {
                Ok(TeamSetting(TeamMode::Ignore))
            }
            Raw::Text(text) => Err(serde::de::Error::custom(format!(
                "expected true, false, or \"ignore\", got {text:?}"
            ))),
        }
    }
}

impl TeamReviews {
    pub fn default_mode(&self) -> TeamMode {
        if self.count_as_mine {
            TeamMode::Mine
        } else {
            TeamMode::Team
        }
    }

    pub fn repo_modes(&self) -> Vec<(Glob, TeamMode)> {
        self.repos
            .iter()
            .map(|(pattern, setting)| (Glob::new(pattern), setting.0))
            .collect()
    }
}

#[derive(Debug, Deserialize)]
pub struct ViewConfig {
    /// A GitHub search query, e.g. `is:open org:acme label:backend`.
    pub query: String,
    /// Initial client-side filter, e.g. `-draft ci:pass,pending`.
    #[serde(default)]
    pub filter: String,
    /// Initial sort, e.g. `turn,waiting`.
    pub sort: Option<String>,
}

/// `$XDG_CONFIG_HOME/ghpr/config.toml` (or the platform equivalent).
pub fn default_path() -> Result<PathBuf> {
    directories::ProjectDirs::from("", "", "ghpr")
        .map(|dirs| dirs.config_dir().join("config.toml"))
        .ok_or_else(|| eyre!("could not determine the config directory"))
}

/// Load the config. An explicitly given file must exist; a missing default file
/// just means "use the defaults".
pub fn load(explicit: Option<&Path>) -> Result<Config> {
    let path = match explicit {
        Some(path) => path.to_owned(),
        None => {
            let path = default_path()?;
            if !path.exists() {
                return Ok(Config::default());
            }
            path
        }
    };
    let text = std::fs::read_to_string(&path)
        .wrap_err_with(|| format!("could not read config file {}", path.display()))?;
    parse(&text).wrap_err_with(|| format!("invalid config file {}", path.display()))
}

fn parse(text: &str) -> Result<Config> {
    let config: Config = toml::from_str(text)?;
    for (name, view) in &config.views {
        crate::filter::Filter::parse(&view.filter)
            .map_err(|e| eyre!("views.{name}.filter: {e}"))?;
        if let Some(sort) = &view.sort {
            crate::sort::SortSpec::parse(sort).map_err(|e| eyre!("views.{name}.sort: {e}"))?;
        }
    }
    for team in &config.team_reviews.teams {
        if team
            .split_once('/')
            .is_none_or(|(org, slug)| org.is_empty() || slug.is_empty())
        {
            bail!("team_reviews.teams: expected ORG/TEAM-SLUG, got {team:?}");
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_defaults() {
        let config = parse("").unwrap();
        assert!(config.default_view.is_none());
        assert!(config.views.is_empty());
    }

    #[test]
    fn parses_views() {
        let config = parse(
            r#"
            default_view = "backend"

            [views.backend]
            query = "is:open org:acme label:backend"
            "#,
        )
        .unwrap();
        assert_eq!(config.default_view.as_deref(), Some("backend"));
        assert_eq!(
            config.views["backend"].query,
            "is:open org:acme label:backend"
        );
    }

    #[test]
    fn parses_score_weights_and_view_filters() {
        let config = parse(
            r#"
            [score]
            turn_me = 200
            label.urgent = 50
            repo."acme/core" = 20

            [views.backend]
            query = "org:acme"
            filter = "-draft ci:pass,pending"
            sort = "turn,waiting"
            "#,
        )
        .unwrap();
        assert_eq!(config.score.turn_me, 200);
        assert_eq!(
            config.score.mentioned, 30,
            "unset weights keep their default"
        );
        assert_eq!(config.score.label["urgent"], 50);
        assert_eq!(config.score.repo["acme/core"], 20);
        assert_eq!(
            config.views["backend"].sort.as_deref(),
            Some("turn,waiting")
        );
    }

    #[test]
    fn rejects_invalid_view_filters_and_score_keys() {
        let err = parse("[views.x]\nquery = \"q\"\nfilter = \"turn:you\"").unwrap_err();
        assert!(format!("{err:#}").contains("views.x.filter"), "{err:#}");
        let err = parse("[views.x]\nquery = \"q\"\nsort = \"nope\"").unwrap_err();
        assert!(format!("{err:#}").contains("views.x.sort"), "{err:#}");
        assert!(
            parse("[score]\nturn_you = 1").is_err(),
            "typos in weights are caught"
        );
    }

    #[test]
    fn parses_columns() {
        assert_eq!(Config::default().columns, columns::DEFAULT);
        let config = parse(r#"columns = ["turn", "title", "score"]"#).unwrap();
        assert_eq!(config.columns, [Column::Turn, Column::Title, Column::Score]);
        let err = parse(r#"columns = ["turn", "colour"]"#).unwrap_err();
        assert!(format!("{err:#}").contains("unknown column"), "{err:#}");
    }

    #[test]
    fn parses_refresh_interval() {
        assert_eq!(
            Config::default().refresh_interval,
            SignedDuration::from_mins(5)
        );
        let config = parse(r#"refresh_interval = "90s""#).unwrap();
        assert_eq!(config.refresh_interval, SignedDuration::from_secs(90));
        let config = parse(r#"refresh_interval = "2m""#).unwrap();
        assert_eq!(config.refresh_interval, SignedDuration::from_mins(2));
        assert!(parse(r#"refresh_interval = "soon""#).is_err());
        assert!(parse(r#"refresh_interval = "1s""#).is_err());
    }

    #[test]
    fn parses_bots_and_teams() {
        let config = parse(
            r#"
            bots = ["ci-user"]

            [team_reviews]
            teams = ["acme/platform"]
            count_as_mine = true
            "#,
        )
        .unwrap();
        assert_eq!(config.bots, ["ci-user"]);
        assert_eq!(config.team_reviews.teams, ["acme/platform"]);
        assert!(config.team_reviews.count_as_mine);
    }

    #[test]
    fn parses_per_repo_team_settings() {
        let config = parse(
            r#"
            [team_reviews.repos]
            "acme/platform-*" = true
            "acme/monorepo" = false
            "acme/legacy" = "ignore"
            "#,
        )
        .unwrap();
        let reviews = &config.team_reviews;
        assert_eq!(reviews.default_mode(), TeamMode::Team);
        let modes: Vec<TeamMode> = reviews.repo_modes().into_iter().map(|(_, m)| m).collect();
        // BTreeMap order: legacy, monorepo, platform-*
        assert_eq!(modes, [TeamMode::Ignore, TeamMode::Team, TeamMode::Mine]);
        let err = parse("[team_reviews.repos]\n\"a/b\" = \"sometimes\"").unwrap_err();
        assert!(format!("{err:#}").contains("ignore"), "{err:#}");
    }

    #[test]
    fn rejects_malformed_team_names() {
        for bad in ["platform", "acme/", "/platform"] {
            let text = format!("[team_reviews]\nteams = [{bad:?}]");
            assert!(parse(&text).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn view_without_query_is_an_error() {
        assert!(parse("[views.broken]\n").is_err());
    }
}
