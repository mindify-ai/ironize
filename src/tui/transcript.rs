//! The conversation as displayed: entries rendered to wrapped lines, cached
//! per entry and width.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::markdown;
use super::theme::{self, BULLET};
use super::wrap::{pad, truncate, width, wrap};
use crate::tools::{DiffKind, DiffLine, Todo, TodoStatus, ToolDisplay};

const COLLAPSED_OUTPUT_LINES: usize = 5;
const COLLAPSED_DIFF_LINES: usize = 14;
const EXPANDED_LINES: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Succeeded,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Welcome {
    pub user: String,
    pub model: String,
    pub workspace: String,
    pub cwd: String,
}

#[derive(Debug, Clone)]
pub enum Entry {
    Welcome(Welcome),
    User(String),
    Assistant(String),
    Tool {
        id: String,
        label: &'static str,
        subject: String,
        status: ToolStatus,
        brief: String,
        display: ToolDisplay,
    },
    Notice(Level, String),
}

#[derive(Default)]
pub struct Transcript {
    entries: Vec<Entry>,
    cache: Vec<Option<(u16, bool, Vec<Line<'static>>)>>,
}

impl Transcript {
    pub fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
        self.cache.push(None);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.cache.clear();
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn last_mut(&mut self) -> Option<&mut Entry> {
        let last = self.entries.len().checked_sub(1)?;
        self.cache[last] = None;
        self.entries.last_mut()
    }

    pub fn tool_mut(&mut self, tool_id: &str) -> Option<&mut Entry> {
        let index = self
            .entries
            .iter()
            .rposition(|e| matches!(e, Entry::Tool { id, .. } if id == tool_id))?;
        self.cache[index] = None;
        Some(&mut self.entries[index])
    }

    pub fn invalidate(&mut self) {
        self.cache.iter_mut().for_each(|c| *c = None);
    }

    /// All lines at `width`, with a blank line between entries.
    pub fn lines(&mut self, width: u16, expanded: bool, frame: usize) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for (entry, cache) in self.entries.iter().zip(self.cache.iter_mut()) {
            let animated = matches!(
                entry,
                Entry::Tool {
                    status: ToolStatus::Running,
                    ..
                }
            );
            let lines = match cache {
                Some((w, e, lines)) if *w == width && *e == expanded && !animated => lines.clone(),
                _ => {
                    let lines = render_entry(entry, width as usize, expanded, frame);
                    if !animated {
                        *cache = Some((width, expanded, lines.clone()));
                    }
                    lines
                }
            };
            out.extend(lines);
            out.push(Line::default());
        }
        out
    }
}

fn render_entry(entry: &Entry, width: usize, expanded: bool, frame: usize) -> Vec<Line<'static>> {
    let width = width.max(10);
    match entry {
        Entry::Welcome(w) => welcome(w, width),
        Entry::User(text) => {
            let bg = Style::default().bg(theme::USER_BG);
            let mut lines = Vec::new();
            for (i, line) in text.lines().enumerate() {
                let first = if i == 0 {
                    Span::styled("› ", theme::bold(theme::ACCENT))
                } else {
                    Span::raw("  ")
                };
                let wrapped = wrap(
                    vec![Span::styled(line.to_string(), theme::text())],
                    width,
                    &[first],
                    &[Span::raw("  ")],
                );
                lines.extend(wrapped.into_iter().map(|l| pad(l, width, bg)));
            }
            lines
        }
        Entry::Assistant(text) => {
            let body = markdown::render(text, width.saturating_sub(2));
            body.into_iter()
                .enumerate()
                .map(|(i, mut line)| {
                    let prefix = if i == 0 {
                        Span::styled(BULLET, theme::accent())
                    } else {
                        Span::raw("  ")
                    };
                    line.spans.insert(0, prefix);
                    line
                })
                .collect()
        }
        Entry::Tool {
            label,
            subject,
            status,
            brief,
            display,
            ..
        } => tool_card(
            label, subject, *status, brief, display, width, expanded, frame,
        ),
        Entry::Notice(level, text) => {
            let (mark, style) = match level {
                Level::Info => ("  ", theme::dim()),
                Level::Warn => ("! ", Style::default().fg(theme::WARN)),
                Level::Error => ("✗ ", Style::default().fg(theme::ERROR)),
            };
            let mut lines = Vec::new();
            for (i, line) in text.lines().enumerate() {
                let first = if i == 0 {
                    Span::styled(format!("  {mark}"), style)
                } else {
                    Span::raw("    ")
                };
                lines.extend(wrap(
                    vec![Span::styled(line.to_string(), style)],
                    width,
                    &[first],
                    &[Span::raw("    ")],
                ));
            }
            lines
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn tool_card(
    label: &str,
    subject: &str,
    status: ToolStatus,
    brief: &str,
    display: &ToolDisplay,
    width: usize,
    expanded: bool,
    frame: usize,
) -> Vec<Line<'static>> {
    let (bullet, bullet_style) = match status {
        ToolStatus::Running => (
            format!("{} ", theme::SPINNER[frame % theme::SPINNER.len()]),
            Style::default().fg(theme::WARN),
        ),
        ToolStatus::Succeeded => (BULLET.to_string(), Style::default().fg(theme::SUCCESS)),
        ToolStatus::Failed => (BULLET.to_string(), Style::default().fg(theme::ERROR)),
        ToolStatus::Declined => (BULLET.to_string(), theme::dim()),
    };
    let subject_line = subject.lines().next().unwrap_or_default();
    let room = width.saturating_sub(width_of(label) + 4);
    let mut header = vec![
        Span::styled(bullet, bullet_style),
        Span::styled(label.to_string(), theme::bold(theme::TEXT)),
    ];
    if !subject_line.is_empty() {
        header.push(Span::styled(
            format!("({})", truncate(subject_line, room.saturating_sub(2))),
            theme::dim(),
        ));
    }
    let mut lines = vec![Line::from(header)];

    if status == ToolStatus::Running {
        return lines;
    }
    let brief_style = match status {
        ToolStatus::Failed => Style::default().fg(theme::ERROR),
        ToolStatus::Declined => theme::dim().add_modifier(Modifier::ITALIC),
        _ => theme::dim(),
    };
    if !brief.is_empty() {
        lines.extend(wrap(
            vec![Span::styled(brief.to_string(), brief_style)],
            width,
            &[Span::styled("  ⎿ ", theme::faint())],
            &[Span::raw("    ")],
        ));
    }

    match display {
        ToolDisplay::None => {}
        ToolDisplay::Lines(output) => {
            let limit = if expanded {
                EXPANDED_LINES
            } else {
                COLLAPSED_OUTPUT_LINES
            };
            for line in output.iter().take(limit) {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(truncate(line, width.saturating_sub(4)), theme::dim()),
                ]));
            }
            more(&mut lines, output.len(), limit);
        }
        ToolDisplay::Diff(diff) => {
            let limit = if expanded {
                EXPANDED_LINES
            } else {
                COLLAPSED_DIFF_LINES
            };
            lines.extend(diff_lines(diff, width, limit));
            more(&mut lines, diff.len(), limit);
        }
        ToolDisplay::Todos(todos) => lines.extend(todo_lines(todos, width, "    ")),
    }
    lines
}

fn more(lines: &mut Vec<Line<'static>>, total: usize, shown: usize) {
    if total > shown {
        lines.push(Line::styled(
            format!("    … +{} lines (ctrl+o to expand)", total - shown),
            theme::faint(),
        ));
    }
}

fn width_of(s: &str) -> usize {
    width(s)
}

/// Diff lines with colored backgrounds, as used in tool cards and approvals.
pub fn diff_lines(diff: &[DiffLine], width: usize, limit: usize) -> Vec<Line<'static>> {
    diff.iter()
        .take(limit)
        .map(|d| {
            let room = width.saturating_sub(6);
            let (sign, style) = match d.kind {
                DiffKind::Hunk => {
                    return Line::from(vec![
                        Span::raw("    "),
                        Span::styled(truncate(&d.text, room + 2), theme::faint()),
                    ])
                }
                DiffKind::Context => ("  ", theme::dim()),
                DiffKind::Added => (
                    "+ ",
                    Style::default().fg(theme::SUCCESS).bg(theme::ADDED_BG),
                ),
                DiffKind::Removed => (
                    "- ",
                    Style::default().fg(theme::ERROR).bg(theme::REMOVED_BG),
                ),
            };
            let text = truncate(&d.text.replace('\t', "    "), room);
            let line = Line::from(vec![
                Span::raw("    "),
                Span::styled(format!("{sign}{text}"), style),
            ]);
            if d.kind == DiffKind::Context {
                line
            } else {
                let mut padded = pad(
                    Line::from(line.spans[1..].to_vec()),
                    width.saturating_sub(4),
                    style,
                );
                padded.spans.insert(0, Span::raw("    "));
                padded.style = Style::default();
                padded
            }
        })
        .collect()
}

pub fn todo_lines(todos: &[Todo], width: usize, indent: &str) -> Vec<Line<'static>> {
    todos
        .iter()
        .map(|t| {
            let (mark, style) = match t.status {
                TodoStatus::Completed => ("☑ ", theme::dim().add_modifier(Modifier::CROSSED_OUT)),
                TodoStatus::InProgress => ("◐ ", theme::bold(theme::ACCENT)),
                TodoStatus::Pending => ("☐ ", theme::text()),
            };
            let mark_style = if t.status == TodoStatus::Completed {
                Style::default().fg(theme::SUCCESS)
            } else {
                style
            };
            let room = width.saturating_sub(width_of(indent) + 2);
            Line::from(vec![
                Span::raw(indent.to_string()),
                Span::styled(mark, mark_style),
                Span::styled(truncate(&t.content, room), style),
            ])
        })
        .collect()
}

fn welcome(w: &Welcome, width: usize) -> Vec<Line<'static>> {
    let inner = width.min(76).saturating_sub(4).max(20);
    let border = Style::default().fg(theme::ACCENT_STRONG);
    let row = |spans: Vec<Span<'static>>| -> Line<'static> {
        let used: usize = spans.iter().map(|s| width_of(&s.content)).sum();
        let mut line = vec![Span::styled("│ ", border)];
        line.extend(spans);
        line.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        line.push(Span::styled(" │", border));
        Line::from(line)
    };
    let field = |name: &str, value: &str| {
        row(vec![
            Span::styled(format!("  {name:<10}"), theme::dim()),
            Span::styled(truncate(value, inner.saturating_sub(12)), theme::text()),
        ])
    };
    vec![
        Line::styled(format!("╭{}╮", "─".repeat(inner + 2)), border),
        row(vec![
            Span::styled("✻ ", theme::bold(theme::ACCENT)),
            Span::styled("Welcome to ", theme::bold(theme::TEXT)),
            Span::styled("Ironize", theme::bold(theme::ACCENT)),
            Span::styled(format!("  v{}", env!("CARGO_PKG_VERSION")), theme::faint()),
        ]),
        row(vec![Span::styled(
            "  The agent harness by Mindify AI, powered by Cortex",
            theme::dim(),
        )]),
        row(vec![]),
        field("account", &w.user),
        field("model", &w.model),
        field("workspace", &w.workspace),
        field("cwd", &w.cwd),
        row(vec![]),
        row(vec![
            Span::styled("  /help", theme::accent()),
            Span::styled(" commands · ", theme::dim()),
            Span::styled("/model", theme::accent()),
            Span::styled(" switch model · ", theme::dim()),
            Span::styled("shift+tab", theme::accent()),
            Span::styled(" permissions", theme::dim()),
        ]),
        Line::styled(format!("╰{}╯", "─".repeat(inner + 2)), border),
    ]
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
    fn tool_card_collapses_output() {
        let mut t = Transcript::default();
        t.push(Entry::Tool {
            id: "1".into(),
            label: "Bash",
            subject: "cargo test".into(),
            status: ToolStatus::Succeeded,
            brief: "exit code 0 · 1.2s".into(),
            display: ToolDisplay::Lines((1..=8).map(|i| format!("line {i}")).collect()),
        });
        let lines = plain(&t.lines(60, false, 0));
        assert_eq!(lines[0], "● Bash(cargo test)");
        assert_eq!(lines[1], "  ⎿ exit code 0 · 1.2s");
        assert_eq!(lines[2], "    line 1");
        assert!(lines.contains(&"    … +3 lines (ctrl+o to expand)".to_string()));
        let expanded = plain(&t.lines(60, true, 0));
        assert!(expanded.contains(&"    line 8".to_string()));
    }

    #[test]
    fn welcome_box_has_consistent_width() {
        let lines = render_entry(
            &Entry::Welcome(Welcome {
                user: "Ada".into(),
                model: "gpt".into(),
                workspace: "personal".into(),
                cwd: "/tmp".into(),
            }),
            100,
            false,
            0,
        );
        let widths: Vec<usize> = lines.iter().map(|l| l.width()).collect();
        assert!(widths.iter().all(|w| *w == widths[0]), "{widths:?}");
    }

    #[test]
    fn user_messages_fill_the_width() {
        let lines = render_entry(&Entry::User("hi\nthere".into()), 20, false, 0);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.width() == 20));
    }
}
