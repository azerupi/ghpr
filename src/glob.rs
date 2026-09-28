//! Case-insensitive patterns with `*` (any run of characters) and `?` (one character).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob(Vec<char>);

impl Glob {
    pub fn new(pattern: &str) -> Self {
        Glob(pattern.to_lowercase().chars().collect())
    }

    pub fn matches(&self, text: &str) -> bool {
        let text: Vec<char> = text.to_lowercase().chars().collect();
        glob_match(&self.0, &text)
    }

    /// How specific the pattern is: a pattern without wildcards beats any with
    /// them, and otherwise more literal characters win.
    pub fn specificity(&self) -> (bool, usize) {
        let literal = self.0.iter().filter(|c| !matches!(c, '*' | '?')).count();
        (literal == self.0.len(), literal)
    }
}

fn glob_match(pattern: &[char], text: &[char]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some(('*', rest)) => (0..=text.len()).any(|i| glob_match(rest, &text[i..])),
        Some(('?', rest)) => !text.is_empty() && glob_match(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && glob_match(rest, &text[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching() {
        assert!(Glob::new("acme/*").matches("ACME/api"));
        assert!(Glob::new("*/a?i").matches("acme/api"));
        assert!(!Glob::new("acme/web").matches("acme/api"));
        assert!(Glob::new("*").matches(""));
    }

    #[test]
    fn exact_patterns_are_most_specific() {
        let exact = Glob::new("acme/core").specificity();
        let long = Glob::new("acme/platform-*").specificity();
        let short = Glob::new("acme/*").specificity();
        assert!(exact > long && long > short);
    }
}
