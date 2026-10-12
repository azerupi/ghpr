//! Turning a view name into the GitHub searches that populate it.

use color_eyre::eyre::{Result, bail};

use crate::config::Config;
use crate::model::RepoName;

const DEFAULT_VIEW: &str = "for-me";
const BUILTIN_VIEWS: &[&str] = &["for-me", "mine", "repo"];
const OPEN_PRS: &[&str] = &["is:pr", "is:open", "archived:false"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub name: String,
    pub searches: Vec<Search>,
    /// Initial client-side filter.
    pub filter: String,
    /// Initial sort.
    pub sort: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub query: String,
    pub keep: Keep,
}

/// Which results of a search end up in the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    All,
    /// Only PRs whose turn is `me`. Used for searches that match far more PRs
    /// than need attention, like everything the viewer ever reviewed.
    MyTurn,
    /// Team review requests: dropped in repos where team requests are ignored.
    Team,
}

/// Every view, in tab order: the built-ins, then the config's own.
pub fn names(config: &Config) -> Vec<String> {
    let mut names: Vec<String> = BUILTIN_VIEWS.iter().map(|v| (*v).to_owned()).collect();
    names.extend(
        config
            .views
            .keys()
            .filter(|k| !names.contains(k))
            .cloned()
            .collect::<Vec<_>>(),
    );
    names
}

/// Resolve `name` (or the default view), restricted to `repos` if any are given.
/// `current_repos` is only consulted by the `repo` view.
pub fn resolve(
    name: Option<&str>,
    repos: &[RepoName],
    config: &Config,
    current_repos: impl FnOnce() -> Vec<RepoName>,
) -> Result<View> {
    let name = name
        .or(config.default_view.as_deref())
        .unwrap_or(DEFAULT_VIEW)
        .to_owned();

    if let Some(view) = config.views.get(&name) {
        let mut terms: Vec<String> = view.query.split_whitespace().map(str::to_owned).collect();
        if !terms.iter().any(|t| t == "is:pr" || t == "type:pr") {
            terms.insert(0, "is:pr".to_owned());
        }
        return Ok(View {
            name,
            searches: vec![search(&terms.join(" "), repos, Keep::All)],
            filter: view.filter.clone(),
            sort: view.sort.clone().unwrap_or_else(|| "score".to_owned()),
        });
    }

    let open = OPEN_PRS.join(" ");
    let (searches, filter, sort) = match name.as_str() {
        "for-me" => {
            // GitHub drops you from the requested reviewers once you review, so
            // "requested from me" alone misses every PR waiting for a re-review.
            let mut searches = vec![
                search(
                    &format!("{open} user-review-requested:@me"),
                    repos,
                    Keep::All,
                ),
                search(&format!("{open} author:@me"), repos, Keep::All),
                search(&format!("{open} reviewed-by:@me"), repos, Keep::MyTurn),
                search(&format!("{open} mentions:@me"), repos, Keep::MyTurn),
            ];
            searches.extend(config.team_reviews.teams.iter().map(|team| {
                search(
                    &format!("{open} team-review-requested:{team}"),
                    repos,
                    Keep::Team,
                )
            }));
            (searches, "", "score")
        }
        "mine" => (
            vec![search(
                &format!("{open} author:@me sort:updated-desc"),
                repos,
                Keep::All,
            )],
            "",
            "turn,waiting",
        ),
        "repo" => {
            let repos = if repos.is_empty() {
                // A fork's PRs are mostly on its upstream; both are searched.
                let here = current_repos();
                if here.is_empty() {
                    bail!("not inside a GitHub repository; pass one with -R OWNER/REPO");
                }
                here
            } else {
                repos.to_vec()
            };
            (
                vec![search(
                    &format!("{open} sort:updated-desc"),
                    &repos,
                    Keep::All,
                )],
                "-draft",
                "waiting",
            )
        }
        _ => {
            let mut available: Vec<&str> = BUILTIN_VIEWS.to_vec();
            available.extend(config.views.keys().map(String::as_str));
            bail!(
                "unknown view {name:?} (available: {})",
                available.join(", ")
            );
        }
    };

    Ok(View {
        name,
        searches,
        filter: filter.to_owned(),
        sort: sort.to_owned(),
    })
}

fn search(base: &str, repos: &[RepoName], keep: Keep) -> Search {
    let mut query = base.to_owned();
    for repo in repos {
        query.push_str(&format!(" repo:{repo}"));
    }
    Search { query, keep }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{TeamReviews, ViewConfig};

    fn repo(s: &str) -> RepoName {
        s.parse().unwrap()
    }

    fn no_repo() -> Vec<RepoName> {
        Vec::new()
    }

    fn queries(view: &View) -> Vec<&str> {
        view.searches.iter().map(|s| s.query.as_str()).collect()
    }

    #[test]
    fn names_list_builtins_then_config_views() {
        let mut config = Config::default();
        for name in ["backend", "mine"] {
            config.views.insert(
                name.to_owned(),
                ViewConfig {
                    query: "org:acme".to_owned(),
                    filter: String::new(),
                    sort: None,
                },
            );
        }
        assert_eq!(names(&config), ["for-me", "mine", "repo", "backend"]);
    }

    #[test]
    fn default_view_is_for_me() {
        let view = resolve(None, &[], &Config::default(), no_repo).unwrap();
        assert_eq!(view.name, "for-me");
        assert_eq!(view.sort, "score");
        assert_eq!(
            queries(&view),
            [
                "is:pr is:open archived:false user-review-requested:@me",
                "is:pr is:open archived:false author:@me",
                "is:pr is:open archived:false reviewed-by:@me",
                "is:pr is:open archived:false mentions:@me",
            ]
        );
        let keeps: Vec<Keep> = view.searches.iter().map(|s| s.keep).collect();
        assert_eq!(keeps, [Keep::All, Keep::All, Keep::MyTurn, Keep::MyTurn]);
    }

    #[test]
    fn for_me_includes_configured_teams_and_repo_restrictions() {
        let config = Config {
            team_reviews: TeamReviews {
                teams: vec!["acme/platform".to_owned()],
                ..TeamReviews::default()
            },
            ..Config::default()
        };
        let view = resolve(Some("for-me"), &[repo("acme/api")], &config, no_repo).unwrap();
        assert_eq!(view.searches.len(), 5);
        assert_eq!(
            view.searches[4].query,
            "is:pr is:open archived:false team-review-requested:acme/platform repo:acme/api"
        );
        assert!(
            view.searches
                .iter()
                .all(|s| s.query.ends_with("repo:acme/api"))
        );
    }

    #[test]
    fn repo_view_uses_current_repo() {
        let view = resolve(Some("repo"), &[], &Config::default(), || vec![repo("o/r")]).unwrap();
        assert_eq!(
            queries(&view),
            ["is:pr is:open archived:false sort:updated-desc repo:o/r"]
        );
    }

    #[test]
    fn repo_view_in_a_fork_covers_upstream_and_the_fork() {
        let here = || vec![repo("ros2-rust/ros2_rust"), repo("me/ros2_rust")];
        let view = resolve(Some("repo"), &[], &Config::default(), here).unwrap();
        assert_eq!(
            queries(&view),
            [
                "is:pr is:open archived:false sort:updated-desc repo:ros2-rust/ros2_rust repo:me/ros2_rust"
            ]
        );
    }

    #[test]
    fn explicit_repos_win_over_current_repo() {
        let view = resolve(
            Some("repo"),
            &[repo("a/b"), repo("c/d")],
            &Config::default(),
            || panic!("should not look up the current repo"),
        )
        .unwrap();
        assert!(view.searches[0].query.ends_with("repo:a/b repo:c/d"));
    }

    #[test]
    fn repo_view_outside_a_repo_is_an_error() {
        let err = resolve(Some("repo"), &[], &Config::default(), no_repo).unwrap_err();
        assert!(err.to_string().contains("-R"), "{err}");
    }

    #[test]
    fn config_views_override_builtins_and_get_is_pr() {
        let mut config = Config::default();
        config.views.insert(
            "mine".to_owned(),
            ViewConfig {
                query: "is:open author:@me org:acme".to_owned(),
                filter: "-draft".to_owned(),
                sort: None,
            },
        );
        let view = resolve(Some("mine"), &[], &config, no_repo).unwrap();
        assert_eq!(queries(&view), ["is:pr is:open author:@me org:acme"]);
        assert_eq!(
            (view.filter.as_str(), view.sort.as_str()),
            ("-draft", "score")
        );
    }

    #[test]
    fn default_view_comes_from_config() {
        let config = Config {
            default_view: Some("mine".to_owned()),
            ..Config::default()
        };
        let view = resolve(None, &[], &config, no_repo).unwrap();
        assert_eq!(view.name, "mine");
    }

    #[test]
    fn unknown_view_lists_available_ones() {
        let err = resolve(Some("nope"), &[], &Config::default(), no_repo).unwrap_err();
        assert!(err.to_string().contains("for-me, mine, repo"), "{err}");
    }
}
