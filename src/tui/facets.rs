//! The facets panel: what's in the view, by repo, author, label, ...

use std::collections::BTreeMap;

use crate::filter::{Mark, mark};
use crate::model::{CiState, Pr, ReviewDecision};

pub struct Group {
    pub key: &'static str,
    pub title: &'static str,
    pub values: Vec<Value>,
}

pub struct Value {
    pub value: String,
    /// How many of the shown PRs have this value.
    pub count: usize,
    pub mark: Mark,
}

/// Reads a PR's values for one facet.
type ValuesOf = fn(&Pr) -> Vec<String>;

/// Facet keys, their titles, and how to read each PR's values.
const FACETS: &[(&str, &str, ValuesOf)] = &[
    ("turn", "Turn", |pr| vec![pr.turn.label().to_owned()]),
    ("repo", "Repository", |pr| vec![pr.repo.to_string()]),
    ("author", "Author", |pr| {
        vec![pr.author.clone().unwrap_or_else(|| "ghost".to_owned())]
    }),
    ("label", "Label", |pr| pr.labels.clone()),
    ("ci", "CI", |pr| {
        vec![
            match pr.ci {
                CiState::Pass => "pass",
                CiState::Fail => "fail",
                CiState::Pending => "pending",
                CiState::None => "none",
            }
            .to_owned(),
        ]
    }),
    ("review", "Review", |pr| {
        vec![
            match pr.review_decision {
                Some(ReviewDecision::Approved) => "approved",
                Some(ReviewDecision::ChangesRequested) => "changes",
                Some(ReviewDecision::ReviewRequired) => "required",
                None => "none",
            }
            .to_owned(),
        ]
    }),
];

/// Values come from `all` so that excluded ones stay visible (and can be
/// brought back); counts come from `shown`. Most common first.
pub fn groups(all: &[Pr], shown: &[Pr], filter_text: &str) -> Vec<Group> {
    FACETS
        .iter()
        .map(|(key, title, values_of)| {
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for pr in all {
                for value in values_of(pr) {
                    // Commas separate alternatives in the filter, so these can't be expressed.
                    if !value.contains(',') {
                        counts.entry(value).or_default();
                    }
                }
            }
            for pr in shown {
                for value in values_of(pr) {
                    if let Some(count) = counts.get_mut(&value) {
                        *count += 1;
                    }
                }
            }
            let mut values: Vec<Value> = counts
                .into_iter()
                .map(|(value, count)| Value {
                    mark: mark(filter_text, key, &value),
                    value,
                    count,
                })
                .collect();
            values.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
            Group { key, title, values }
        })
        .filter(|group| !group.values.is_empty())
        .collect()
}

/// The `(key, value)` of the `index`th value across all groups.
pub fn nth(groups: &[Group], index: usize) -> Option<(&'static str, &str)> {
    groups
        .iter()
        .flat_map(|g| g.values.iter().map(move |v| (g.key, v.value.as_str())))
        .nth(index)
}

pub fn len(groups: &[Group]) -> usize {
    groups.iter().map(|g| g.values.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Turn;
    use crate::test_support::pr;

    #[test]
    fn counts_shown_prs_but_lists_every_value() {
        let mut a = pr(1);
        a.labels = vec!["bug".into()];
        let mut b = pr(2);
        b.repo = "acme/web".parse().unwrap();
        b.turn = Turn::Author;
        b.labels = vec!["bug".into(), "ui".into()];
        let all = vec![a.clone(), b.clone()];
        let shown = vec![a];

        let groups = groups(&all, &shown, "-repo:acme/web");
        let repo = groups.iter().find(|g| g.key == "repo").unwrap();
        let repos: Vec<(&str, usize, Mark)> = repo
            .values
            .iter()
            .map(|v| (v.value.as_str(), v.count, v.mark))
            .collect();
        assert_eq!(
            repos,
            [("acme/api", 1, Mark::None), ("acme/web", 0, Mark::Excluded)]
        );

        let labels = groups.iter().find(|g| g.key == "label").unwrap();
        let names: Vec<&str> = labels.values.iter().map(|v| v.value.as_str()).collect();
        assert_eq!(names, ["bug", "ui"]);

        assert_eq!(nth(&groups, 0), Some(("turn", "me")));
        assert_eq!(nth(&groups, 2), Some(("repo", "acme/api")));
        assert_eq!(len(&groups), 2 + 2 + 1 + 2 + 1 + 1);
    }
}
