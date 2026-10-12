//! Reading setup's answers out of a config document and writing them back,
//! touching only the keys setup owns: comments, custom views, weights, and
//! anything else in the file stay exactly as they were.

use std::fmt;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, eyre};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use super::SETUP_VERSION;
use crate::config;
use crate::profile::Whereabouts;

/// The start of a config file setup creates (and all of one it skips).
pub const STARTER: &str = "\
# ghpr configuration. Every setting is optional; the README's Configuration
# and Profiles sections describe them all. `ghpr config init` runs the guided
# setup again: it keeps your edits and comments, and backs up this file first.
";

/// Where a profile's token comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    /// `gh auth token --user LOGIN`
    Gh(String),
    /// An environment variable.
    Env(String),
    /// Neither, in an existing profile (named here): `GH_TOKEN`,
    /// `GITHUB_TOKEN`, or gh's active account.
    Unpinned(String),
}

impl Source {
    pub fn gh_user(&self) -> Option<&str> {
        match self {
            Source::Gh(login) => Some(login),
            _ => None,
        }
    }

    pub fn token_env(&self) -> Option<&str> {
        match self {
            Source::Env(var) => Some(var),
            _ => None,
        }
    }

    /// A profile name to suggest for it.
    pub fn suggested_name(&self) -> String {
        match self {
            Source::Gh(login) => login.clone(),
            Source::Env(var) => var
                .trim_end_matches("_TOKEN")
                .trim_end_matches("_GITHUB")
                .to_ascii_lowercase()
                .replace('_', "-"),
            Source::Unpinned(name) => name.clone(),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Source::Gh(login) => write!(f, "{login}"),
            Source::Env(var) => write!(f, "${var}"),
            Source::Unpinned(_) => write!(f, "no account pinned"),
        }
    }
}

/// One profile as setup will write it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileAnswer {
    pub name: String,
    /// Its name in the existing file, if it was there already.
    pub previous: Option<String>,
    pub source: Source,
    pub default: bool,
    /// Only asked for (and written) when the profile isn't the default.
    pub dirs: Vec<String>,
    pub email: Option<String>,
    pub teams: Vec<String>,
    pub default_view: Option<String>,
    pub bots: Vec<String>,
}

impl ProfileAnswer {
    /// A profile for `source` that isn't in the file yet, starting from the
    /// file's top-level settings.
    pub fn new(source: Source, top: &Settings) -> Self {
        ProfileAnswer {
            name: source.suggested_name(),
            previous: None,
            source,
            default: false,
            dirs: Vec::new(),
            email: None,
            teams: top.teams.clone(),
            default_view: top.default_view.clone(),
            bots: top.bots.clone(),
        }
    }
}

/// The settings setup asks about, as the top level of a file sets them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub teams: Vec<String>,
    pub default_view: Option<String>,
    pub bots: Vec<String>,
}

/// What an existing file already says, to start the questions from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Existing {
    pub setup_version: u32,
    pub top: Settings,
    /// With the top-level settings filled in where a profile doesn't set its own.
    pub profiles: Vec<ProfileAnswer>,
}

impl Existing {
    pub fn read(doc: &DocumentMut) -> Existing {
        let root = doc.as_table();
        let top = Settings {
            teams: teams(root).unwrap_or_default(),
            default_view: string(root, "default_view"),
            bots: strings(root.get("bots")).unwrap_or_default(),
        };
        let profiles = root
            .get("profiles")
            .and_then(Item::as_table_like)
            .into_iter()
            .flat_map(|t| t.iter())
            .filter_map(|(name, item)| {
                let table = item.as_table_like()?;
                let source = match (string(table, "gh_user"), string(table, "token_env")) {
                    (_, Some(var)) => Source::Env(var),
                    (Some(login), None) => Source::Gh(login),
                    (None, None) => Source::Unpinned(name.to_owned()),
                };
                Some(ProfileAnswer {
                    name: name.to_owned(),
                    previous: Some(name.to_owned()),
                    source,
                    default: table.get("default").and_then(Item::as_bool) == Some(true),
                    dirs: strings(table.get("dirs")).unwrap_or_default(),
                    email: string(table, "email"),
                    teams: teams(table).unwrap_or_else(|| top.teams.clone()),
                    default_view: string(table, "default_view")
                        .or_else(|| top.default_view.clone()),
                    bots: strings(table.get("bots")).unwrap_or_else(|| top.bots.clone()),
                })
            })
            .collect();
        Existing {
            setup_version: root
                .get("setup_version")
                .and_then(Item::as_integer)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0),
            top,
            profiles,
        }
    }

    pub fn profile(&self, source: &Source) -> Option<&ProfileAnswer> {
        self.profiles.iter().find(|p| &p.source == source)
    }
}

fn string(table: &dyn TableLike, key: &str) -> Option<String> {
    table.get(key).and_then(Item::as_str).map(str::to_owned)
}

fn strings(item: Option<&Item>) -> Option<Vec<String>> {
    let array = item?.as_array()?;
    Some(
        array
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
    )
}

fn teams(table: &dyn TableLike) -> Option<Vec<String>> {
    strings(table.get("team_reviews")?.as_table_like()?.get("teams"))
}

/// A fresh document: the starter comment, to which setup adds its keys.
pub fn new_document() -> DocumentMut {
    format!("{STARTER}setup_version = 0\n")
        .parse()
        .expect("the starter config is valid TOML")
}

/// Write the answers into `doc`. Profiles that aren't among them are removed;
/// renamed ones keep everything else they had. The top-level settings setup
/// asks about move into the profiles, which now set them all.
pub fn apply(doc: &mut DocumentMut, profiles: &[ProfileAnswer]) {
    let root = doc.as_table_mut();
    // Missing, or not a table (which ghpr rejects anyway).
    if root.get("profiles").and_then(Item::as_table_like).is_none() {
        let mut table = Table::new();
        table.set_implicit(true);
        root.insert("profiles", Item::Table(table));
    }
    let table = root
        .get_mut("profiles")
        .and_then(Item::as_table_like_mut)
        .expect("just made sure");

    let keep: Vec<&str> = profiles
        .iter()
        .filter_map(|p| p.previous.as_deref())
        .collect();
    let names: Vec<String> = table.iter().map(|(k, _)| k.to_owned()).collect();
    for name in &names {
        if !keep.contains(&name.as_str()) {
            table.remove(name);
        }
    }
    // Take renamed ones out first, so swapping two names works.
    let renamed: Vec<(String, Item)> = profiles
        .iter()
        .filter_map(|p| {
            let old = p.previous.as_deref().filter(|old| *old != p.name)?;
            Some((p.name.clone(), table.remove(old)?))
        })
        .collect();
    for (name, item) in renamed {
        table.insert(&name, item);
    }

    for answer in profiles {
        if !table.contains_key(&answer.name) {
            table.insert(&answer.name, Item::Table(Table::new()));
        }
        let item = table.get_mut(&answer.name).expect("just inserted");
        if item.as_table_like().is_none() {
            *item = Item::Table(Table::new());
        }
        write_profile(item, answer);
    }

    let root = doc.as_table_mut();
    let first_time = !root.contains_key("setup_version");
    // First, so comments above the keys removed next have somewhere to go.
    set(
        root,
        "setup_version",
        Some(Value::from(i64::from(SETUP_VERSION))),
    );
    if first_time {
        space_before_first_table(root);
    }
    remove(root, "default_view");
    remove(root, "bots");
    if let Some(reviews) = root
        .get_mut("team_reviews")
        .and_then(Item::as_table_like_mut)
    {
        remove(reviews, "teams");
        if reviews.is_empty() {
            root.remove("team_reviews");
        }
    }
}

/// A blank line between the top-level keys and the first table, which may
/// not have had keys above it before.
fn space_before_first_table(root: &mut Table) {
    fn positions(table: &Table, out: &mut Vec<isize>) {
        for (_, item) in table.iter() {
            if let Item::Table(t) = item {
                out.extend(t.position().filter(|_| !t.is_implicit()));
                positions(t, out);
            }
        }
    }
    fn space(table: &mut Table, first: isize) {
        for (_, item) in table.iter_mut() {
            if let Item::Table(t) = item {
                if t.position() == Some(first) && !t.is_implicit() {
                    let decor = t.decor_mut();
                    let prefix = decor.prefix().and_then(|p| p.as_str()).unwrap_or("");
                    if !prefix.starts_with('\n') {
                        decor.set_prefix(format!("\n{prefix}"));
                    }
                }
                space(t, first);
            }
        }
    }
    let mut all = Vec::new();
    positions(root, &mut all);
    if let Some(first) = all.into_iter().min() {
        space(root, first);
    }
}

fn write_profile(item: &mut Item, answer: &ProfileAnswer) {
    let is_table = item.is_table();
    let profile = item.as_table_like_mut().expect("checked by the caller");
    set(profile, "gh_user", answer.source.gh_user().map(Value::from));
    set(
        profile,
        "token_env",
        answer.source.token_env().map(Value::from),
    );
    set(
        profile,
        "default",
        answer.default.then(|| Value::from(true)),
    );
    if !answer.default {
        set(profile, "dirs", list(&answer.dirs));
        set(profile, "email", answer.email.as_deref().map(Value::from));
    }
    set(
        profile,
        "default_view",
        answer.default_view.as_deref().map(Value::from),
    );
    set(profile, "bots", list(&answer.bots));

    let teams = list(&answer.teams);
    match profile.get_mut("team_reviews") {
        Some(reviews) if reviews.as_table_like().is_some() => {
            let reviews = reviews.as_table_like_mut().expect("checked");
            set(reviews, "teams", teams);
            if reviews.is_empty() {
                profile.remove("team_reviews");
            }
        }
        _ => match teams {
            Some(teams) if is_table => {
                let mut reviews = Table::new();
                reviews.insert("teams", Item::Value(teams));
                profile.insert("team_reviews", Item::Table(reviews));
            }
            Some(teams) => {
                let mut reviews = InlineTable::new();
                reviews.insert("teams", teams);
                profile.insert("team_reviews", Item::Value(Value::InlineTable(reviews)));
            }
            None => {
                profile.remove("team_reviews");
            }
        },
    }
}

fn list(items: &[String]) -> Option<Value> {
    (!items.is_empty()).then(|| Value::Array(items.iter().collect::<Array>()))
}

/// Set or remove `key`. An unchanged value is left alone, and a changed one
/// keeps its comment.
fn set(table: &mut dyn TableLike, key: &str, value: Option<Value>) {
    let Some(mut value) = value else {
        remove(table, key);
        return;
    };
    // Assigned in place: `insert` would reset the key, and the comment above it.
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            if !same(old, &value) {
                *value.decor_mut() = old.decor().clone();
                *old = value;
            }
        }
        Some(old) => *old = Item::Value(value),
        None => {
            table.insert(key, Item::Value(value));
        }
    }
}

/// Remove `key`, keeping any comment lines above it (they may well be about
/// the whole file) by moving them to the next key.
fn remove(table: &mut dyn TableLike, key: &str) {
    let comment = table
        .key(key)
        .and_then(|k| k.leaf_decor().prefix())
        .and_then(|p| p.as_str())
        .filter(|p| p.contains('#'))
        .map(str::to_owned);
    let next = table
        .iter()
        .skip_while(|(k, _)| *k != key)
        .skip(1)
        .find(|(_, item)| item.is_value())
        .map(|(k, _)| k.to_owned());
    table.remove(key);
    if let (Some(comment), Some(next)) = (comment, next)
        && let Some(mut next) = table.key_mut(&next)
    {
        let decor = next.leaf_decor_mut();
        let own = decor
            .prefix()
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_owned();
        decor.set_prefix(format!("{comment}{own}"));
    }
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        (Value::Integer(a), Value::Integer(b)) => a.value() == b.value(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same(a, b))
        }
        _ => false,
    }
}

/// Check that ghpr will accept the file, whichever profile ends up in effect.
pub fn validate(text: &str, profiles: &[ProfileAnswer]) -> Result<()> {
    config::parse_with(text, &Whereabouts::default())?;
    for profile in profiles {
        let here = Whereabouts {
            explicit: Some(profile.name.clone()),
            ..Whereabouts::default()
        };
        config::parse_with(text, &here).wrap_err_with(|| format!("profile {}", profile.name))?;
    }
    Ok(())
}

/// Where a backup of `path` made now goes: `config.toml.bak-20261012-093000`.
pub fn backup_path(path: &Path, now: &jiff::Zoned) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "config.toml".into(), |n| n.to_string_lossy());
    path.with_file_name(format!("{name}.bak-{}", now.strftime("%Y%m%d-%H%M%S")))
}

/// Write `text` to `path` atomically, backing up what was there. Returns the
/// backup's path.
pub fn write(path: &Path, text: &str) -> Result<Option<PathBuf>> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).wrap_err_with(|| format!("could not create {}", dir.display()))?;
    let backup = if path.exists() {
        let backup = backup_path(path, &jiff::Zoned::now());
        std::fs::copy(path, &backup).wrap_err_with(|| {
            format!(
                "could not back up {} to {}",
                path.display(),
                backup.display()
            )
        })?;
        Some(backup)
    } else {
        None
    };
    let name = path
        .file_name()
        .ok_or_else(|| eyre!("invalid config path {}", path.display()))?;
    let tmp = dir.join(format!(".{}.tmp", name.to_string_lossy()));
    std::fs::write(&tmp, text).wrap_err_with(|| format!("could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).wrap_err_with(|| format!("could not write {}", path.display()))?;
    tracing::info!(path = %path.display(), backup = ?backup, "wrote config");
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HAND_WRITTEN: &str = r#"# my ghpr config
default_view = "backend" # the one I use most
bots = ["ci-user"]
refresh_interval = "2m"

[team_reviews]
teams = ["acme/platform"]
count_as_mine = true

[score]
turn_me = 200 # really, me first

[views.backend]
query = "is:open org:acme label:backend"

[profiles.old]
gh_user = "someone-else"
"#;

    fn doc(text: &str) -> DocumentMut {
        text.parse().unwrap()
    }

    fn answer(name: &str, source: Source) -> ProfileAnswer {
        ProfileAnswer::new(source, &Settings::default()).with_name(name)
    }

    impl ProfileAnswer {
        fn with_name(mut self, name: &str) -> Self {
            self.name = name.to_owned();
            self
        }
    }

    #[test]
    fn reads_existing_settings_with_profile_overrides() {
        let existing = Existing::read(&doc(r#"
            setup_version = 1
            bots = ["ci-user"]
            [team_reviews]
            teams = ["acme/platform"]
            [profiles.work]
            gh_user = "me-work"
            dirs = ["~/work"]
            email = "me@work.example"
            team_reviews.teams = ["acme/robotics"]
            [profiles.ci]
            token_env = "CI_TOKEN"
            default_view = "mine"
            [profiles.personal]
            default = true
            "#));
        assert_eq!(existing.setup_version, 1);
        assert_eq!(existing.top.teams, ["acme/platform"]);
        let [work, ci, personal] = existing.profiles.as_slice() else {
            panic!("{existing:?}")
        };
        assert_eq!(work.source, Source::Gh("me-work".into()));
        assert_eq!(work.teams, ["acme/robotics"], "the profile's own value");
        assert_eq!(work.bots, ["ci-user"], "the top-level value");
        assert_eq!(work.email.as_deref(), Some("me@work.example"));
        assert_eq!(ci.source, Source::Env("CI_TOKEN".into()));
        assert_eq!(ci.default_view.as_deref(), Some("mine"));
        assert_eq!(personal.source, Source::Unpinned("personal".into()));
        assert!(personal.default && !work.default);
        assert_eq!(Existing::read(&doc("")).setup_version, 0);
    }

    #[test]
    fn keeps_everything_it_doesnt_own() {
        let mut d = doc(HAND_WRITTEN);
        let existing = Existing::read(&d);
        let mut personal =
            ProfileAnswer::new(Source::Gh("me".into()), &existing.top).with_name("personal");
        personal.default = true;
        let mut work = answer("work", Source::Gh("me-work".into()));
        work.dirs = vec!["~/work".into()];
        work.teams = vec!["acme/robotics".into()];
        let profiles = [personal, work];
        apply(&mut d, &profiles);
        let text = d.to_string();
        validate(&text, &profiles).unwrap();
        insta::assert_snapshot!(text);

        for kept in [
            "# my ghpr config",
            "turn_me = 200 # really, me first",
            "[views.backend]",
            "refresh_interval = \"2m\"",
            "count_as_mine = true",
        ] {
            assert!(text.contains(kept), "lost {kept:?}:\n{text}");
        }
        assert!(!text.contains("[profiles.old]"), "removed profiles go");
        let config = config::parse_with(&text, &Whereabouts::default()).unwrap();
        assert_eq!(config.setup_version, Some(SETUP_VERSION));
        assert_eq!(config.profile.unwrap().name, "personal");
        assert_eq!(
            config.default_view.as_deref(),
            Some("backend"),
            "moved into the profile"
        );
        assert_eq!(config.team_reviews.teams, ["acme/platform"]);
        assert_eq!(config.bots, ["ci-user"]);
    }

    #[test]
    fn rerun_changes_only_what_changed() {
        let mut d = doc(HAND_WRITTEN);
        let mut personal = answer("personal", Source::Gh("me".into()));
        personal.default = true;
        apply(&mut d, std::slice::from_ref(&personal));
        let once = d.to_string();

        // Again, renaming the profile and changing one value with a comment.
        let mut d = doc(&once.replace(
            "gh_user = \"me\"",
            "gh_user = \"me\" # my own account\nextra = 1",
        ));
        let existing = Existing::read(&d);
        let mut again = existing.profiles[0].clone();
        again.name = "home".into();
        again.source = Source::Gh("me2".into());
        apply(&mut d, &[again]);
        let twice = d.to_string();
        assert!(
            twice.contains("[profiles.home]\ngh_user = \"me2\" # my own account\nextra = 1"),
            "{twice}"
        );
        assert!(!twice.contains("personal"), "{twice}");
    }

    #[test]
    fn writes_a_fresh_file() {
        let mut d = new_document();
        let mut personal = answer("personal", Source::Gh("me".into()));
        personal.default = true;
        personal.teams = vec!["me/team".into()];
        let mut work = answer("work", Source::Env("WORK_TOKEN".into()));
        work.dirs = vec!["~/work".into(), "~/clients".into()];
        work.email = Some("me@work.example".into());
        work.default_view = Some("mine".into());
        work.bots = vec!["jenkins".into()];
        let profiles = [personal, work];
        apply(&mut d, &profiles);
        let text = d.to_string();
        validate(&text, &profiles).unwrap();
        insta::assert_snapshot!(text);
    }

    #[test]
    fn swaps_names_and_handles_inline_profiles() {
        let mut d =
            doc("[profiles]\na = { gh_user = \"x\" }\nb = { gh_user = \"y\", default = true }\n");
        let existing = Existing::read(&d);
        let mut a = existing.profiles[0].clone();
        a.name = "b".into();
        a.teams = vec!["o/t".into()];
        let mut b = existing.profiles[1].clone();
        b.name = "a".into();
        let profiles = [a, b];
        apply(&mut d, &profiles);
        let text = d.to_string();
        validate(&text, &profiles).unwrap();
        let config = config::parse_with(&text, &Whereabouts::default()).unwrap();
        let profile = config.profile.unwrap();
        assert_eq!(
            (profile.name.as_str(), profile.gh_user.as_deref()),
            ("a", Some("y")),
            "{text}"
        );
        let here = Whereabouts {
            explicit: Some("b".into()),
            ..Whereabouts::default()
        };
        let config = config::parse_with(&text, &here).unwrap();
        assert_eq!(config.team_reviews.teams, ["o/t"], "{text}");
    }

    #[test]
    fn rejects_what_ghpr_would() {
        let mut d = new_document();
        let mut p = answer("p", Source::Gh("me".into()));
        p.default = true;
        p.teams = vec!["no-slash".into()];
        apply(&mut d, std::slice::from_ref(&p));
        assert!(validate(&d.to_string(), &[p]).is_err());
    }

    #[test]
    fn writes_atomically_with_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        assert_eq!(write(&path, "a = 1\n").unwrap(), None, "nothing to back up");
        let backup = write(&path, "a = 2\n").unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 2\n");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "a = 1\n");
        assert!(
            backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("config.toml.bak-")
        );
        let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "no temp file left behind: {left:?}");
    }

    #[test]
    fn backup_names_carry_the_time() {
        let now: jiff::Zoned = "2026-10-12T09:30:05[UTC]".parse().unwrap();
        assert_eq!(
            backup_path(Path::new("/c/config.toml"), &now),
            PathBuf::from("/c/config.toml.bak-20261012-093005")
        );
    }
}
