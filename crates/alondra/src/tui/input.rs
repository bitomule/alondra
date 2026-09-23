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
            KeyCode::Char(c) if !ctrl => {
                let shifted = k.modifiers.contains(KeyModifiers::SHIFT) && c.is_lowercase();
                if shifted {
                    c.to_uppercase().for_each(|u| self.insert(u));
                } else {
                    self.insert(c);
                }
            }
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

    /// Lines word-wrapped at `width` (a word longer than a line is split), and the cursor's
    /// (row, column) in them.
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(2);
        let mut rows: Vec<Vec<(usize, char)>> = vec![vec![]];
        let row_width = |row: &[(usize, char)]| row.iter().map(|&(_, c)| char_width(c)).sum::<usize>();
        for (i, &c) in self.chars.iter().enumerate() {
            if c == '\n' {
                rows.push(vec![]);
                continue;
            }
            let row = rows.last_mut().expect("at least one row");
            if c == ' ' || row_width(row) + char_width(c) <= width {
                row.push((i, c));
                continue;
            }
            let carried = match row.iter().rposition(|&(_, ch)| ch == ' ') {
                Some(space) if space + 1 < row.len() || space + 1 == row.len() => row.split_off(space + 1),
                _ => vec![],
            };
            rows.push(carried);
            rows.last_mut().expect("just pushed").push((i, c));
        }
        let mut cursor = None;
        for (r, row) in rows.iter().enumerate() {
            let mut col = 0;
            for &(i, c) in row {
                if i == self.cursor {
                    cursor = Some((r, col.min(width - 1)));
                }
                col += char_width(c);
            }
        }
        let cursor = cursor.unwrap_or_else(|| {
            let before_cursor_newline = self.chars[..self.cursor].iter().filter(|&&c| c == '\n').count();
            let r = if self.cursor == self.chars.len() { rows.len() - 1 } else { before_cursor_newline.min(rows.len() - 1) };
            let col = row_width(&rows[r]);
            if col >= width { (r + 1, 0) } else { (r, col) }
        });
        let mut lines: Vec<String> = rows.into_iter().map(|row| row.into_iter().map(|(_, c)| c).collect()).collect();
        if cursor.0 >= lines.len() {
            lines.push(String::new());
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
    fn shift_with_a_lowercase_letter_types_a_capital() {
        let mut t = TextInput::default();
        t.handle(key(KeyCode::Char('h'), KeyModifiers::SHIFT));
        t.handle(key(KeyCode::Char('O'), KeyModifiers::SHIFT));
        t.handle(key(KeyCode::Char('l'), KeyModifiers::NONE));
        t.handle(key(KeyCode::Char('á'), KeyModifiers::SHIFT));
        assert_eq!(t.text(), "HOlÁ");
    }

    #[test]
    fn layout_wraps_at_words_not_in_the_middle_of_them() {
        let mut t = TextInput::default();
        "the quick brown fox".chars().for_each(|c| t.insert(c));
        let (lines, cursor) = t.layout(10);
        assert_eq!(lines, vec!["the quick ", "brown fox"]);
        assert_eq!(cursor, (1, 9));
        let mut long = TextInput::default();
        "abcdefghij klm".chars().for_each(|c| long.insert(c));
        assert_eq!(long.layout(4).0, vec!["abcd", "efgh", "ij ", "klm"]);
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
