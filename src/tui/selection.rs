//! Selecting transcript text with the mouse.
//!
//! Positions are (line, column) in the transcript's rendered lines, so a
//! selection stays put while the view scrolls or new output streams in.

use ratatui::text::Line;
use unicode_width::UnicodeWidthChar;

/// A (line, column) position in the rendered transcript.
pub type Pos = (usize, usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// The cell where the drag started.
    pub anchor: Pos,
    /// The cell under the pointer now; both ends are included.
    pub head: Pos,
}

impl Selection {
    pub fn at(pos: Pos) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    /// Start and end in reading order; `end` is exclusive.
    pub fn ordered(&self) -> (Pos, Pos) {
        let (start, (line, col)) = if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        (start, (line, col.saturating_add(1)))
    }

    /// The selected column range `[from, to)` on `line`, if any.
    pub fn columns(&self, line: usize, line_width: usize) -> Option<(usize, usize)> {
        let ((l0, c0), (l1, c1)) = self.ordered();
        if line < l0 || line > l1 {
            return None;
        }
        let from = if line == l0 { c0 } else { 0 };
        let to = if line == l1 { c1 } else { line_width };
        let to = to.min(line_width);
        (from < to).then_some((from, to))
    }

    /// The selected text, one row per line, with trailing padding removed.
    pub fn text(&self, lines: &[Line<'_>]) -> String {
        let ((l0, _), (l1, _)) = self.ordered();
        let mut rows = Vec::new();
        for (i, line) in lines.iter().enumerate().take(l1 + 1).skip(l0) {
            let plain = plain(line);
            let row = match self.columns(i, usize::MAX) {
                Some((from, to)) => slice_columns(&plain, from, to),
                None => String::new(),
            };
            rows.push(row.trim_end().to_string());
        }
        rows.join("\n")
    }
}

pub fn plain(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// The characters of `text` that start within columns `[from, to)`.
fn slice_columns(text: &str, from: usize, to: usize) -> String {
    let mut col = 0;
    let mut out = String::new();
    for ch in text.chars() {
        if col >= to {
            break;
        }
        if col >= from {
            out.push(ch);
        }
        col += ch.width().unwrap_or(0);
    }
    out
}

/// The column range `[from, to)` of the word under `col`, or of the single
/// cell when there is no word there.
pub fn word_at(text: &str, col: usize) -> (usize, usize) {
    let mut cells = Vec::new();
    let mut at = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if w > 0 {
            cells.push((at, w, is_word(ch)));
        }
        at += w;
    }
    let Some(hit) = cells.iter().position(|(c, w, _)| col >= *c && col < c + w) else {
        return (col, col + 1);
    };
    if !cells[hit].2 {
        return (cells[hit].0, cells[hit].0 + cells[hit].1);
    }
    let mut first = hit;
    while first > 0
        && cells[first - 1].2
        && cells[first - 1].0 + cells[first - 1].1 == cells[first].0
    {
        first -= 1;
    }
    let mut last = hit;
    while last + 1 < cells.len()
        && cells[last + 1].2
        && cells[last].0 + cells[last].1 == cells[last + 1].0
    {
        last += 1;
    }
    (cells[first].0, cells[last].0 + cells[last].1)
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || "_-./:~@#%+=".contains(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<Line<'static>> {
        text.iter().map(|t| Line::raw(t.to_string())).collect()
    }

    #[test]
    fn extracts_text_across_lines_in_either_direction() {
        let lines = lines(&["● hello world", "  second line   ", "third"]);
        let forward = Selection {
            anchor: (0, 8),
            head: (2, 2),
        };
        assert_eq!(forward.text(&lines), "world\n  second line\nthi");
        let backward = Selection {
            anchor: (2, 2),
            head: (0, 8),
        };
        assert_eq!(backward.text(&lines), forward.text(&lines));
    }

    #[test]
    fn handles_wide_characters_by_column() {
        let lines = lines(&["日本語 text"]);
        let sel = Selection {
            anchor: (0, 2),
            head: (0, 5),
        };
        assert_eq!(sel.text(&lines), "本語");
    }

    #[test]
    fn columns_clip_to_the_line() {
        let sel = Selection {
            anchor: (1, 4),
            head: (3, 1),
        };
        assert_eq!(sel.columns(0, 10), None);
        assert_eq!(sel.columns(1, 10), Some((4, 10)));
        assert_eq!(sel.columns(2, 7), Some((0, 7)));
        assert_eq!(sel.columns(3, 10), Some((0, 2)));
        assert_eq!(sel.columns(1, 3), None);
    }

    #[test]
    fn finds_words_and_paths() {
        let text = "  run cargo test in src/tui/app.rs now";
        assert_eq!(word_at(text, 7), (6, 11));
        let (from, to) = word_at(text, 22);
        assert_eq!(&text[from..to], "src/tui/app.rs");
        assert_eq!(word_at(text, 0), (0, 1));
        assert_eq!(word_at(text, 99), (99, 100));
    }
}
