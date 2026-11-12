//! A small multi-line text input with a movable cursor.

use unicode_width::UnicodeWidthChar;

#[derive(Default)]
pub struct Input {
    chars: Vec<char>,
    cursor: usize,
}

impl Input {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_blank(&self) -> bool {
        self.chars.iter().all(|c| c.is_whitespace())
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars().filter(|c| *c != '\r') {
            self.insert(if c == '\t' { ' ' } else { c });
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    pub fn home(&mut self) {
        while self.cursor > 0 && self.chars[self.cursor - 1] != '\n' {
            self.cursor -= 1;
        }
    }

    pub fn end(&mut self) {
        while self.cursor < self.chars.len() && self.chars[self.cursor] != '\n' {
            self.cursor += 1;
        }
    }

    /// Ctrl-W: deletes the word before the cursor, and the spaces after it.
    pub fn delete_word(&mut self) {
        let end = self.cursor;
        while self.cursor > 0 && self.chars[self.cursor - 1] == ' ' {
            self.cursor -= 1;
        }
        while self.cursor > 0 && !self.chars[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
        if self.cursor == end {
            // Nothing but a line break before the cursor: join the lines.
            self.backspace();
            return;
        }
        self.chars.drain(self.cursor..end);
    }

    /// Ctrl-U: deletes from the start of the line to the cursor.
    pub fn delete_to_line_start(&mut self) {
        let end = self.cursor;
        self.home();
        self.chars.drain(self.cursor..end);
    }

    /// Wraps the text to `width` columns, breaking after a space where it
    /// can. Returns the rows and the cursor's (column, row) within them.
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(1);
        // The (column, row) of each character, and of the end of the text.
        let mut pos = Vec::with_capacity(self.chars.len() + 1);
        let mut rows: Vec<Vec<char>> = vec![Vec::new()];
        let mut col = 0;
        for &c in &self.chars {
            let w = c.width().unwrap_or(0);
            if c != '\n' && col + w > width {
                let row = rows.last_mut().unwrap();
                // Move the word in progress down, unless it fills the row alone.
                let tail = match row.iter().rposition(|&ch| ch == ' ') {
                    Some(space) if space + 1 < row.len() => row.split_off(space + 1),
                    _ => Vec::new(),
                };
                let moved = pos.len() - tail.len();
                rows.push(Vec::new());
                col = 0;
                for (k, &ch) in tail.iter().enumerate() {
                    pos[moved + k] = (col, rows.len() - 1);
                    col += ch.width().unwrap_or(0);
                }
                rows.last_mut().unwrap().extend(tail);
            }
            pos.push((col, rows.len() - 1));
            if c == '\n' {
                rows.push(Vec::new());
                col = 0;
            } else {
                rows.last_mut().unwrap().push(c);
                col += w;
            }
        }
        // A full last row puts the cursor at the start of a new one.
        if col >= width {
            rows.push(Vec::new());
            col = 0;
        }
        pos.push((col, rows.len() - 1));
        let rows = rows.into_iter().map(|r| r.into_iter().collect()).collect();
        (rows, pos[self.cursor])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_at_the_cursor() {
        let mut input = Input::default();
        input.insert_str("helo");
        input.left();
        input.insert('l');
        assert_eq!(input.text(), "hello");
        input.home();
        input.delete();
        input.end();
        input.backspace();
        assert_eq!(input.text(), "ell");
        assert!(!input.is_blank());
        input.clear();
        assert!(input.is_blank());
    }

    #[test]
    fn wraps_and_tracks_the_cursor() {
        let mut input = Input::default();
        input.insert_str("abcdef\ngh");
        let (rows, cursor) = input.layout(4);
        assert_eq!(rows, ["abcd", "ef", "gh"]);
        assert_eq!(cursor, (2, 2));
        input.home();
        assert_eq!(input.layout(4).1, (0, 2));
        input.left();
        assert_eq!(input.layout(4).1, (2, 1));
    }

    #[test]
    fn breaks_lines_between_words() {
        let mut input = Input::default();
        input.insert_str("the cat sat");
        let (rows, cursor) = input.layout(8);
        assert_eq!(rows, ["the cat ", "sat"]);
        assert_eq!(cursor, (3, 1));
        for _ in 0..5 {
            input.left();
        }
        // On the "t" of "cat", which stayed on the first row.
        assert_eq!(input.layout(8).1, (6, 0));
        // A word longer than the row is cut where it overflows.
        input.clear();
        input.insert_str("a abcdefghij");
        assert_eq!(input.layout(5).0, ["a ", "abcde", "fghij", ""]);
        assert_eq!(input.layout(5).1, (0, 3));
    }

    #[test]
    fn deletes_words_and_lines_as_vim_insert_mode_does() {
        let mut input = Input::default();
        input.insert_str("first line\nsome more  words  ");
        input.delete_word();
        assert_eq!(input.text(), "first line\nsome more  ");
        input.delete_word();
        assert_eq!(input.text(), "first line\nsome ");
        input.delete_to_line_start();
        assert_eq!(input.text(), "first line\n");
        // At the start of a line, Ctrl-W joins it to the line before.
        input.delete_word();
        assert_eq!(input.text(), "first line");
    }
}
