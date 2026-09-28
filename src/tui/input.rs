//! A single-line text editor for the filter and sort prompts.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineInput {
    text: String,
    /// Cursor position, in characters.
    cursor: usize,
}

impl LineInput {
    pub fn new(text: &str) -> Self {
        LineInput {
            text: text.to_owned(),
            cursor: text.chars().count(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replace the text, putting the cursor at `cursor` (clamped to the end).
    pub fn set(&mut self, text: &str, cursor: usize) {
        self.text = text.to_owned();
        self.cursor = cursor.min(self.len());
    }

    /// Apply an editing key. Returns whether the text changed.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let before = self.text.clone();
        match key.code {
            KeyCode::Char('u') if ctrl => {
                self.remove(0, self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let chars: Vec<char> = self.text.chars().collect();
                let mut start = self.cursor;
                while start > 0 && chars[start - 1] == ' ' {
                    start -= 1;
                }
                while start > 0 && chars[start - 1] != ' ' {
                    start -= 1;
                }
                self.remove(start, self.cursor);
                self.cursor = start;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.len(),
            KeyCode::Char(c) if !ctrl => {
                let at = self.byte(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.remove(self.cursor - 1, self.cursor);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < self.len() => {
                self.remove(self.cursor, self.cursor + 1)
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.len(),
            _ => {}
        }
        self.text != before
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    fn byte(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn remove(&mut self, from: usize, to: usize) {
        let (from, to) = (self.byte(from), self.byte(to));
        self.text.replace_range(from..to, "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(input: &mut LineInput, code: KeyCode) -> bool {
        input.handle(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(input: &mut LineInput, c: char) -> bool {
        input.handle(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    #[test]
    fn typing_and_moving() {
        let mut input = LineInput::new("ab");
        assert_eq!(input.cursor(), 2);
        assert!(press(&mut input, KeyCode::Char('c')));
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Char('é'));
        assert_eq!(input.text(), "aébc");
        assert_eq!(input.cursor(), 2);
        assert!(
            !press(&mut input, KeyCode::Left),
            "moving doesn't change the text"
        );
        press(&mut input, KeyCode::Backspace);
        assert_eq!((input.text(), input.cursor()), ("ébc", 0));
        press(&mut input, KeyCode::Delete);
        assert_eq!(input.text(), "bc");
        press(&mut input, KeyCode::End);
        press(&mut input, KeyCode::Right);
        assert_eq!(input.cursor(), 2);
        assert!(
            !press(&mut input, KeyCode::Delete),
            "nothing to delete at the end"
        );
    }

    #[test]
    fn word_and_line_deletion() {
        let mut input = LineInput::new("repo:a/b  turn:me");
        ctrl(&mut input, 'w');
        assert_eq!(input.text(), "repo:a/b  ");
        ctrl(&mut input, 'w');
        assert_eq!(input.text(), "");

        let mut input = LineInput::new("one two");
        press(&mut input, KeyCode::Left);
        ctrl(&mut input, 'u');
        assert_eq!((input.text(), input.cursor()), ("o", 0));
    }
}
