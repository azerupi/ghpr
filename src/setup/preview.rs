//! What the preview pane works out from the answers so far: where each profile
//! applies, which accounts look like bots, and what a view would show.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::write::ProfileAnswer;
use crate::fetch::Fetched;
use crate::filter::{self, Filter};
use crate::model::{Pr, Reviewer, Role, ScoreWeights};
use crate::profile::{self, Whereabouts, expand_home};
use crate::sort::SortSpec;
use crate::views::View;

/// Which profile applies at one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub place: String,
    /// The profile and why, `None` when no profile applies.
    pub outcome: Result<Option<(String, String)>, String>,
}

/// Where each profile in the draft `text` applies: here, under each listed
/// directory, in repos with each listed email, and anywhere else.
pub fn routes(text: &str, profiles: &[ProfileAnswer], here: &Whereabouts) -> Vec<Route> {
    let table: toml::Table = match toml::from_str(text) {
        Ok(table) => table,
        Err(e) => {
            return vec![Route {
                place: "config".into(),
                outcome: Err(e.to_string()),
            }];
        }
    };
    let home = here.home.as_deref();
    let elsewhere = Whereabouts {
        cwd: PathBuf::new(),
        home: here.home.clone(),
        ..Whereabouts::default()
    };
    let mut places = vec![(
        format!("here ({})", tilde(&here.cwd, home)),
        Whereabouts {
            explicit: None,
            ..here.clone()
        },
    )];
    for p in profiles.iter().filter(|p| !p.default) {
        for dir in &p.dirs {
            places.push((
                dir.clone(),
                Whereabouts {
                    cwd: expand_home(dir, home),
                    ..elsewhere.clone()
                },
            ));
        }
        if let Some(email) = &p.email {
            places.push((
                format!("repos with user.email {email}"),
                Whereabouts {
                    git_email: Some(email.clone()),
                    ..elsewhere.clone()
                },
            ));
        }
    }
    places.push(("anywhere else".into(), elsewhere));

    places
        .into_iter()
        .map(|(place, at)| Route {
            place,
            outcome: profile::apply(&mut table.clone(), &at)
                .map(|p| p.map(|p| (p.name, p.reason)))
                .map_err(|e| e.to_string()),
        })
        .collect()
}

/// The listed directories that don't exist.
pub fn missing_dirs(
    dirs: &[String],
    home: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<String> {
    dirs.iter()
        .filter(|d| !exists(&expand_home(d, home)))
        .cloned()
        .collect()
}

/// `path` with the home directory shown as `~`.
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// A user account that looks automated, and a PR it shows up on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub login: String,
    pub seen_on: String,
}

/// Accounts among the PRs' authors, latest actors, and requested reviewers
/// whose login looks automated. GitHub App bots are left out: ghpr already
/// knows those.
pub fn bot_suggestions<'a>(prs: impl IntoIterator<Item = &'a Pr>, viewer: &str) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Vec::new();
    for pr in prs {
        let mut logins: Vec<&str> = Vec::new();
        if let Some(author) = pr.author.as_deref().filter(|_| !pr.author_is_bot) {
            logins.push(author);
        }
        if let Some(activity) = pr
            .last_activity
            .as_ref()
            .filter(|a| !matches!(a.role, Role::Bot | Role::Me))
        {
            logins.push(&activity.who);
        }
        for reviewer in &pr.requested_reviewers {
            if let Reviewer::User(login) = reviewer {
                logins.push(login);
            }
        }
        for login in logins {
            if looks_automated(login)
                && !login.eq_ignore_ascii_case(viewer)
                && !out.iter().any(|s| s.login.eq_ignore_ascii_case(login))
            {
                out.push(Suggestion {
                    login: login.to_owned(),
                    seen_on: format!("{}#{}", pr.repo, pr.number),
                });
            }
        }
    }
    out
}

fn looks_automated(login: &str) -> bool {
    const WORDS: &[&str] = &[
        "bot",
        "bots",
        "ci",
        "cd",
        "svc",
        "service",
        "automation",
        "automated",
        "jenkins",
        "build",
        "builder",
        "deploy",
        "deployer",
        "release",
        "robot",
        "machine",
    ];
    let login = login.to_ascii_lowercase();
    if login.ends_with("[bot]") {
        return false;
    }
    login.ends_with("bot")
        || login.contains("jenkins")
        || login.contains("automation")
        || login
            .split(['-', '_', '.'])
            .any(|word| WORDS.contains(&word))
}

/// A view's PRs as the preview shows them.
#[derive(Debug, Clone)]
pub struct Sample {
    pub view: View,
    /// Everything fetched, for the cache.
    pub all: Vec<Pr>,
    /// After the view's filter, in its sort order.
    pub shown: Vec<Pr>,
    pub viewer: String,
    /// Some search had more matches than were fetched.
    pub truncated: bool,
    pub at: Timestamp,
}

impl Sample {
    pub fn new(view: View, fetched: Fetched, weights: &ScoreWeights, at: Timestamp) -> Sample {
        let ctx = filter::Context {
            viewer: &fetched.viewer,
            now: at,
            local: None,
        };
        let mut shown: Vec<Pr> = match Filter::parse(&view.filter) {
            Ok(filter) => fetched
                .prs
                .iter()
                .filter(|pr| filter.shows(pr, &ctx))
                .cloned()
                .collect(),
            Err(_) => fetched.prs.clone(),
        };
        if let Ok(sort) = SortSpec::parse(&view.sort) {
            sort.sort(&mut shown, weights, at);
        }
        Sample {
            view,
            truncated: !fetched.truncated.is_empty(),
            all: fetched.prs,
            shown,
            viewer: fetched.viewer,
            at,
        }
    }
}

/// The searches behind a view, in words.
pub fn describe(view: &View, teams: usize) -> String {
    let teams = match teams {
        0 => String::new(),
        1 => ", + 1 team".to_owned(),
        n => format!(", + {n} teams"),
    };
    match view.name.as_str() {
        "for-me" if view.searches.len() > 1 => format!(
            "review requests, your PRs, and PRs you reviewed or were mentioned in when it's your turn{teams}"
        ),
        "mine" if view.searches.len() == 1 && view.searches[0].query.contains("author:@me") => {
            "your open PRs".to_owned()
        }
        "repo"
            if view.searches.len() == 1 && view.searches[0].query.contains("sort:updated-desc") =>
        {
            let repos: Vec<&str> = view.searches[0]
                .query
                .split_whitespace()
                .filter_map(|t| t.strip_prefix("repo:"))
                .collect();
            format!("open PRs in {}", repos.join(", "))
        }
        "org" if view.searches.len() == 1 && view.searches[0].query.contains(" org:") => {
            let owners: Vec<&str> = view.searches[0]
                .query
                .split_whitespace()
                .filter_map(|t| t.strip_prefix("org:"))
                .collect();
            format!("open PRs in every repo of {}", owners.join(", "))
        }
        _ => format!(
            "search: {}",
            view.searches
                .iter()
                .map(|s| s.query.as_str())
                .collect::<Vec<_>>()
                .join(" + ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::model::Activity;
    use crate::setup::write::{self, Settings, Source};
    use crate::test_support::pr;

    fn profile(name: &str, default: bool, dirs: &[&str], email: Option<&str>) -> ProfileAnswer {
        let mut p = ProfileAnswer::new(Source::Gh(name.into()), &Settings::default());
        p.name = name.into();
        p.default = default;
        p.dirs = dirs.iter().map(|d| (*d).to_owned()).collect();
        p.email = email.map(Into::into);
        p
    }

    fn draft(profiles: &[ProfileAnswer]) -> String {
        let mut doc = write::new_document();
        write::apply(&mut doc, profiles);
        doc.to_string()
    }

    #[test]
    fn routes_show_where_each_profile_applies() {
        let profiles = [
            profile("personal", true, &[], None),
            profile("work", false, &["~/work"], Some("me@work.example")),
        ];
        let here = Whereabouts {
            cwd: "/home/me/code/ghpr".into(),
            home: Some("/home/me".into()),
            ..Whereabouts::default()
        };
        let lines: Vec<String> = routes(&draft(&profiles), &profiles, &here)
            .into_iter()
            .map(|r| match r.outcome {
                Ok(Some((name, why))) => format!("{} → {name} ({why})", r.place),
                Ok(None) => format!("{} → none", r.place),
                Err(e) => format!("{} → error {e}", r.place),
            })
            .collect();
        assert_eq!(
            lines,
            [
                "here (~/code/ghpr) → personal (default)",
                "~/work → work (directory ~/work)",
                "repos with user.email me@work.example → work (git user.email me@work.example)",
                "anywhere else → personal (default)",
            ]
        );
    }

    #[test]
    fn finds_missing_dirs_and_shortens_home() {
        let home = Some(Path::new("/home/me"));
        let dirs = ["~/work".to_owned(), "/opt/x".to_owned()];
        let missing = missing_dirs(&dirs, home, &|p| p == Path::new("/home/me/work"));
        assert_eq!(missing, ["/opt/x"]);
        assert_eq!(tilde(Path::new("/home/me"), home), "~");
        assert_eq!(tilde(Path::new("/home/me/a"), home), "~/a");
        assert_eq!(tilde(Path::new("/tmp"), home), "/tmp");
    }

    #[test]
    fn suggests_user_accounts_that_look_automated() {
        let mut a = pr(1);
        a.author = Some("deploy-svc".into());
        let mut b = pr(2);
        b.author = Some("renovate[bot]".into());
        b.author_is_bot = true;
        b.last_activity = Some(Activity {
            who: "acme-ci".into(),
            ..a.last_activity.clone().unwrap()
        });
        b.requested_reviewers = vec![
            Reviewer::User("jenkins".into()),
            Reviewer::User("lucia".into()),
        ];
        let mut c = pr(3);
        c.author = Some("deploy-svc".into());
        let mut d = pr(4);
        d.last_activity.as_mut().unwrap().who = "metabot".into();
        d.last_activity.as_mut().unwrap().role = Role::Bot;

        let found = bot_suggestions([&a, &b, &c, &d], "me");
        let found: Vec<(&str, &str)> = found
            .iter()
            .map(|s| (s.login.as_str(), s.seen_on.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("deploy-svc", "acme/api#1"),
                ("acme-ci", "acme/api#2"),
                ("jenkins", "acme/api#2"),
            ],
            "app bots, people, and already-known bots are left out"
        );
        for person in ["alice", "lucia", "marcin", "abbott-x", "circle"] {
            assert!(!looks_automated(person), "{person}");
        }
    }

    #[test]
    fn describes_views() {
        let config = Config::default();
        let view = |name| {
            crate::views::resolve(Some(name), &[], &config, || vec!["o/r".parse().unwrap()])
                .unwrap()
        };
        assert_eq!(
            describe(&view("for-me"), 2),
            "review requests, your PRs, and PRs you reviewed or were mentioned in when it's your turn, + 2 teams"
        );
        assert_eq!(describe(&view("mine"), 0), "your open PRs");
        assert_eq!(describe(&view("repo"), 0), "open PRs in o/r");
        assert_eq!(describe(&view("org"), 0), "open PRs in every repo of o");
    }
}
