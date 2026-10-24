use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

const LIMIT: usize = 4096;

#[derive(Clone, Debug, Default)]
pub(super) struct Editor {
    pub text: String,
    pub cursor: usize,
    start: usize,
}

impl Editor {
    pub fn new(text: String) -> Self {
        let mut value = Self::default();
        value.insert(&text);
        value
    }

    pub fn insert(&mut self, text: &str) -> bool {
        let mut value = String::new();
        for c in text.chars().filter(|c| !c.is_control()) {
            if self.text.len() + value.len() + c.len_utf8() > LIMIT {
                break;
            }
            value.push(c);
        }
        if value.is_empty() {
            return false;
        }
        self.text.insert_str(self.cursor, &value);
        self.cursor += value.len();
        true
    }

    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.cursor
            + self.text[self.cursor..]
                .chars()
                .next()
                .map_or(0, char::len_utf8)
    }

    pub fn key(&mut self, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Left => self.cursor = self.previous(),
            KeyCode::Right => self.cursor = self.next(),
            KeyCode::Home | KeyCode::Char('a') if key.code == KeyCode::Home || control => {
                self.cursor = 0
            }
            KeyCode::End | KeyCode::Char('e') if key.code == KeyCode::End || control => {
                self.cursor = self.text.len()
            }
            KeyCode::Backspace if control || alt => {
                let end = self.cursor;
                while self.cursor > 0
                    && self.text[..self.cursor]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace)
                {
                    self.cursor = self.previous();
                }
                while self.cursor > 0
                    && self.text[..self.cursor]
                        .chars()
                        .next_back()
                        .is_some_and(|c| !c.is_whitespace())
                {
                    self.cursor = self.previous();
                }
                self.text.drain(self.cursor..end);
                return end != self.cursor;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let previous = self.previous();
                self.text.drain(previous..self.cursor);
                self.cursor = previous;
                return true;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.drain(self.cursor..self.next());
                return true;
            }
            KeyCode::Char('u') if control => {
                let changed = !self.text.is_empty();
                self.text.clear();
                self.cursor = 0;
                self.start = 0;
                return changed;
            }
            KeyCode::Char('k') if control => {
                let changed = self.cursor < self.text.len();
                self.text.truncate(self.cursor);
                return changed;
            }
            KeyCode::Char(c)
                if !control && !alt && !key.modifiers.contains(KeyModifiers::SUPER) =>
            {
                return self.insert(&c.to_string());
            }
            _ => {}
        }
        false
    }

    pub fn visible(&mut self, width: u16) -> (String, u16) {
        self.start = self.start.min(self.text.len()).min(self.cursor);
        while !self.text.is_char_boundary(self.start) {
            self.start -= 1;
        }
        while self.start < self.cursor
            && self.text[self.start..self.cursor].width() >= width.max(1) as usize
        {
            self.start += self.text[self.start..]
                .chars()
                .next()
                .map_or(1, char::len_utf8);
        }
        let mut end = self.start;
        for (i, c) in self.text[self.start..].char_indices() {
            let next = self.start + i + c.len_utf8();
            if self.text[self.start..next].width() > width as usize {
                break;
            }
            end = next;
        }
        (
            self.text[self.start..end].to_string(),
            self.text[self.start..self.cursor]
                .width()
                .min(width.saturating_sub(1) as usize) as u16,
        )
    }

    pub fn click(&mut self, column: u16) {
        self.cursor = self.start;
        for (i, c) in self.text[self.start..].char_indices() {
            let next = self.start + i + c.len_utf8();
            if self.text[self.start..next].width() > column as usize {
                break;
            }
            self.cursor = next;
        }
    }
}
