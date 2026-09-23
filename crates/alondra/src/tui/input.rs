use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::md::char_width;

#[derive(Default, Clone, Debug)]
pub struct TextInput {
    chars: Vec<char>,
    cursor: usize,
}

impl TextInput {
    pub fn with(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        TextInput { cursor: chars.len(), chars }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.replace("\r\n", "\n").replace('\r', "\n").chars() {
            self.insert(if c == '\t' { ' ' } else { c });
        }
    }

    fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    fn delete_word(&mut self) {
        let mut i = self.cursor;
        while i > 0 && self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.chars.drain(i..self.cursor);
        self.cursor = i;
    }

    fn line_start(&self) -> usize {
        self.chars[..self.cursor].iter().rposition(|&c| c == '\n').map_or(0, |p| p + 1)
    }

    fn line_end(&self) -> usize {
        self.chars[self.cursor..].iter().position(|&c| c == '\n').map_or(self.chars.len(), |p| self.cursor + p)
    }

    pub fn handle(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Enter if alt || k.modifiers.contains(KeyModifiers::SHIFT) => self.insert('\n'),
            KeyCode::Char('j') if ctrl => self.insert('\n'),
            KeyCode::Char('u') if ctrl => {
                let start = self.line_start();
                self.chars.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Char('a') if ctrl => self.cursor = self.line_start(),
            KeyCode::Char('e') if ctrl => self.cursor = self.line_end(),
            KeyCode::Char(c) if !ctrl => self.insert(c),
            KeyCode::Backspace if alt => self.delete_word(),
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.chars.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return false,
        }
        true
    }

    /// Lines hard-wrapped at `width`, and the cursor's (row, column) in them.
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(2);
        let mut lines = vec![String::new()];
        let mut col = 0;
        let mut cursor = (0, 0);
        for (i, &c) in self.chars.iter().enumerate() {
            if i == self.cursor {
                cursor = (lines.len() - 1, col);
            }
            if c == '\n' {
                lines.push(String::new());
                col = 0;
                continue;
            }
            let w = char_width(c);
            if col + w > width {
                lines.push(String::new());
                col = 0;
            }
            if let Some(l) = lines.last_mut() {
                l.push(c);
            }
            col += w;
        }
        if self.cursor == self.chars.len() {
            if col >= width {
                lines.push(String::new());
                col = 0;
            }
            cursor = (lines.len() - 1, col);
        }
        (lines, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn edits_in_the_middle_and_deletes_words() {
        let mut t = TextInput::with("hello world");
        t.handle(key(KeyCode::Left, KeyModifiers::NONE));
        t.handle(key(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(t.text(), "hello worl!d");
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        t.handle(key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(t.text(), "hello ");
        t.handle(key(KeyCode::Enter, KeyModifiers::ALT));
        t.insert_str("a\r\nb");
        assert_eq!(t.text(), "hello \na\nb");
        t.handle(key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(t.text(), "hello \na\n");
    }

    #[test]
    fn layout_wraps_and_tracks_the_cursor() {
        let t = TextInput::with("abcdef\ngh");
        let (lines, cur) = t.layout(4);
        assert_eq!(lines, vec!["abcd", "ef", "gh"]);
        assert_eq!(cur, (2, 2));
        let full = TextInput::with("abcd");
        assert_eq!(full.layout(4).1, (1, 0));
    }
}
