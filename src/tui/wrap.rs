//! Word-wrapping styled lines, with hanging indents.

use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| width(&s.content)).sum()
}

/// Wraps `spans` to `max` columns. The first output line starts with `first`,
/// the rest with `rest`. Words longer than a line are broken.
pub fn wrap(
    spans: Vec<Span<'static>>,
    max: usize,
    first: &[Span<'static>],
    rest: &[Span<'static>],
) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut line: Vec<Span<'static>> = first.to_vec();
    let mut used = spans_width(first);
    let mut indent = used;
    let mut at_start = true;

    let new_line = |out: &mut Vec<Line<'static>>, line: &mut Vec<Span<'static>>| {
        out.push(Line::from(std::mem::take(line)));
        line.extend(rest.iter().cloned());
    };

    for span in spans {
        let style = span.style;
        for token in tokens(&span.content) {
            let is_space = token.chars().all(char::is_whitespace);
            let w = width(token);
            if is_space {
                if at_start && !out.is_empty() {
                    continue; // no leading whitespace on wrapped lines
                }
                if used + w <= max {
                    line.push(Span::styled(token.to_string(), style));
                    used += w;
                }
                continue;
            }
            if used + w > max && !at_start {
                trim_trailing_space(&mut line);
                new_line(&mut out, &mut line);
                indent = spans_width(rest);
                used = indent;
            }
            if used + w <= max {
                line.push(Span::styled(token.to_string(), style));
                used += w;
            } else {
                // Break an over-long word across lines.
                let mut chunk = String::new();
                for ch in token.chars() {
                    let cw = width(ch.encode_utf8(&mut [0; 4]));
                    if used + cw > max && used > indent {
                        line.push(Span::styled(std::mem::take(&mut chunk), style));
                        new_line(&mut out, &mut line);
                        indent = spans_width(rest);
                        used = indent;
                    }
                    chunk.push(ch);
                    used += cw;
                }
                if !chunk.is_empty() {
                    line.push(Span::styled(chunk, style));
                }
            }
            at_start = false;
        }
    }
    out.push(Line::from(line));
    out
}

fn trim_trailing_space(line: &mut Vec<Span<'static>>) {
    while line
        .last()
        .is_some_and(|s| s.content.chars().all(char::is_whitespace) && !s.content.is_empty())
    {
        line.pop();
    }
}

/// Splits text into alternating runs of whitespace and non-whitespace.
fn tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = 0;
    let mut prev: Option<bool> = None;
    for (i, ch) in text.char_indices() {
        let space = ch.is_whitespace();
        if prev.is_some_and(|p| p != space) {
            tokens.push(&text[start..i]);
            start = i;
        }
        prev = Some(space);
    }
    if start < text.len() {
        tokens.push(&text[start..]);
    }
    tokens
}

/// Truncates a plain string to `max` columns, adding an ellipsis.
pub fn truncate(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let cw = width(ch.encode_utf8(&mut [0; 4]));
        if used + cw + 1 > max {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

/// Pads a line with `style`d spaces to exactly `max` columns (for full-width
/// backgrounds).
pub fn pad(mut line: Line<'static>, max: usize, style: ratatui::style::Style) -> Line<'static> {
    let used = spans_width(&line.spans);
    if used < max {
        line.spans.push(Span::styled(" ".repeat(max - used), style));
    }
    line.style = style;
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn wraps_words_with_hanging_indent() {
        let lines = wrap(
            vec![Span::raw("the quick brown fox jumps")],
            12,
            &[Span::raw("• ")],
            &[Span::raw("  ")],
        );
        assert_eq!(plain(&lines), ["• the quick", "  brown fox", "  jumps"]);
        for l in &lines {
            assert!(width(&plain(std::slice::from_ref(l))[0]) <= 12);
        }
    }

    #[test]
    fn breaks_long_words_and_wide_chars() {
        let lines = wrap(vec![Span::raw("abcdefghij")], 4, &[], &[]);
        assert_eq!(plain(&lines), ["abcd", "efgh", "ij"]);
        let lines = wrap(vec![Span::raw("日本語テキスト")], 6, &[], &[]);
        assert!(lines.iter().all(|l| l.width() <= 6));
    }

    #[test]
    fn truncates_with_ellipsis() {
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("hi", 6), "hi");
    }
}
