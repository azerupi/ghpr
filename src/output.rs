//! Non-interactive output for `ghpr list`.

use color_eyre::eyre::{Result, bail};
use jiff::Timestamp;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::columns::{self, Column};
use crate::model::{Pr, ScoreWeights, score};

/// Spaces between columns; a little more air than the interactive table.
const SPACING: usize = 2;

/// An aligned table of `columns`. When `width` is given, low-priority columns
/// are dropped and titles truncated to fit it.
pub fn table(
    prs: &[Pr],
    columns: &[Column],
    weights: &ScoreWeights,
    now: Timestamp,
    width: Option<usize>,
) -> String {
    let rows: Vec<Vec<String>> = prs
        .iter()
        .map(|pr| columns.iter().map(|c| c.text(pr, weights, now)).collect())
        .collect();
    let mut widths: Vec<usize> = columns.iter().map(|c| c.header().width()).collect();
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.width());
        }
    }

    let keep = match width {
        Some(total) => {
            let keep = columns::fit(columns, &widths, total, SPACING);
            // The title gets whatever is left.
            let others: usize = columns
                .iter()
                .zip(&widths)
                .zip(&keep)
                .filter(|((c, _), k)| **k && **c != Column::Title)
                .map(|((_, w), _)| w + SPACING)
                .sum();
            if let Some(t) = columns.iter().position(|c| *c == Column::Title) {
                widths[t] =
                    widths[t].min(total.saturating_sub(others).max(columns::MIN_TITLE_WIDTH));
            }
            keep
        }
        None => vec![true; columns.len()],
    };

    let mut out = String::new();
    let mut push_row = |cells: Vec<&str>| {
        let line: Vec<String> = cells
            .into_iter()
            .zip(columns)
            .zip(&widths)
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(((cell, column), w), _)| {
                let cell = truncate(cell, *w);
                if column.right_aligned() {
                    format!("{cell:>w$}", w = *w)
                } else {
                    pad(&cell, *w)
                }
            })
            .collect();
        out.push_str(line.join(&" ".repeat(SPACING)).trim_end());
        out.push('\n');
    };
    push_row(columns.iter().map(|c| c.header()).collect());
    for row in &rows {
        push_row(row.iter().map(String::as_str).collect());
    }
    out
}

pub fn json(prs: &[Pr]) -> Result<String> {
    Ok(serde_json::to_string_pretty(prs)? + "\n")
}

/// One line per PR, substituting `{field}` placeholders. `{{` and `}}` are literal braces.
pub fn format(
    prs: &[Pr],
    template: &str,
    weights: &ScoreWeights,
    now: Timestamp,
) -> Result<String> {
    let segments = parse_template(template)?;
    let mut out = String::new();
    for pr in prs {
        for segment in &segments {
            match segment {
                Segment::Literal(text) => out.push_str(text),
                Segment::Field(field) => out.push_str(&field_value(pr, *field, weights, now)),
            }
        }
        out.push('\n');
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
enum Field {
    Repo,
    Number,
    Title,
    Author,
    Url,
    Draft,
    Ci,
    Review,
    Additions,
    Deletions,
    Files,
    Base,
    Head,
    Labels,
    Created,
    Updated,
    Turn,
    Merge,
    Waiting,
    Last,
    Mine,
    Threads,
    Mentioned,
    Score,
}

const FIELDS: &[(&str, Field)] = &[
    ("repo", Field::Repo),
    ("number", Field::Number),
    ("title", Field::Title),
    ("author", Field::Author),
    ("url", Field::Url),
    ("draft", Field::Draft),
    ("ci", Field::Ci),
    ("review", Field::Review),
    ("additions", Field::Additions),
    ("deletions", Field::Deletions),
    ("files", Field::Files),
    ("base", Field::Base),
    ("head", Field::Head),
    ("labels", Field::Labels),
    ("created", Field::Created),
    ("updated", Field::Updated),
    ("turn", Field::Turn),
    ("merge", Field::Merge),
    ("waiting", Field::Waiting),
    ("last", Field::Last),
    ("mine", Field::Mine),
    ("threads", Field::Threads),
    ("mentioned", Field::Mentioned),
    ("score", Field::Score),
];

enum Segment {
    Literal(String),
    Field(Field),
}

fn parse_template(template: &str) -> Result<Vec<Segment>> {
    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => bail!("unclosed '{{' in template"),
                    }
                }
                let Some(&(_, field)) = FIELDS.iter().find(|(n, _)| *n == name) else {
                    let names: Vec<_> = FIELDS.iter().map(|(n, _)| *n).collect();
                    bail!(
                        "unknown field {{{name}}} in template (available: {})",
                        names.join(", ")
                    );
                };
                if !literal.is_empty() {
                    segments.push(Segment::Literal(std::mem::take(&mut literal)));
                }
                segments.push(Segment::Field(field));
            }
            '}' => bail!("unmatched '}}' in template (use '}}}}' for a literal brace)"),
            c => literal.push(c),
        }
    }
    if !literal.is_empty() {
        segments.push(Segment::Literal(literal));
    }
    Ok(segments)
}

fn field_value(pr: &Pr, field: Field, weights: &ScoreWeights, now: Timestamp) -> String {
    match field {
        Field::Repo => pr.repo.to_string(),
        Field::Number => pr.number.to_string(),
        Field::Title => pr.title.clone(),
        Field::Author => author(pr),
        Field::Url => pr.url.clone(),
        Field::Draft => pr.is_draft.to_string(),
        Field::Ci => pr.ci.label().to_owned(),
        Field::Review => pr.review_decision.map_or("-", |r| r.label()).to_owned(),
        Field::Additions => pr.additions.to_string(),
        Field::Deletions => pr.deletions.to_string(),
        Field::Files => pr.changed_files.to_string(),
        Field::Base => pr.base_ref.clone(),
        Field::Head => pr.head_ref.clone(),
        Field::Labels => pr.labels.join(","),
        Field::Created => age(now, pr.created_at),
        Field::Updated => age(now, pr.updated_at),
        Field::Turn => pr.turn.label().to_owned(),
        Field::Merge => pr.merge_status(),
        Field::Waiting => age(now, pr.waiting_since),
        Field::Last => last_role(pr).to_owned(),
        Field::Mine => pr.my_review.map_or("-", |r| r.state.label()).to_owned(),
        Field::Threads => pr.unresolved_threads.to_string(),
        Field::Mentioned => pr.mentions_me.to_string(),
        Field::Score => score(pr, weights, now).to_string(),
    }
}

fn author(pr: &Pr) -> String {
    pr.author.clone().unwrap_or_else(|| "ghost".to_owned())
}

fn last_role(pr: &Pr) -> &'static str {
    pr.last_activity.as_ref().map_or("-", |a| a.role.label())
}

/// A compact "how long ago": `5m`, `3h`, `4d`, `6w`, `2y`.
pub fn age(now: Timestamp, then: Timestamp) -> String {
    span(now.as_second() - then.as_second())
}

/// A rough length of time: `12m`, `5h`, `3d`, `2w`, `1y`.
pub fn span(secs: i64) -> String {
    let secs = secs.max(0);
    let (minute, hour, day) = (60, 3600, 86_400);
    match secs {
        s if s < hour => format!("{}m", s / minute),
        s if s < day => format!("{}h", s / hour),
        s if s < 14 * day => format!("{}d", s / day),
        s if s < 365 * day => format!("{}w", s / (7 * day)),
        s => format!("{}y", s / (365 * day)),
    }
}

fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

fn pad(s: &str, width: usize) -> String {
    let fill = width.saturating_sub(s.width());
    format!("{s}{}", " ".repeat(fill))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CiState, Turn};
    use crate::test_support::{now, pr_titled as pr};

    #[test]
    fn age_picks_a_sensible_unit() {
        let ago = |secs: i64| {
            age(
                now(),
                Timestamp::from_second(now().as_second() - secs).unwrap(),
            )
        };
        assert_eq!(ago(30), "0m");
        assert_eq!(ago(90 * 60), "1h");
        assert_eq!(ago(3 * 86_400), "3d");
        assert_eq!(ago(20 * 86_400), "2w");
        assert_eq!(ago(800 * 86_400), "2y");
        assert_eq!(ago(-100), "0m", "future timestamps clamp to zero");
    }

    #[test]
    fn table_aligns_columns() {
        let mut draft = pr(7, "WIP thing");
        draft.is_draft = true;
        draft.author = None;
        draft.review_decision = None;
        draft.ci = CiState::None;
        draft.turn = Turn::Blocked;
        draft.last_activity = None;
        let prs = [pr(1234, "Fix token refresh race"), draft];
        let out = table(
            &prs,
            columns::DEFAULT,
            &ScoreWeights::default(),
            now(),
            None,
        );
        let expected = "\
TURN     REPO         #  TITLE                   AUTHOR  SIZE    CI    REVIEW    WAIT  LAST
me       acme/api  1234  Fix token refresh race  alice   +42 -8  pass  approved  2d    author
blocked  acme/api     7  [draft] WIP thing       ghost   +42 -8  -     -         2d    -
";
        assert_eq!(out, expected);
    }

    #[test]
    fn table_truncates_titles_to_fit_width() {
        let title = "A very long title that will certainly not fit in a narrow terminal";
        let prs = [pr(1, title)];
        let out = table(
            &prs,
            columns::DEFAULT,
            &ScoreWeights::default(),
            now(),
            Some(100),
        );
        assert!(out.lines().all(|l| l.width() <= 100), "{out}");
        assert!(out.contains('…'), "{out}");
        assert!(out.contains("AUTHOR"), "wide enough to keep every column");

        let narrow = table(
            &prs,
            columns::DEFAULT,
            &ScoreWeights::default(),
            now(),
            Some(60),
        );
        assert!(narrow.lines().all(|l| l.width() <= 60), "{narrow}");
        assert!(
            !narrow.contains("LAST"),
            "low-priority columns go first: {narrow}"
        );
    }

    #[test]
    fn table_shows_configured_columns() {
        let columns: Vec<Column> = ["number", "score", "title"]
            .iter()
            .map(|c| c.parse().unwrap())
            .collect();
        let out = table(
            &[pr(12, "Title")],
            &columns,
            &ScoreWeights::default(),
            now(),
            None,
        );
        assert_eq!(out, " #  SCORE  TITLE\n12    125  Title\n");
    }

    #[test]
    fn truncate_respects_wide_characters() {
        assert_eq!(truncate("日本語のタイトル", 7), "日本語…");
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn format_substitutes_fields_and_escapes() {
        let out = format(
            &[pr(12, "Title")],
            "{repo}#{number} {{{labels}}} {updated} {turn}/{last} {score}",
            &ScoreWeights::default(),
            now(),
        )
        .unwrap();
        assert_eq!(out, "acme/api#12 {bug,auth} 1d me/author 125\n");
    }

    #[test]
    fn format_rejects_unknown_fields_and_stray_braces() {
        assert!(
            format(&[], "{nope}", &ScoreWeights::default(), now())
                .unwrap_err()
                .to_string()
                .contains("available")
        );
        assert!(format(&[], "oops}", &ScoreWeights::default(), now()).is_err());
        assert!(
            format(&[], "{repo", &ScoreWeights::default(), now())
                .unwrap_err()
                .to_string()
                .contains("unclosed")
        );
    }
}
