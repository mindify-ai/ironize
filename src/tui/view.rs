//! Drawing the interface.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use ratatui::Frame;

use super::app::{Activity, App};
use super::theme;
use super::transcript::{diff_lines, todo_lines};
use super::wrap::{truncate, wrap};
use crate::agent::PermissionMode;
use crate::tools::{Preview, TodoStatus, ToolKind};

const MAX_INPUT_ROWS: usize = 8;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let width = area.width as usize;

    let bottom_height = match &app.permission {
        Some(_) => permission_lines(app, width.saturating_sub(4)).len() as u16 + 2,
        None => {
            let (lines, _) = app.input.layout(width.saturating_sub(6));
            lines.len().clamp(1, MAX_INPUT_ROWS) as u16 + 2
        }
    };
    let show_todos = !app.todos.is_empty()
        && (app.busy.is_some() || app.todos.iter().any(|t| t.status != TodoStatus::Completed));
    let todo_height = if show_todos {
        app.todos.len().min(8) as u16
    } else {
        0
    };

    let [transcript_area, todo_area, status_area, input_area, footer_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(todo_height),
        Constraint::Length(1),
        Constraint::Length(bottom_height),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_transcript(frame, app, transcript_area);
    if show_todos {
        let lines = todo_lines(&app.todos, width, "  ");
        frame.render_widget(Paragraph::new(lines), todo_area);
    }
    draw_status(frame, app, status_area);
    if app.permission.is_some() {
        draw_permission(frame, app, input_area);
    } else {
        draw_input(frame, app, input_area);
        draw_palette(frame, app, input_area);
    }
    draw_footer(frame, app, footer_area);
    if app.picker.is_some() {
        draw_picker(frame, app, area);
    }
}

fn draw_transcript(frame: &mut Frame, app: &mut App, area: Rect) {
    let lines = app.transcript.lines(area.width, app.expanded, app.frame);
    let height = area.height as usize;
    app.viewport = height;
    let max_scroll = lines.len().saturating_sub(height);
    app.scroll = app.scroll.min(max_scroll);
    let start = lines.len().saturating_sub(height + app.scroll);
    let visible: Vec<Line> = lines.into_iter().skip(start).take(height).collect();
    let widths: Vec<usize> = visible.iter().map(Line::width).collect();
    frame.render_widget(Paragraph::new(visible), area);
    app.transcript_area = area;
    app.transcript_top = start;

    if let Some(selection) = app.selection {
        let highlight = Style::default().fg(theme::TEXT).bg(theme::SELECTION_BG);
        for (row, width) in widths.into_iter().enumerate() {
            let width = width.min(area.width as usize);
            if let Some((from, to)) = selection.columns(start + row, width) {
                let cells = Rect::new(
                    area.x + from as u16,
                    area.y + row as u16,
                    (to - from) as u16,
                    1,
                );
                frame.buffer_mut().set_style(cells, highlight);
            }
        }
    }

    if app.scroll > 0 {
        let label = format!(" ↓ {} more · pgdn ", app.scroll);
        let w = label.chars().count() as u16;
        if area.width > w + 1 && area.height > 0 {
            let at = Rect::new(area.right() - w - 1, area.bottom() - 1, w, 1);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    label,
                    Style::default().fg(theme::TEXT).bg(theme::SELECTED_BG),
                )),
                at,
            );
        }
    }
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let Some(busy) = &app.busy else { return };
    let spinner = theme::SPINNER[app.frame % theme::SPINNER.len()];
    let what = match &busy.activity {
        Activity::Thinking => "Thinking…".to_string(),
        Activity::Responding => "Responding…".to_string(),
        Activity::Tool(label) => format!("Running {label}…"),
        Activity::Waiting => "Waiting for your approval".to_string(),
    };
    let secs = busy.since.elapsed().as_secs();
    let elapsed = if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    };
    let line = Line::from(vec![
        Span::styled(format!("{spinner} "), theme::accent()),
        Span::styled(what, theme::bold(theme::ACCENT)),
        Span::styled(format!("  {elapsed} · esc to interrupt"), theme::faint()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let border = if app.busy.is_some() {
        theme::faint()
    } else {
        Style::default().fg(theme::ACCENT_STRONG)
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let text_width = inner.width.saturating_sub(2) as usize;
    let prompt = Span::styled("› ", theme::bold(theme::ACCENT));
    if app.input.is_empty() {
        let hint = if app.busy.is_some() {
            "Type to queue a follow-up…"
        } else {
            "Ask Ironize to build, fix or explain…  (/ for commands)"
        };
        let line = Line::from(vec![
            prompt,
            Span::styled(
                truncate(hint, text_width),
                theme::faint().add_modifier(Modifier::ITALIC),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), inner);
        if app.picker.is_none() {
            frame.set_cursor_position((inner.x + 2, inner.y));
        }
        return;
    }
    let (lines, (row, col)) = app.input.layout(text_width);
    let rows = inner.height as usize;
    let start = (row + 1).saturating_sub(rows);
    let visible: Vec<Line> = lines
        .iter()
        .enumerate()
        .skip(start)
        .take(rows)
        .map(|(i, l)| {
            let lead = if i == 0 {
                prompt.clone()
            } else {
                Span::raw("  ")
            };
            Line::from(vec![lead, Span::styled(l.clone(), theme::text())])
        })
        .collect();
    frame.render_widget(Paragraph::new(visible), inner);
    if app.picker.is_none() {
        frame.set_cursor_position((inner.x + 2 + col as u16, inner.y + (row - start) as u16));
    }
}

fn draw_palette(frame: &mut Frame, app: &App, input: Rect) {
    let items = app.palette();
    if items.is_empty() {
        return;
    }
    let height = items.len() as u16 + 2;
    let width = input.width.min(90);
    if input.y < height {
        return;
    }
    let area = Rect::new(input.x, input.y - height, width, height);
    let selected = app.palette_selected.min(items.len() - 1);
    let args_width = items
        .iter()
        .map(|(_, args, _)| args.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, (name, args, description))| {
            let bg = if i == selected {
                Style::default().bg(theme::SELECTED_BG)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(format!(" {name:<9}"), theme::bold(theme::ACCENT).patch(bg)),
                Span::styled(format!("{args:<args_width$}"), theme::faint().patch(bg)),
                Span::styled(description.to_string(), theme::dim().patch(bg)),
            ])
        })
        .collect();
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::faint());
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn permission_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let Some(permission) = &app.permission else {
        return Vec::new();
    };
    let request = &permission.request;
    let action = match request.kind {
        ToolKind::Edit => "wants to change a file",
        ToolKind::Execute => "wants to run a command",
        ToolKind::Network => "wants to access the network",
        ToolKind::Read => "wants to read",
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(request.label, theme::bold(theme::WARN)),
        Span::styled(format!(" {action}"), theme::text()),
    ])];
    if request.kind == ToolKind::Edit {
        lines.push(Line::styled(
            truncate(&request.subject, width),
            theme::dim(),
        ));
    }
    lines.push(Line::default());
    match &request.preview {
        Preview::Command(cmd) => {
            for (i, line) in cmd.lines().take(8).enumerate() {
                let lead = if i == 0 { "  $ " } else { "    " };
                lines.extend(wrap(
                    vec![Span::styled(
                        line.to_string(),
                        Style::default().fg(theme::CODE),
                    )],
                    width,
                    &[Span::styled(lead, theme::faint())],
                    &[Span::raw("    ")],
                ));
            }
        }
        Preview::Diff(diff) => {
            lines.extend(diff_lines(diff, width, 14));
            if diff.len() > 14 {
                lines.push(Line::styled(
                    format!("    … +{} lines", diff.len() - 14),
                    theme::faint(),
                ));
            }
        }
        Preview::Text(text) => lines.extend(wrap(
            vec![Span::styled(text.clone(), theme::dim())],
            width,
            &[Span::raw("  ")],
            &[Span::raw("  ")],
        )),
        Preview::None => lines.push(Line::styled(
            format!("  {}", truncate(&request.subject, width.saturating_sub(2))),
            theme::dim(),
        )),
    }
    lines.push(Line::default());
    lines.push(Line::styled("Do you want to proceed?", theme::text()));
    let options = [
        "Yes".to_string(),
        format!(
            "Yes, and don't ask again for {} this session",
            request.label
        ),
        "No, and tell Ironize what to do differently (esc)".to_string(),
    ];
    for (i, option) in options.into_iter().enumerate() {
        let chosen = i == permission.selected;
        let style = if chosen {
            theme::bold(theme::ACCENT)
        } else {
            theme::dim()
        };
        lines.push(Line::from(vec![
            Span::styled(if chosen { "› " } else { "  " }, style),
            Span::styled(
                format!("{}. {}", i + 1, truncate(&option, width.saturating_sub(6))),
                style,
            ),
        ]));
    }
    lines
}

fn draw_permission(frame: &mut Frame, app: &App, area: Rect) {
    let lines = permission_lines(app, area.width.saturating_sub(4) as usize);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::WARN))
        .title(Span::styled(
            " Permission required ",
            theme::bold(theme::WARN),
        ))
        .padding(Padding::horizontal(1));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let mode = app.mode.get();
    let (badge, style) = match mode {
        PermissionMode::Default => ("  ? /help for shortcuts".to_string(), theme::faint()),
        PermissionMode::AcceptEdits => (
            "  ⏵⏵ accept edits on".to_string(),
            theme::bold(theme::SUCCESS),
        ),
        PermissionMode::Yolo => (
            "  ⚠ yolo: never asks".to_string(),
            theme::bold(theme::ERROR),
        ),
    };
    let mut left = vec![Span::styled(badge, style)];
    if let Some((toast, _)) = &app.toast {
        left = vec![Span::styled(
            format!("  ✓ {toast}"),
            theme::bold(theme::SUCCESS),
        )];
    } else if mode != PermissionMode::Default {
        left.push(Span::styled(" (shift+tab to cycle)", theme::faint()));
    }
    let right = Line::from(vec![
        Span::styled(app.model.clone(), theme::dim()),
        Span::styled(format!(" · {}", app.workspace), theme::faint()),
        Span::styled(
            format!(
                " · ↑{} ↓{}  ",
                tokens(app.usage.input_tokens),
                tokens(app.usage.output_tokens)
            ),
            theme::faint(),
        ),
    ]);
    frame.render_widget(Paragraph::new(Line::from(left)), area);
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}

fn tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

fn draw_picker(frame: &mut Frame, app: &App, area: Rect) {
    let Some(picker) = &app.picker else { return };
    let width = area.width.saturating_sub(4).min(84);
    let height = area.height.saturating_sub(4).min(22);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::ACCENT_STRONG))
        .title(Span::styled(" Select a model ", theme::bold(theme::ACCENT)))
        .title_bottom(Line::styled(
            " ↑↓ choose · enter select · esc cancel · type to filter ",
            theme::faint(),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Filter ", theme::dim()),
            Span::styled(format!("{}▏", picker.filter), theme::text()),
        ]),
        Line::default(),
    ];
    let w = inner.width as usize;
    match (&picker.models, &picker.error) {
        (_, Some(error)) => lines.push(Line::styled(
            truncate(error, w),
            Style::default().fg(theme::ERROR),
        )),
        (None, None) => lines.push(Line::styled(
            format!(
                "{} Loading models…",
                theme::SPINNER[app.frame % theme::SPINNER.len()]
            ),
            theme::dim(),
        )),
        (Some(_), None) => {
            let visible = picker.visible();
            if visible.is_empty() {
                lines.push(Line::styled("No models match", theme::dim()));
            }
            let rows = (inner.height as usize).saturating_sub(2);
            let start = (picker.selected + 1).saturating_sub(rows);
            for (i, model) in visible.iter().enumerate().skip(start).take(rows) {
                let chosen = i == picker.selected;
                let bg = if chosen {
                    Style::default().bg(theme::SELECTED_BG)
                } else {
                    Style::default()
                };
                let current = if model.id == app.model { "✓ " } else { "  " };
                let ctx = model
                    .context_window
                    .map(|c| format!("{}k ctx", c / 1000))
                    .unwrap_or_default();
                let tools = if model.supports_tools {
                    "tools"
                } else {
                    "text only"
                };
                let left = format!("{current}{}", model.id);
                let right = format!("{}  {tools}  {ctx}", model.provider);
                let gap = w
                    .saturating_sub(left.chars().count() + right.chars().count())
                    .max(1);
                lines.push(Line::from(vec![
                    Span::styled(
                        truncate(&left, w.saturating_sub(right.chars().count() + 1)),
                        theme::bold(if chosen { theme::ACCENT } else { theme::TEXT }).patch(bg),
                    ),
                    Span::styled(" ".repeat(gap), bg),
                    Span::styled(right, theme::dim().patch(bg)),
                ]));
            }
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::super::app::{App, AppEvent};
    use super::super::transcript::Welcome;
    use super::*;
    use crate::agent::{AgentEvent, SharedMode};
    use crate::tools::{Todo, ToolOutput};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app() -> App {
        App::new(
            Welcome {
                user: "Ada <ada@example.com>".into(),
                model: "gpt-5.6-sol".into(),
                workspace: "personal".into(),
                cwd: "/work/app".into(),
            },
            SharedMode::new(PermissionMode::Default),
            Vec::new(),
        )
    }

    #[test]
    fn renders_welcome_input_and_footer() {
        let mut app = app();
        let s = screen(&mut app, 90, 24);
        assert!(s.contains("Welcome to Ironize"), "{s}");
        assert!(s.contains("Ada <ada@example.com>"));
        assert!(s.contains("Ask Ironize to build, fix or explain"));
        assert!(s.contains("gpt-5.6-sol · personal · ↑0 ↓0"));
    }

    #[test]
    fn renders_a_working_turn() {
        let mut app = app();
        for c in "add tests".chars() {
            app.on_key(crossterm::event::KeyEvent::from(
                crossterm::event::KeyCode::Char(c),
            ));
        }
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Enter,
        ));
        app.on_event(AppEvent::Agent(AgentEvent::TextDelta(
            "I'll **add** tests.".into(),
        )));
        app.on_event(AppEvent::Agent(AgentEvent::ToolStarted {
            id: "1".into(),
            label: "Bash",
            subject: "cargo test".into(),
        }));
        app.on_event(AppEvent::Agent(AgentEvent::ToolFinished {
            id: "1".into(),
            output: ToolOutput::ok("ok", "exit code 0 · 0.4s"),
            declined: false,
        }));
        app.on_event(AppEvent::Agent(AgentEvent::Todos(vec![Todo {
            content: "Write tests".into(),
            status: TodoStatus::InProgress,
        }])));
        let s = screen(&mut app, 90, 30);
        assert!(s.contains("› add tests"), "{s}");
        assert!(s.contains("● I'll add tests."));
        assert!(s.contains("● Bash(cargo test)"));
        assert!(s.contains("⎿ exit code 0 · 0.4s"));
        assert!(s.contains("◐ Write tests"));
        assert!(s.contains("esc to interrupt"));
    }

    #[test]
    fn renders_permission_prompt() {
        let mut app = app();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        app.on_event(AppEvent::Agent(AgentEvent::Permission(
            crate::agent::tests_support::permission_request(tx),
        )));
        let s = screen(&mut app, 90, 30);
        assert!(s.contains("Permission required"), "{s}");
        assert!(s.contains("Bash wants to run a command"));
        assert!(s.contains("$ cargo test"));
        assert!(s.contains("› 1. Yes"));
        assert!(s.contains("2. Yes, and don't ask again for Bash this session"));
    }

    #[test]
    fn survives_tiny_terminals() {
        let mut app = app();
        app.on_event(AppEvent::Agent(AgentEvent::TextDelta("x".repeat(500))));
        for (w, h) in [(10, 5), (1, 1), (30, 8)] {
            screen(&mut app, w, h);
        }
    }

    fn mouse(
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
    ) -> crossterm::event::Event {
        crossterm::event::Event::Mouse(crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        })
    }

    fn row_of(screen: &str, needle: &str) -> u16 {
        screen.lines().position(|l| l.contains(needle)).unwrap() as u16
    }

    #[test]
    fn drag_selects_highlights_and_copies() {
        use super::super::app::Action;
        use crossterm::event::{MouseButton, MouseEventKind};
        let mut app = app();
        app.on_event(AppEvent::Agent(AgentEvent::TextDelta(
            "first line of the answer\n\nsecond paragraph here".into(),
        )));
        let s = screen(&mut app, 60, 30);
        let (r0, r1) = (row_of(&s, "first line"), row_of(&s, "second paragraph"));

        app.on_terminal(mouse(MouseEventKind::Down(MouseButton::Left), 8, r0));
        app.on_terminal(mouse(MouseEventKind::Drag(MouseButton::Left), 30, r0));
        app.on_terminal(mouse(MouseEventKind::Drag(MouseButton::Left), 7, r1));
        app.on_terminal(mouse(MouseEventKind::Up(MouseButton::Left), 7, r1));
        assert_eq!(
            app.take_actions(),
            [Action::Copy("line of the answer\n\n  second".into())]
        );
        assert!(app
            .toast
            .as_ref()
            .unwrap()
            .0
            .starts_with("Copied 28 characters"));

        let mut terminal = Terminal::new(TestBackend::new(60, 30)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(8, r0)].bg, theme::SELECTION_BG);
        assert_ne!(buffer[(7, r0)].bg, theme::SELECTION_BG);
        assert_eq!(buffer[(7, r1)].bg, theme::SELECTION_BG);
        assert_ne!(buffer[(8, r1)].bg, theme::SELECTION_BG);

        // ctrl+c copies again instead of quitting, esc clears.
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        assert!(matches!(&app.take_actions()[..], [Action::Copy(_)]));
        assert!(app.selection.is_none());
        assert!(!app.should_quit);
    }

    #[test]
    fn double_click_copies_a_word_and_plain_click_clears() {
        use super::super::app::Action;
        use crossterm::event::{MouseButton, MouseEventKind};
        let mut app = app();
        app.on_event(AppEvent::Agent(AgentEvent::TextDelta(
            "edit src/tui/app.rs now".into(),
        )));
        let s = screen(&mut app, 60, 30);
        let r = row_of(&s, "edit src");
        let down = mouse(MouseEventKind::Down(MouseButton::Left), 12, r);
        let up = mouse(MouseEventKind::Up(MouseButton::Left), 12, r);
        app.on_terminal(down.clone());
        app.on_terminal(up.clone());
        assert!(app.selection.is_none(), "a click alone selects nothing");
        app.on_terminal(down);
        app.on_terminal(up);
        assert_eq!(app.take_actions(), [Action::Copy("src/tui/app.rs".into())]);
        app.on_terminal(mouse(MouseEventKind::Down(MouseButton::Left), 2, r + 3));
        assert!(app.selection.is_some());
    }

    #[test]
    fn pastes_into_prompt_and_toggles_mouse() {
        use super::super::app::Action;
        let mut app = app();
        app.on_terminal(crossterm::event::Event::Paste("from\r\nclipboard".into()));
        assert_eq!(app.input.text(), "from\nclipboard");
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('v'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        assert_eq!(app.take_actions(), [Action::Paste]);
        app.on_event(AppEvent::Pasted(Ok(" more".into())));
        assert_eq!(app.input.text(), "from\nclipboard more");
        app.submit_text("/mouse".into());
        assert_eq!(app.take_actions(), [Action::SetMouseCapture(false)]);
    }

    #[test]
    fn formats_token_counts() {
        assert_eq!(tokens(999), "999");
        assert_eq!(tokens(12_345), "12.3k");
        assert_eq!(tokens(2_500_000), "2.5M");
    }
}
