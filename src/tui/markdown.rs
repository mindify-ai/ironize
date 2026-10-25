//! Renders Markdown to styled, wrapped terminal lines.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme;
use super::wrap::wrap;

/// Renders `source` to lines at most `width` columns wide.
pub fn render(source: &str, width: usize) -> Vec<Line<'static>> {
    let mut r = Renderer {
        width: width.max(8),
        ..Default::default()
    };
    let parser = Parser::new_ext(
        source,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS,
    );
    for event in parser {
        r.event(event);
    }
    r.flush();
    while r.out.last().is_some_and(|l| l.spans.is_empty()) {
        r.out.pop();
    }
    r.out
}

#[derive(Default)]
struct Renderer {
    width: usize,
    out: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    styles: Vec<Style>,
    /// One entry per open list: the next number for ordered lists.
    lists: Vec<Option<u64>>,
    /// Marker for the first line of the current list item.
    marker: Option<String>,
    quote: usize,
    code: Option<String>,
    links: Vec<String>,
    table_header: bool,
}

impl Renderer {
    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_else(theme::text)
    }

    fn push_style(&mut self, f: impl FnOnce(Style) -> Style) {
        let next = f(self.style());
        self.styles.push(next);
    }

    fn blank(&mut self) {
        if self.out.last().is_some_and(|l| !l.spans.is_empty()) {
            self.out.push(Line::default());
        }
    }

    fn prefixes(&mut self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let mut first = Vec::new();
        let mut rest = Vec::new();
        for _ in 0..self.quote {
            first.push(Span::styled("│ ", theme::faint()));
            rest.push(Span::styled("│ ", theme::faint()));
        }
        let depth = self.lists.len();
        if depth > 0 {
            let pad = "  ".repeat(depth - 1);
            match self.marker.take() {
                Some(marker) => {
                    let w = super::wrap::width(&marker);
                    first.push(Span::raw(pad.clone()));
                    first.push(Span::styled(marker, theme::accent()));
                    rest.push(Span::raw(format!("{pad}{}", " ".repeat(w))));
                }
                None => {
                    let indent = format!("{pad}  ");
                    first.push(Span::raw(indent.clone()));
                    rest.push(Span::raw(indent));
                }
            }
        }
        (first, rest)
    }

    fn flush(&mut self) {
        if self.spans.is_empty() && self.marker.is_none() {
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        let (first, rest) = self.prefixes();
        self.out.extend(wrap(spans, self.width, &first, &rest));
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if self.code.is_some() {
                    self.code_text(&text);
                } else {
                    let style = self.style();
                    self.spans.push(Span::styled(text.into_string(), style));
                }
            }
            Event::Code(code) => self.spans.push(Span::styled(
                code.into_string(),
                Style::default().fg(theme::CODE),
            )),
            Event::InlineHtml(html) | Event::Html(html) => self
                .spans
                .push(Span::styled(html.into_string(), theme::dim())),
            Event::SoftBreak => self.spans.push(Span::raw(" ")),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.blank();
                self.out
                    .push(Line::styled("─".repeat(self.width.min(48)), theme::faint()));
                self.out.push(Line::default());
            }
            Event::TaskListMarker(done) => {
                let (mark, style) = if done {
                    ("☑ ", Style::default().fg(theme::SUCCESS))
                } else {
                    ("☐ ", theme::dim())
                };
                self.spans.push(Span::styled(mark, style));
            }
            Event::FootnoteReference(name) => self
                .spans
                .push(Span::styled(format!("[{name}]"), theme::dim())),
            Event::InlineMath(m) | Event::DisplayMath(m) => self.spans.push(Span::styled(
                m.into_string(),
                Style::default().fg(theme::CODE),
            )),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.flush();
                self.blank();
                let color = if level == HeadingLevel::H1 {
                    theme::ACCENT_STRONG
                } else {
                    theme::ACCENT
                };
                let mut style = theme::bold(color);
                if level == HeadingLevel::H1 {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                self.styles.push(style);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote += 1;
                self.push_style(|s| s.add_modifier(Modifier::ITALIC).fg(theme::DIM));
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.blank();
                let lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang
                        .split(',')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                if !lang.is_empty() {
                    self.out.push(Line::from(vec![
                        Span::styled("  ╭ ", theme::faint()),
                        Span::styled(lang.clone(), theme::dim()),
                    ]));
                }
                self.code = Some(lang);
            }
            Tag::List(start) => {
                self.flush();
                if self.lists.is_empty() {
                    self.blank();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                self.marker = Some(marker);
            }
            Tag::Emphasis => self.push_style(|s| s.add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(|s| s.add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.push_style(|s| s.add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                self.links.push(dest_url.into_string());
                self.push_style(|s| s.fg(theme::CODE).add_modifier(Modifier::UNDERLINED));
            }
            Tag::Image { dest_url, .. } => {
                self.spans
                    .push(Span::styled(format!("[image: {dest_url}]"), theme::dim()));
            }
            Tag::Table(_) => {
                self.flush();
                self.blank();
            }
            Tag::TableHead => {
                self.table_header = true;
                self.push_style(|s| s.add_modifier(Modifier::BOLD));
            }
            Tag::TableRow | Tag::TableCell => {}
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                if self.lists.is_empty() {
                    self.out.push(Line::default());
                }
            }
            TagEnd::Heading(_) => {
                self.styles.pop();
                self.flush();
                self.out.push(Line::default());
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.styles.pop();
                self.quote = self.quote.saturating_sub(1);
                self.blank();
            }
            TagEnd::CodeBlock => {
                self.code = None;
                self.out.push(Line::default());
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.out.push(Line::default());
                }
            }
            TagEnd::Item => self.flush(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                if let Some(url) = self.links.pop() {
                    let text: String = self
                        .spans
                        .iter()
                        .rev()
                        .take(1)
                        .map(|s| s.content.to_string())
                        .collect();
                    if !url.is_empty() && text != url && !url.starts_with('#') {
                        self.spans
                            .push(Span::styled(format!(" ({url})"), theme::faint()));
                    }
                }
            }
            TagEnd::TableHead => {
                self.styles.pop();
                self.table_row_end();
                self.table_header = false;
            }
            TagEnd::TableRow => self.table_row_end(),
            TagEnd::TableCell => self.spans.push(Span::styled(" │ ", theme::faint())),
            TagEnd::Table => self.out.push(Line::default()),
            _ => {}
        }
    }

    fn table_row_end(&mut self) {
        if self.spans.last().is_some_and(|s| s.content == " │ ") {
            self.spans.pop();
        }
        let header = self.table_header;
        self.flush();
        if header {
            self.out
                .push(Line::styled("─".repeat(self.width.min(48)), theme::faint()));
        }
    }

    fn code_text(&mut self, text: &str) {
        let gutter = Span::styled("  │ ", theme::faint());
        let style = Style::default().fg(theme::CODE);
        for line in text.lines() {
            let content = Span::styled(line.replace('\t', "    "), style);
            self.out.extend(wrap(
                vec![content],
                self.width,
                std::slice::from_ref(&gutter),
                std::slice::from_ref(&gutter),
            ));
        }
    }
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
    fn renders_common_blocks() {
        let md = "# Title\n\nSome **bold** and `code`.\n\n- one\n- two\n  1. nested\n\n```rust\nfn main() {}\n```\n\n> quoted";
        let lines = plain(&render(md, 60));
        assert_eq!(lines[0], "Title");
        assert!(lines.contains(&"Some bold and code.".to_string()));
        assert!(lines.contains(&"• one".to_string()));
        assert!(lines.contains(&"• two".to_string()));
        assert!(lines.contains(&"  1. nested".to_string()));
        assert!(lines.contains(&"  ╭ rust".to_string()));
        assert!(lines.contains(&"  │ fn main() {}".to_string()));
        assert!(lines.contains(&"│ quoted".to_string()));
        assert!(!lines.last().unwrap().is_empty());
    }

    #[test]
    fn wraps_list_items_with_hanging_indent() {
        let lines = plain(&render("- alpha beta gamma delta", 14));
        assert_eq!(lines, ["• alpha beta", "  gamma delta"]);
    }

    #[test]
    fn tolerates_unfinished_streaming_markdown() {
        let lines = plain(&render("Here:\n\n```py\nprint(1", 40));
        assert!(lines.iter().any(|l| l.contains("print(1")));
    }
}
