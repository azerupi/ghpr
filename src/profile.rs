//! Profiles: separate accounts (say, work and personal) with their own token,
//! settings, cache, and state, picked by where ghpr runs.
//!
//! ```toml
//! [profiles.work]
//! dirs = ["~/work"]            # when run under these directories
//! email = "me@work.example"    # or in a repo whose git user.email matches
//! gh_user = "me-work"          # token from `gh auth token --user me-work`
//! # token_env = "WORK_TOKEN"   # or from this environment variable
//! # ...plus any top-level setting, overriding it for this profile
//!
//! [profiles.personal]
//! default = true               # when nothing else matches
//! ```

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, bail, eyre};

/// Keys that describe the profile itself; everything else in a profile's
/// table overrides the top-level config.
const OWN_KEYS: &[&str] = &["dirs", "email", "gh_user", "token_env", "default"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    /// Take the token for this gh account.
    pub gh_user: Option<String>,
    /// Take the token from this environment variable.
    pub token_env: Option<String>,
    /// Why this profile was picked, for `ghpr paths` and the log.
    pub reason: String,
}

/// Where ghpr is running, as far as picking a profile goes.
#[derive(Debug, Clone, Default)]
pub struct Whereabouts {
    /// `--profile` or `GHPR_PROFILE`.
    pub explicit: Option<String>,
    /// `git config ghpr.profile` here.
    pub git_profile: Option<String>,
    /// `git config user.email` here.
    pub git_email: Option<String>,
    pub cwd: PathBuf,
    pub home: Option<PathBuf>,
}

impl Whereabouts {
    pub fn current(explicit: Option<String>) -> Self {
        Whereabouts {
            explicit,
            git_profile: crate::git::config_value("ghpr.profile"),
            git_email: crate::git::config_value("user.email"),
            cwd: std::env::current_dir().unwrap_or_default(),
            home: directories::BaseDirs::new().map(|d| d.home_dir().to_owned()),
        }
    }
}

/// Pick a profile from the config's `[profiles]` table and fold its settings
/// into `root`. First match wins: `--profile`/`GHPR_PROFILE`, then
/// `git config ghpr.profile`, then `dirs` (longest match), then `email`, then
/// `default = true`.
pub fn apply(root: &mut toml::Table, here: &Whereabouts) -> Result<Option<Profile>> {
    let profiles = match root.remove("profiles") {
        Some(toml::Value::Table(profiles)) => profiles,
        Some(_) => bail!("profiles: expected a table of profiles"),
        None if here.explicit.is_some() => {
            bail!(
                "no profiles are configured, so there's no profile {:?}",
                here.explicit.as_deref().unwrap_or("")
            )
        }
        None => return Ok(None),
    };
    let tables: Vec<(&String, &toml::Table)> = profiles
        .iter()
        .map(|(name, value)| match value {
            toml::Value::Table(table) => Ok((name, table)),
            _ => Err(eyre!("profiles.{name}: expected a table")),
        })
        .collect::<Result<_>>()?;
    let names = || {
        tables
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let named = |name: &str, source: &str| -> Result<(String, String)> {
        match tables.iter().find(|(n, _)| *n == name) {
            Some(_) => Ok((name.to_owned(), source.to_owned())),
            None => bail!(
                "unknown profile {name:?} from {source} (profiles: {})",
                names()
            ),
        }
    };

    let chosen = if let Some(name) = &here.explicit {
        Some(named(name, "--profile / GHPR_PROFILE")?)
    } else if let Some(name) = &here.git_profile {
        Some(named(name, "git config ghpr.profile")?)
    } else {
        by_dir(&tables, here)?
            .or(by_email(&tables, here)?)
            .or(by_default(&tables)?)
    };
    let Some((name, reason)) = chosen else {
        return Ok(None);
    };

    let mut table = profiles
        .get(&name)
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let string = |table: &toml::Table, key: &str| -> Result<Option<String>> {
        match table.get(key) {
            None => Ok(None),
            Some(toml::Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => bail!("profiles.{name}.{key}: expected a string"),
        }
    };
    let profile = Profile {
        gh_user: string(&table, "gh_user")?,
        token_env: string(&table, "token_env")?,
        name: name.clone(),
        reason,
    };
    for key in OWN_KEYS {
        table.remove(*key);
    }
    merge(root, table);
    Ok(Some(profile))
}

/// Tables merge key by key (so a profile can add one view or set one weight);
/// anything else replaces the value underneath.
fn merge(base: &mut toml::Table, overrides: toml::Table) {
    for (key, value) in overrides {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base)), toml::Value::Table(value)) => merge(base, value),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

type Tables<'a> = [(&'a String, &'a toml::Table)];

fn by_dir(tables: &Tables, here: &Whereabouts) -> Result<Option<(String, String)>> {
    let mut best: Option<(usize, String, String)> = None;
    for (name, table) in tables {
        let dirs = match table.get("dirs") {
            None => continue,
            Some(toml::Value::Array(dirs)) => dirs,
            Some(_) => bail!("profiles.{name}.dirs: expected a list of directories"),
        };
        for dir in dirs {
            let Some(dir) = dir.as_str() else {
                bail!("profiles.{name}.dirs: expected a list of directories");
            };
            let path = expand_home(dir, here.home.as_deref());
            let len = path.components().count();
            if here.cwd.starts_with(&path) && best.as_ref().is_none_or(|(l, ..)| len > *l) {
                best = Some((len, name.to_string(), format!("directory {dir}")));
            }
        }
    }
    Ok(best.map(|(_, name, reason)| (name, reason)))
}

fn by_email(tables: &Tables, here: &Whereabouts) -> Result<Option<(String, String)>> {
    let Some(email) = &here.git_email else {
        return Ok(None);
    };
    for (name, table) in tables {
        match table.get("email") {
            None => {}
            Some(toml::Value::String(e)) if e.eq_ignore_ascii_case(email) => {
                return Ok(Some((name.to_string(), format!("git user.email {email}"))));
            }
            Some(toml::Value::String(_)) => {}
            Some(_) => bail!("profiles.{name}.email: expected a string"),
        }
    }
    Ok(None)
}

fn by_default(tables: &Tables) -> Result<Option<(String, String)>> {
    let mut defaults = Vec::new();
    for (name, table) in tables {
        match table.get("default") {
            None | Some(toml::Value::Boolean(false)) => {}
            Some(toml::Value::Boolean(true)) => defaults.push(name.to_string()),
            Some(_) => bail!("profiles.{name}.default: expected true or false"),
        }
    }
    match defaults.as_slice() {
        [] => Ok(None),
        [name] => Ok(Some((name.clone(), "default".to_owned()))),
        _ => bail!(
            "more than one profile has default = true: {}",
            defaults.join(", ")
        ),
    }
}

pub(crate) fn expand_home(dir: &str, home: Option<&Path>) -> PathBuf {
    match (
        dir.strip_prefix("~/")
            .or(dir.strip_prefix("~").filter(|r| r.is_empty())),
        home,
    ) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(dir),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
        default_view = "for-me"
        bots = ["ci-user"]

        [team_reviews]
        teams = ["me/personal-team"]

        [views.mine]
        query = "author:@me"

        [profiles.work]
        dirs = ["~/work"]
        email = "me@work.example"
        gh_user = "me-work"
        default_view = "robots"

        [profiles.work.team_reviews]
        teams = ["acme/robotics"]

        [profiles.work.views.robots]
        query = "org:acme"

        [profiles.work-infra]
        dirs = ["~/work/infra"]
        token_env = "INFRA_TOKEN"

        [profiles.personal]
        gh_user = "me"
        default = true
    "#;

    fn here(cwd: &str) -> Whereabouts {
        Whereabouts {
            cwd: PathBuf::from(cwd),
            home: Some(PathBuf::from("/home/me")),
            ..Whereabouts::default()
        }
    }

    fn pick(here: &Whereabouts) -> Result<(Option<Profile>, toml::Table)> {
        let mut root: toml::Table = toml::from_str(CONFIG).unwrap();
        let profile = apply(&mut root, here)?;
        Ok((profile, root))
    }

    fn name(here: &Whereabouts) -> String {
        pick(here).unwrap().0.map(|p| p.name).unwrap_or_default()
    }

    #[test]
    fn picks_by_directory_longest_first() {
        assert_eq!(name(&here("/home/me/work/robot")), "work");
        assert_eq!(name(&here("/home/me/work/infra/x")), "work-infra");
        assert_eq!(
            name(&here("/home/me/workshop")),
            "personal",
            "not a prefix match on names"
        );
        assert_eq!(name(&here("/home/me/code")), "personal");
    }

    #[test]
    fn explicit_and_git_config_win() {
        let mut h = here("/home/me/work");
        h.git_profile = Some("personal".into());
        assert_eq!(name(&h), "personal");
        h.explicit = Some("work-infra".into());
        assert_eq!(name(&h), "work-infra");

        h.explicit = Some("nope".into());
        let err = pick(&h).unwrap_err().to_string();
        assert!(
            err.contains("unknown profile \"nope\"") && err.contains("personal"),
            "{err}"
        );
    }

    #[test]
    fn email_matches_outside_the_listed_directories() {
        let mut h = here("/tmp/clone");
        h.git_email = Some("ME@work.example".into());
        let (profile, _) = pick(&h).unwrap();
        let profile = profile.unwrap();
        assert_eq!(profile.name, "work");
        assert!(profile.reason.contains("user.email"));
        // Directories come first: in ~/work (not a repo) git reports the
        // personal email, and that mustn't pull in the personal profile.
        let mut h = here("/home/me/work");
        h.git_email = Some("me@personal.example".into());
        assert_eq!(name(&h), "work");
    }

    #[test]
    fn profile_settings_override_and_merge() {
        let (profile, root) = pick(&here("/home/me/work")).unwrap();
        let profile = profile.unwrap();
        assert_eq!(profile.gh_user.as_deref(), Some("me-work"));
        assert_eq!(profile.reason, "directory ~/work");
        assert_eq!(root["default_view"].as_str(), Some("robots"));
        assert_eq!(
            root["bots"][0].as_str(),
            Some("ci-user"),
            "untouched settings stay"
        );
        assert_eq!(
            root["team_reviews"]["teams"][0].as_str(),
            Some("acme/robotics"),
            "lists replace"
        );
        let views = root["views"].as_table().unwrap();
        assert!(
            views.contains_key("mine") && views.contains_key("robots"),
            "tables merge"
        );
        assert!(!root.contains_key("profiles") && !root.contains_key("gh_user"));
    }

    #[test]
    fn no_profiles_means_no_profile() {
        let mut root: toml::Table = toml::from_str("bots = []").unwrap();
        assert_eq!(apply(&mut root, &here("/")).unwrap(), None);
        let h = Whereabouts {
            explicit: Some("work".into()),
            ..here("/")
        };
        assert!(
            apply(&mut root, &h).is_err(),
            "asking for one that can't exist"
        );
    }

    #[test]
    fn rejects_bad_profile_values() {
        for bad in [
            "[profiles.a]\ndefault = true\n[profiles.b]\ndefault = true",
            "[profiles.a]\ndirs = \"~/work\"\ndefault = true",
            "[profiles.a]\ngh_user = 3\ndefault = true",
            "profiles = 1",
        ] {
            let mut root: toml::Table = toml::from_str(bad).unwrap();
            assert!(apply(&mut root, &here("/home/me/work")).is_err(), "{bad}");
        }
    }

    #[test]
    fn home_expansion() {
        let home = Some(Path::new("/home/me"));
        assert_eq!(expand_home("~/work", home), PathBuf::from("/home/me/work"));
        assert_eq!(expand_home("~", home), PathBuf::from("/home/me"));
        assert_eq!(expand_home("/abs", home), PathBuf::from("/abs"));
        assert_eq!(expand_home("~other", home), PathBuf::from("~other"));
    }
}
