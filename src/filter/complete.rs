//! Suggestions for the word under the cursor in the filter and sort prompts.

use std::collections::BTreeMap;

use jiff::Timestamp;

use super::parse::{FLAGS, KEYS, ValueKind, enum_values};
use super::{Context, Filter};
use crate::model::{Pr, Reviewer};
use crate::state::LocalState;

/// Replace the characters `start..end` of the text with one of `candidates`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Char offsets into the text.
    pub start: usize,
    pub end: usize,
    /// What's been typed of the word so far (the text in `start..end`).
    pub typed: String,
    /// Completing a value (after `key:` or `,`) rather than a key.
    pub is_value: bool,
    /// The word starts with `-`; a lone `-` is waiting for the rest.
    pub negated: bool,
    /// Best first.
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub replacement: String,
    pub description: String,
    /// How many PRs the filter would match with this candidate, if meaningful.
    pub count: Option<usize>,
    /// Add a space after accepting (values and flags; keys end in `:` instead).
    pub space_after: bool,
}

impl Completion {
    /// The rest of the best candidate, if it extends what's typed. Shown dimmed.
    pub fn ghost(&self) -> Option<&str> {
        // After a space there's nothing to go on yet; a suggestion would just be noise.
        if self.typed.is_empty() && !self.is_value {
            return None;
        }
        let best = self.candidates.first()?;
        let rest = strip_prefix_ignore_case(&best.replacement, &self.typed)?;
        (!rest.is_empty()).then_some(rest)
    }

    /// Whether the word is still incomplete: some candidate extends it and none
    /// equals it. The filter isn't applied (or complained about) until it's done.
    pub fn is_incomplete(&self) -> bool {
        if self.typed.is_empty() && !self.is_value {
            return self.negated;
        }
        let extends = self.candidates.iter().any(|c| {
            strip_prefix_ignore_case(&c.replacement, &self.typed)
                .is_some_and(|rest| !rest.is_empty())
        });
        let exact = self
            .candidates
            .iter()
            .any(|c| c.replacement.eq_ignore_ascii_case(&self.typed));
        extends && !exact
    }

    /// The longest start shared by all candidates, if it's longer than what's typed.
    pub fn common_prefix(&self) -> Option<String> {
        // Fuzzy matches don't start with what's typed, so they can't share a prefix with it.
        let mut prefixed = self
            .candidates
            .iter()
            .filter(|c| strip_prefix_ignore_case(&c.replacement, &self.typed).is_some());
        let mut prefix: Vec<char> = prefixed.next()?.replacement.chars().collect();
        for candidate in prefixed {
            let len = prefix
                .iter()
                .zip(candidate.replacement.chars())
                .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
                .count();
            prefix.truncate(len);
        }
        let prefix: String = prefix.into_iter().collect();
        let typed_len = self.typed.chars().count();
        let extends = strip_prefix_ignore_case(&prefix, &self.typed).is_some();
        (extends && prefix.chars().count() > typed_len).then_some(prefix)
    }

    /// The text and cursor after accepting candidate `index`.
    pub fn accept(&self, text: &str, index: usize) -> (String, usize) {
        let candidate = &self.candidates[index];
        let chars: Vec<char> = text.chars().collect();
        let before: String = chars[..self.start].iter().collect();
        let after: String = chars[self.end..].iter().collect();
        let space = candidate.space_after && !after.starts_with(' ');
        let mut cursor = self.start + candidate.replacement.chars().count();
        if candidate.space_after {
            cursor += 1;
        }
        let text = format!(
            "{before}{}{}{after}",
            candidate.replacement,
            if space { " " } else { "" }
        );
        (text, cursor)
    }
}

/// Complete the filter word at `cursor` (in chars). `prs` supplies values like
/// repository names and the match counts.
pub fn filter(
    text: &str,
    cursor: usize,
    prs: &[Pr],
    viewer: &str,
    now: Timestamp,
    local: Option<&LocalState>,
) -> Option<Completion> {
    let chars: Vec<char> = text.chars().collect();
    let (start, word) = word_at(&chars, cursor)?;
    let negated = word.starts_with('-');
    let body_start = start + usize::from(negated);
    let body: String = chars[body_start..cursor].iter().collect();

    let counter = Counter {
        chars: &chars,
        cursor,
        prs,
        ctx: Context { viewer, now, local },
    };

    let Some((key, value_part)) = body.split_once(':') else {
        // A key or flag.
        let mut candidates: Vec<Candidate> = KEYS
            .iter()
            .map(|k| Candidate {
                replacement: format!("{}:", k.name),
                description: k.description.to_owned(),
                count: None,
                space_after: false,
            })
            .chain(FLAGS.iter().map(|(flag, description)| Candidate {
                replacement: (*flag).to_owned(),
                description: (*description).to_owned(),
                count: Some(counter.count(body_start, flag)),
                space_after: true,
            }))
            .collect();
        rank(&mut candidates, &body, false);
        return Some(Completion {
            start: body_start,
            end: cursor,
            typed: body,
            is_value: false,
            negated,
            candidates,
        });
    };

    let key = key.to_ascii_lowercase();
    let info = KEYS.iter().find(|k| k.name == key)?;
    // Complete the last comma-separated value; earlier ones are excluded.
    let (earlier, typed) = match value_part.rsplit_once(',') {
        Some((earlier, typed)) => (earlier.split(',').collect::<Vec<_>>(), typed),
        None => (Vec::new(), value_part),
    };
    let seg_start = cursor - typed.chars().count();
    let typed_bare = typed.trim_start_matches('"');

    let options = value_options(info.values, &key, prs);
    let mut candidates: Vec<Candidate> = options
        .into_iter()
        .filter(|(value, _)| {
            !earlier
                .iter()
                .any(|e| e.trim_matches('"').eq_ignore_ascii_case(value))
        })
        .map(|(value, description)| {
            let replacement = if value.contains(' ') {
                format!("\"{value}\"")
            } else {
                value
            };
            Candidate {
                count: Some(counter.count(seg_start, &replacement)),
                replacement,
                description,
                space_after: true,
            }
        })
        .collect();
    rank(&mut candidates, typed_bare, true);
    Some(Completion {
        start: seg_start,
        end: cursor,
        typed: typed.to_owned(),
        is_value: true,
        negated,
        candidates,
    })
}

/// Sort keys: complete the comma-separated key at the cursor.
pub fn sort(text: &str, cursor: usize) -> Option<Completion> {
    let chars: Vec<char> = text.chars().collect();
    if cursor < chars.len() && !matches!(chars[cursor], ',' | ' ') {
        return None;
    }
    let before: String = chars[..cursor].iter().collect();
    let segment = before.rsplit(',').next().unwrap_or("").trim_start();
    let negated = segment.starts_with('-');
    let typed = segment.trim_start_matches('-').to_owned();
    let start = cursor - typed.chars().count();

    let used: Vec<String> = before
        .split(',')
        .rev()
        .skip(1)
        .map(|k| k.trim().trim_start_matches('-').to_ascii_lowercase())
        .collect();
    let mut candidates: Vec<Candidate> = crate::sort::KEYS
        .iter()
        .filter(|(name, _, _)| !used.iter().any(|u| u == name))
        .map(|(name, _, direction)| Candidate {
            replacement: (*name).to_owned(),
            description: if negated {
                format!("reversed: not {direction}")
            } else {
                (*direction).to_owned()
            },
            count: None,
            space_after: false,
        })
        .collect();
    rank(&mut candidates, &typed, false);
    Some(Completion {
        start,
        end: cursor,
        typed,
        // Sort keys are always what's being completed; suggest one even after a comma.
        is_value: true,
        negated,
        candidates,
    })
}

/// The word ending at `cursor` and where it starts. Only completes at the end
/// of a word; spaces inside quotes don't end it.
fn word_at(chars: &[char], cursor: usize) -> Option<(usize, String)> {
    if cursor > chars.len() || (cursor < chars.len() && !chars[cursor].is_whitespace()) {
        return None;
    }
    let mut start = 0;
    let mut in_quotes = false;
    for (i, c) in chars[..cursor].iter().enumerate() {
        match c {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => start = i + 1,
            _ => {}
        }
    }
    Some((start, chars[start..cursor].iter().collect()))
}

/// Values to suggest for a key, with descriptions. Values seen in `prs` are
/// ordered by how often they occur.
fn value_options(kind: ValueKind, key: &str, prs: &[Pr]) -> Vec<(String, String)> {
    let fixed = |options: &[(&str, &str)]| {
        options
            .iter()
            .map(|(v, d)| ((*v).to_owned(), (*d).to_owned()))
            .collect()
    };
    let seen = |values: &mut dyn Iterator<Item = String>| -> Vec<(String, String)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for value in values {
            // Commas separate alternatives, so such values can't be expressed.
            if !value.contains(',') {
                *counts.entry(value).or_default() += 1;
            }
        }
        let mut values: Vec<(String, usize)> = counts.into_iter().collect();
        values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        values
            .into_iter()
            .map(|(v, _)| (v, String::new()))
            .collect()
    };
    let with_me = |mut values: Vec<(String, String)>| {
        values.insert(0, ("@me".to_owned(), "you".to_owned()));
        values
    };

    match kind {
        ValueKind::Enum => enum_values(key).map_or_else(Vec::new, |v| fixed(&v)),
        ValueKind::Repo => seen(&mut prs.iter().map(|pr| pr.repo.to_string())),
        ValueKind::Org => seen(&mut prs.iter().map(|pr| pr.repo.owner.clone())),
        ValueKind::Author => with_me(seen(&mut prs.iter().filter_map(|pr| pr.author.clone()))),
        ValueKind::Reviewer => with_me(seen(&mut prs.iter().flat_map(|pr| {
            pr.requested_reviewers.iter().filter_map(|r| match r {
                Reviewer::User(login) => Some(login.clone()),
                Reviewer::Team(_) => None,
            })
        }))),
        ValueKind::Team => seen(&mut prs.iter().flat_map(|pr| {
            pr.requested_reviewers.iter().filter_map(|r| match r {
                Reviewer::Team(team) => Some(team.clone()),
                Reviewer::User(_) => None,
            })
        })),
        ValueKind::Label => seen(&mut prs.iter().flat_map(|pr| pr.labels.iter().cloned())),
        ValueKind::Base => seen(&mut prs.iter().map(|pr| pr.base_ref.clone())),
        ValueKind::Head => seen(&mut prs.iter().map(|pr| pr.head_ref.clone())),
        ValueKind::Time => fixed(&[
            (">1d", "more than a day ago"),
            (">3d", "more than 3 days ago"),
            (">7d", "more than a week ago"),
            (">30d", "more than a month ago"),
            ("<1d", "within the last day"),
            ("<1h", "within the last hour"),
        ]),
        ValueKind::Number => fixed(&[
            (">0", "at least one"),
            ("0", "none"),
            (">10", "more than 10"),
        ]),
        ValueKind::Size => fixed(&[
            ("xs", "under 10 lines"),
            ("s", "10-99 lines"),
            ("m", "100-399 lines"),
            ("l", "400-999 lines"),
            ("xl", "1000+ lines"),
            ("<100", "under 100 lines"),
        ]),
    }
}

/// Keep candidates matching `typed` (prefix first, then fuzzy), preserving the
/// given order within each group. With `by_count`, candidates matching no PRs go last.
fn rank(candidates: &mut Vec<Candidate>, typed: &str, by_count: bool) {
    let typed = typed.to_lowercase();
    let score = |c: &Candidate| -> Option<u8> {
        let text = c.replacement.trim_start_matches('"').to_lowercase();
        if text.starts_with(&typed) {
            Some(0)
        } else if is_subsequence(&typed, &text) {
            Some(1)
        } else {
            None
        }
    };
    candidates.retain(|c| score(c).is_some());
    // Stable: ties keep their original order.
    candidates.sort_by_key(|c| {
        let empty = by_count && c.count == Some(0);
        (empty, score(c))
    });
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars();
    needle.chars().all(|n| hay.any(|h| h == n))
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let prefix = prefix.trim_start_matches('"');
    let text_bare = text.trim_start_matches('"');
    let offset = text.len() - text_bare.len();
    let mut end = offset;
    let mut rest = text_bare.chars();
    for p in prefix.chars() {
        let t = rest.next()?;
        if !t.eq_ignore_ascii_case(&p) {
            return None;
        }
        end += t.len_utf8();
    }
    Some(&text[end..])
}

/// Counts matches for the filter with a candidate put in place of the word.
struct Counter<'a> {
    chars: &'a [char],
    cursor: usize,
    prs: &'a [Pr],
    ctx: Context<'a>,
}

impl Counter<'_> {
    fn count(&self, start: usize, replacement: &str) -> usize {
        let before: String = self.chars[..start].iter().collect();
        let after: String = self.chars[self.cursor..].iter().collect();
        let text = format!("{before}{replacement}{after}");
        match Filter::parse(&text) {
            Ok(filter) => self
                .prs
                .iter()
                .filter(|pr| filter.shows(pr, &self.ctx))
                .count(),
            Err(_) => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Turn;
    use crate::test_support::{now, pr};

    fn prs() -> Vec<Pr> {
        let mut a = pr(1);
        a.labels = vec!["bug".into(), "needs review".into()];
        let mut b = pr(2);
        b.repo = "acme/web".parse().unwrap();
        b.labels = vec!["bug".into()];
        b.turn = Turn::Author;
        b.is_draft = true;
        let mut c = pr(3);
        c.repo = "acme/web".parse().unwrap();
        c.labels = vec![];
        vec![a, b, c]
    }

    fn complete(text: &str) -> Completion {
        filter(text, text.chars().count(), &prs(), "me", now(), None).unwrap()
    }

    fn names(c: &Completion) -> Vec<&str> {
        c.candidates
            .iter()
            .map(|c| c.replacement.as_str())
            .collect()
    }

    #[test]
    fn completes_keys_with_ghost_text() {
        let c = complete("tu");
        assert_eq!(names(&c)[0], "turn:");
        assert_eq!(c.ghost(), Some("rn:"));
        assert!(c.is_incomplete());
        assert_eq!((c.start, c.end), (0, 2));
    }

    #[test]
    fn empty_word_lists_all_keys_and_flags() {
        let c = complete("");
        assert_eq!(c.candidates.len(), KEYS.len() + FLAGS.len());
        assert_eq!(c.ghost(), None, "no suggestion after every space");
        assert!(!c.is_incomplete(), "so `turn:me ` still applies");
    }

    #[test]
    fn negation_is_kept() {
        let c = complete("repo:x -dr");
        assert_eq!(c.start, 8);
        assert_eq!(names(&c), ["draft"]);
        assert_eq!(c.candidates[0].count, Some(0), "no PR matches repo:x");
        let (text, cursor) = c.accept("repo:x -dr", 0);
        assert_eq!((text.as_str(), cursor), ("repo:x -draft ", 14));
    }

    #[test]
    fn a_lone_dash_is_incomplete() {
        let c = complete("repo:x -");
        assert!(c.is_incomplete());
        assert_eq!(c.ghost(), None);
        assert!(!complete("repo:x ").is_incomplete());
    }

    #[test]
    fn exact_flag_is_complete() {
        let c = complete("draft");
        assert!(!c.is_incomplete());
        assert_eq!(c.ghost(), None);
    }

    #[test]
    fn enum_values_with_counts() {
        let c = complete("turn:");
        assert_eq!(names(&c)[0], "me", "matching PRs first");
        assert_eq!(c.candidates[0].count, Some(2));
        assert_eq!(c.candidates[0].description, "you need to act");
        assert!(c.is_incomplete());
        let c = complete("turn:au");
        assert_eq!(names(&c), ["author"]);
        assert_eq!(c.ghost(), Some("thor"));
    }

    #[test]
    fn values_from_the_data_by_frequency() {
        let c = complete("repo:");
        assert_eq!(names(&c), ["acme/web", "acme/api"]);
        let c = complete("label:");
        assert_eq!(names(&c), ["bug", "\"needs review\""]);
        let c = complete("author:");
        assert_eq!(
            names(&c),
            ["alice", "@me"],
            "@me matches nothing here, so it's last"
        );
    }

    #[test]
    fn comma_alternatives_exclude_earlier_values() {
        let c = complete("ci:pass,");
        assert!(!names(&c).contains(&"pass"));
        assert_eq!(c.start, 8);
    }

    #[test]
    fn counts_include_the_rest_of_the_filter() {
        let c = complete("repo:acme/web lab");
        assert_eq!(names(&c)[0], "label:");
        let c = complete("repo:acme/web label:");
        let bug = c
            .candidates
            .iter()
            .find(|c| c.replacement == "bug")
            .unwrap();
        assert_eq!(bug.count, Some(1));
        let review = &c.candidates.last().unwrap();
        assert_eq!(review.count, Some(0), "empty matches go last");
    }

    #[test]
    fn fuzzy_after_prefix() {
        let c = complete("wtg");
        assert_eq!(names(&c), ["waiting:"]);
        assert_eq!(c.ghost(), None, "can't show a fuzzy match as a suffix");
    }

    #[test]
    fn quoted_values() {
        let c = complete("label:\"nee");
        assert_eq!(names(&c), ["\"needs review\""]);
        assert_eq!(c.ghost(), Some("ds review\""));
    }

    #[test]
    fn only_at_the_end_of_a_word() {
        assert!(filter("turn:me", 2, &prs(), "me", now(), None).is_none());
        assert!(filter("tu rest", 2, &prs(), "me", now(), None).is_some());
    }

    #[test]
    fn unknown_keys_have_no_completion() {
        assert!(filter("colour:", 7, &prs(), "me", now(), None).is_none());
    }

    #[test]
    fn common_prefix_of_candidates() {
        let c = complete("re");
        assert_eq!(names(&c)[..3], ["repo:", "reviewer:", "review:"]);
        assert_eq!(c.common_prefix(), None, "'re' is already the shared start");
        let c = complete("revi");
        assert_eq!(c.common_prefix().as_deref(), Some("review"));
    }

    #[test]
    fn sort_keys() {
        let c = sort("turn,wa", 7).unwrap();
        assert_eq!(names(&c), ["waiting"]);
        assert_eq!(c.start, 5);
        let c = sort("turn,-", 6).unwrap();
        assert!(!names(&c).contains(&"turn"), "already used");
        assert!(c.candidates[0].description.starts_with("reversed"));
        let (text, cursor) = sort("sc", 2).unwrap().accept("sc", 0);
        assert_eq!((text.as_str(), cursor), ("score", 5));
    }
}
