//! A small multi-line text editor for the prompt box.

use unicode_width::UnicodeWidthChar;

#[derive(Debug, Default, Clone)]
pub struct Input {
    text: String,
    /// Byte offset of the cursor; always on a char boundary.
    cursor: usize,
}

impl Input {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn insert(&mut self, s: &str) {
        let s = s
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ");
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_boundary(self.cursor) {
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if let Some(next) = self.next_boundary(self.cursor) {
            self.text.replace_range(self.cursor..next, "");
        }
    }

    pub fn left(&mut self) {
        if let Some(prev) = self.prev_boundary(self.cursor) {
            self.cursor = prev;
        }
    }

    pub fn right(&mut self) {
        if let Some(next) = self.next_boundary(self.cursor) {
            self.cursor = next;
        }
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end(self.cursor);
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start_before(self.cursor);
    }

    pub fn word_right(&mut self) {
        let rest = &self.text[self.cursor..];
        let skip_space = rest.len() - rest.trim_start().len();
        let word = rest[skip_space..]
            .find(char::is_whitespace)
            .unwrap_or(rest.len() - skip_space);
        self.cursor += skip_space + word;
    }

    pub fn delete_word_back(&mut self) {
        let start = self.word_start_before(self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Ctrl+U: delete to the start of the line.
    pub fn kill_line_back(&mut self) {
        let start = self.line_start(self.cursor);
        let start = if start == self.cursor {
            self.prev_boundary(start).unwrap_or(start)
        } else {
            start
        };
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Ctrl+K: delete to the end of the line.
    pub fn kill_line_forward(&mut self) {
        let end = self.line_end(self.cursor);
        let end = if end == self.cursor {
            self.next_boundary(end).unwrap_or(end)
        } else {
            end
        };
        self.text.replace_range(self.cursor..end, "");
    }

    /// Moves up one line. Returns false when already on the first line.
    pub fn up(&mut self) -> bool {
        let start = self.line_start(self.cursor);
        if start == 0 {
            return false;
        }
        let col = self.text[start..self.cursor].chars().count();
        let prev_start = self.line_start(start - 1);
        self.cursor = self.offset_at_col(prev_start, col);
        true
    }

    /// Moves down one line. Returns false when already on the last line.
    pub fn down(&mut self) -> bool {
        let end = self.line_end(self.cursor);
        if end == self.text.len() {
            return false;
        }
        let col = self.text[self.line_start(self.cursor)..self.cursor]
            .chars()
            .count();
        self.cursor = self.offset_at_col(end + 1, col);
        true
    }

    /// Soft-wraps the text to `width` columns, returning the visual lines and
    /// the cursor's (row, column).
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(2);
        let mut lines = vec![String::new()];
        let mut col = 0;
        let mut cursor = (0, 0);
        for (i, ch) in self.text.char_indices() {
            if i == self.cursor {
                cursor = (lines.len() - 1, col);
            }
            if ch == '\n' {
                lines.push(String::new());
                col = 0;
                continue;
            }
            let w = ch.width().unwrap_or(0);
            if col + w > width {
                lines.push(String::new());
                col = 0;
            }
            lines.last_mut().expect("never empty").push(ch);
            col += w;
        }
        if self.cursor == self.text.len() {
            if col >= width {
                lines.push(String::new());
                col = 0;
            }
            cursor = (lines.len() - 1, col);
        }
        (lines, cursor)
    }

    fn prev_boundary(&self, at: usize) -> Option<usize> {
        self.text[..at].char_indices().next_back().map(|(i, _)| i)
    }

    fn next_boundary(&self, at: usize) -> Option<usize> {
        self.text[at..].chars().next().map(|c| at + c.len_utf8())
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |i| at + i)
    }

    fn offset_at_col(&self, line_start: usize, col: usize) -> usize {
        let end = self.line_end(line_start);
        self.text[line_start..end]
            .char_indices()
            .nth(col)
            .map_or(end, |(i, _)| line_start + i)
    }

    fn word_start_before(&self, at: usize) -> usize {
        let before = &self.text[..at];
        let trimmed = before.trim_end();
        trimmed.rfind(char::is_whitespace).map_or(0, |i| {
            i + trimmed[i..].chars().next().map_or(1, char::len_utf8)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_and_moves_by_word() {
        let mut input = Input::default();
        input.insert("hello wörld");
        input.word_left();
        input.insert("big ");
        assert_eq!(input.text(), "hello big wörld");
        input.end();
        input.delete_word_back();
        assert_eq!(input.text(), "hello big ");
        input.backspace();
        input.home();
        input.word_right();
        input.kill_line_forward();
        assert_eq!(input.text(), "hello");
        input.kill_line_back();
        assert!(input.is_empty());
    }

    #[test]
    fn moves_between_lines_and_reports_edges() {
        let mut input = Input::default();
        input.insert("first line\nsecond");
        assert!(!input.down());
        assert!(input.up());
        input.insert("X");
        assert_eq!(input.text(), "first Xline\nsecond");
        assert!(!input.up());
        assert!(input.down());
    }

    #[test]
    fn layout_wraps_and_tracks_cursor() {
        let mut input = Input::default();
        input.insert("abcdef\ngh");
        let (lines, cursor) = input.layout(4);
        assert_eq!(lines, ["abcd", "ef", "gh"]);
        assert_eq!(cursor, (2, 2));
        input.set("abcd");
        assert_eq!(
            input.layout(4),
            (vec!["abcd".to_string(), String::new()], (1, 0))
        );
        input.insert("\r\nx\ty");
        assert_eq!(input.text(), "abcd\nx    y");
    }
}
