//! UI state and input handling, independent of the terminal so it can be
//! driven directly in tests. The event loop executes the [`Action`]s it emits.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};

use super::input::Input;
use super::transcript::{Entry, Level, ToolStatus, Transcript, Welcome};
use crate::agent::{
    AgentEvent, Decision, PermissionMode, PermissionRequest, SharedMode, TurnOutcome,
};
use crate::cortex::chat::Usage;
use crate::cortex::CatalogEntry;
use crate::tools::Todo;

const DOUBLE_PRESS: Duration = Duration::from_millis(1500);

/// Slash commands: name, argument hint, description.
pub const COMMANDS: &[(&str, &str, &str)] = &[
    ("/help", "", "Show commands and keyboard shortcuts"),
    ("/model", "[id]", "Switch the model"),
    (
        "/mode",
        "[default|accept-edits|yolo]",
        "Change when Ironize asks before acting",
    ),
    ("/clear", "", "Start a new conversation"),
    ("/status", "", "Show account, model and session details"),
    ("/todos", "", "Show the current task list"),
    ("/expand", "", "Toggle full tool output (ctrl+o)"),
    ("/exit", "", "Quit Ironize"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Submit(String),
    Interrupt,
    FetchModels,
    SetModel(String),
    ClearConversation,
    SaveMode(PermissionMode),
    Quit,
}

/// Results of background work, delivered to [`App::on_event`].
#[derive(Debug)]
pub enum AppEvent {
    Agent(AgentEvent),
    TurnFinished(TurnOutcome),
    Models(Result<Vec<CatalogEntry>, String>),
    ModelChanged(Result<String, String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    Thinking,
    Responding,
    Tool(String),
    Waiting,
}

pub struct Busy {
    pub since: Instant,
    pub activity: Activity,
}

pub struct Permission {
    pub request: PermissionRequest,
    pub selected: usize,
}

pub struct ModelPicker {
    pub models: Option<Vec<CatalogEntry>>,
    pub error: Option<String>,
    pub filter: String,
    pub selected: usize,
}

impl ModelPicker {
    pub fn visible(&self) -> Vec<&CatalogEntry> {
        let filter = self.filter.to_lowercase();
        self.models
            .iter()
            .flatten()
            .filter(|m| {
                filter.is_empty()
                    || m.id.to_lowercase().contains(&filter)
                    || m.display_name.to_lowercase().contains(&filter)
            })
            .collect()
    }
}

pub struct App {
    pub transcript: Transcript,
    pub input: Input,
    pub busy: Option<Busy>,
    pub permission: Option<Permission>,
    pub picker: Option<ModelPicker>,
    pub palette_selected: usize,
    pub todos: Vec<Todo>,
    pub usage: Usage,
    pub mode: SharedMode,
    pub model: String,
    pub workspace: String,
    pub user: String,
    pub cwd: String,
    pub expanded: bool,
    pub frame: usize,
    /// Lines scrolled up from the bottom; 0 follows new output.
    pub scroll: usize,
    pub viewport: usize,
    pub should_quit: bool,
    pub dirty: bool,
    pub history: Vec<String>,
    history_pos: Option<usize>,
    history_draft: String,
    queued: VecDeque<String>,
    actions: Vec<Action>,
    last_ctrl_c: Option<Instant>,
}

impl App {
    pub fn new(welcome: Welcome, mode: SharedMode, history: Vec<String>) -> Self {
        let mut transcript = Transcript::default();
        let (model, workspace, user, cwd) = (
            welcome.model.clone(),
            welcome.workspace.clone(),
            welcome.user.clone(),
            welcome.cwd.clone(),
        );
        transcript.push(Entry::Welcome(welcome));
        Self {
            transcript,
            input: Input::default(),
            busy: None,
            permission: None,
            picker: None,
            palette_selected: 0,
            todos: Vec::new(),
            usage: Usage::default(),
            mode,
            model,
            workspace,
            user,
            cwd,
            expanded: false,
            frame: 0,
            scroll: 0,
            viewport: 20,
            should_quit: false,
            dirty: true,
            history,
            history_pos: None,
            history_draft: String::new(),
            queued: VecDeque::new(),
            actions: Vec::new(),
            last_ctrl_c: None,
        }
    }

    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    pub fn notice(&mut self, level: Level, text: impl Into<String>) {
        self.transcript.push(Entry::Notice(level, text.into()));
        self.scroll = 0;
        self.dirty = true;
    }

    pub fn on_tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        if self.busy.is_some() {
            self.dirty = true;
        }
    }

    /// Slash commands matching the input, while the user is typing one.
    pub fn palette(&self) -> Vec<&'static (&'static str, &'static str, &'static str)> {
        let text = self.input.text();
        if !text.starts_with('/')
            || text.contains(char::is_whitespace)
            || self.permission.is_some()
            || self.picker.is_some()
        {
            return Vec::new();
        }
        COMMANDS
            .iter()
            .filter(|(name, _, _)| name.starts_with(text) && *name != text)
            .collect()
    }

    // ----- background events -------------------------------------------------

    pub fn on_event(&mut self, event: AppEvent) {
        self.dirty = true;
        match event {
            AppEvent::Agent(event) => self.on_agent(event),
            AppEvent::TurnFinished(outcome) => self.on_turn_finished(outcome),
            AppEvent::Models(result) => {
                if let Some(picker) = &mut self.picker {
                    match result {
                        Ok(models) => {
                            picker.selected =
                                models.iter().position(|m| m.id == self.model).unwrap_or(0);
                            picker.models = Some(models);
                        }
                        Err(e) => picker.error = Some(e),
                    }
                }
            }
            AppEvent::ModelChanged(Ok(model)) => {
                self.notice(Level::Info, format!("Model set to {model}"));
                self.model = model;
            }
            AppEvent::ModelChanged(Err(e)) => {
                self.notice(Level::Error, format!("Could not switch model: {e}"))
            }
        }
    }

    fn set_activity(&mut self, activity: Activity) {
        if let Some(busy) = &mut self.busy {
            busy.activity = activity;
        }
    }

    fn on_agent(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Thinking => self.set_activity(Activity::Thinking),
            AgentEvent::TextDelta(text) => {
                self.set_activity(Activity::Responding);
                match self.transcript.last_mut() {
                    Some(Entry::Assistant(current)) => current.push_str(&text),
                    _ => self.transcript.push(Entry::Assistant(text)),
                }
            }
            AgentEvent::ToolStarted { id, label, subject } => {
                self.set_activity(Activity::Tool(label.to_string()));
                self.transcript.push(Entry::Tool {
                    id,
                    label,
                    subject,
                    status: ToolStatus::Running,
                    brief: String::new(),
                    display: Default::default(),
                });
            }
            AgentEvent::ToolFinished {
                id,
                output,
                declined,
            } => {
                if let Some(Entry::Tool {
                    status,
                    brief,
                    display,
                    ..
                }) = self.transcript.tool_mut(&id)
                {
                    *status = match (declined, output.is_error) {
                        (true, _) => ToolStatus::Declined,
                        (false, true) => ToolStatus::Failed,
                        (false, false) => ToolStatus::Succeeded,
                    };
                    *brief = output.brief;
                    *display = output.display;
                }
                self.set_activity(Activity::Thinking);
            }
            AgentEvent::Permission(request) => {
                self.set_activity(Activity::Waiting);
                self.permission = Some(Permission {
                    request,
                    selected: 0,
                });
                self.scroll = 0;
            }
            AgentEvent::Todos(todos) => self.todos = todos,
            AgentEvent::Usage(usage) => self.usage = usage,
        }
    }

    fn on_turn_finished(&mut self, outcome: TurnOutcome) {
        self.busy = None;
        if let Some(permission) = self.permission.take() {
            permission.request.respond(Decision::Deny);
        }
        match outcome {
            TurnOutcome::Completed | TurnOutcome::Denied => {}
            TurnOutcome::Cancelled => {
                self.notice(Level::Warn, "Interrupted · tell Ironize what to do instead")
            }
            TurnOutcome::StepLimit => self.notice(
                Level::Warn,
                "Stopped after many steps · send a message to continue",
            ),
            TurnOutcome::Failed(e) => {
                let hint = if e.contains("not signed in") {
                    " · run `ironize login` and restart"
                } else {
                    ""
                };
                self.notice(Level::Error, format!("{e}{hint}"));
            }
        }
        if let Some(next) = self.queued.pop_front() {
            self.start_turn(next);
        }
    }

    fn start_turn(&mut self, text: String) {
        self.transcript.push(Entry::User(text.clone()));
        self.busy = Some(Busy {
            since: Instant::now(),
            activity: Activity::Thinking,
        });
        self.scroll = 0;
        self.actions.push(Action::Submit(text));
    }

    // ----- terminal events ---------------------------------------------------

    pub fn on_terminal(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
            Event::Paste(text) => {
                if self.permission.is_none() && self.picker.is_none() {
                    self.input.insert(&text);
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll_by(3),
                MouseEventKind::ScrollDown => self.scroll_by(-3),
                _ => return,
            },
            Event::Resize(..) => self.transcript.invalidate(),
            _ => return,
        }
        self.dirty = true;
    }

    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta);
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return self.ctrl_c();
        }
        if self.permission.is_some() {
            return self.permission_key(key);
        }
        if self.picker.is_some() {
            return self.picker_key(key);
        }
        if key.code != KeyCode::Char('c') {
            self.last_ctrl_c = None;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let palette = self.palette();

        match key.code {
            KeyCode::Enter if shift || alt => self.input.insert("\n"),
            KeyCode::Char('j') if ctrl => self.input.insert("\n"),
            KeyCode::Enter => {
                if let Some((name, _, _)) =
                    palette.get(self.palette_selected.min(palette.len().saturating_sub(1)))
                {
                    self.input.set(*name);
                }
                if self.input.text().ends_with('\\') {
                    self.input.backspace();
                    self.input.insert("\n");
                } else {
                    self.submit();
                }
            }
            KeyCode::Tab if !palette.is_empty() => {
                let (name, _, _) = palette[self.palette_selected.min(palette.len() - 1)];
                self.input.set(format!("{name} "));
                self.palette_selected = 0;
            }
            KeyCode::BackTab => self.cycle_mode(None),
            KeyCode::Esc => {
                if self.busy.is_some() {
                    self.actions.push(Action::Interrupt);
                } else if !self.input.is_empty() && self.input.text().starts_with('/') {
                    self.input.take();
                }
            }
            KeyCode::Char('o') if ctrl => self.toggle_expanded(),
            KeyCode::Char('l') if ctrl => self.scroll = 0,
            KeyCode::Char('d') if ctrl && self.input.is_empty() => self.quit(),
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Char('u') if ctrl => self.input.kill_line_back(),
            KeyCode::Char('k') if ctrl => self.input.kill_line_forward(),
            KeyCode::Char('w') if ctrl => self.input.delete_word_back(),
            KeyCode::Backspace if alt || ctrl => self.input.delete_word_back(),
            KeyCode::Char('b') if alt => self.input.word_left(),
            KeyCode::Char('f') if alt => self.input.word_right(),
            KeyCode::Left if alt || ctrl => self.input.word_left(),
            KeyCode::Right if alt || ctrl => self.input.word_right(),
            KeyCode::Char(c) if !ctrl => {
                let mut buf = [0u8; 4];
                self.input.insert(c.encode_utf8(&mut buf));
                self.palette_selected = 0;
            }
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::Up if !palette.is_empty() => {
                self.palette_selected = self.palette_selected.saturating_sub(1)
            }
            KeyCode::Down if !palette.is_empty() => {
                self.palette_selected = (self.palette_selected + 1).min(palette.len() - 1)
            }
            KeyCode::Up => {
                if !self.input.up() {
                    self.history_prev();
                }
            }
            KeyCode::Down => {
                if !self.input.down() {
                    self.history_next();
                }
            }
            KeyCode::PageUp => self.scroll_by((self.viewport.max(2) - 1) as isize),
            KeyCode::PageDown => self.scroll_by(-((self.viewport.max(2) - 1) as isize)),
            _ => {}
        }
    }

    fn ctrl_c(&mut self) {
        if let Some(permission) = self.permission.take() {
            permission.request.respond(Decision::Deny);
            self.actions.push(Action::Interrupt);
        } else if self.picker.is_some() {
            self.picker = None;
        } else if self.busy.is_some() {
            self.actions.push(Action::Interrupt);
        } else if !self.input.is_empty() {
            self.input.take();
        } else if self.last_ctrl_c.is_some_and(|t| t.elapsed() < DOUBLE_PRESS) {
            self.quit();
        } else {
            self.last_ctrl_c = Some(Instant::now());
            self.notice(Level::Info, "Press Ctrl+C again to exit");
        }
        self.dirty = true;
    }

    fn quit(&mut self) {
        self.should_quit = true;
        self.actions.push(Action::Quit);
    }

    fn toggle_expanded(&mut self) {
        self.expanded = !self.expanded;
        self.transcript.invalidate();
    }

    fn cycle_mode(&mut self, to: Option<PermissionMode>) {
        let mode = to.unwrap_or_else(|| self.mode.get().next());
        self.mode.set(mode);
        self.actions.push(Action::SaveMode(mode));
    }

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let pos = match self.history_pos {
            None => {
                self.history_draft = self.input.text().to_string();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.history_pos = Some(pos);
        self.input.set(self.history[pos].clone());
    }

    fn history_next(&mut self) {
        let Some(pos) = self.history_pos else { return };
        if pos + 1 < self.history.len() {
            self.history_pos = Some(pos + 1);
            self.input.set(self.history[pos + 1].clone());
        } else {
            self.history_pos = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.input.set(draft);
        }
    }

    fn submit(&mut self) {
        let text = self.input.take();
        self.submit_text(text);
    }

    /// Sends a prompt or runs a slash command, as if typed and submitted.
    pub fn submit_text(&mut self, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.history_pos = None;
        self.palette_selected = 0;
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        if text.starts_with('/') {
            return self.command(&text);
        }
        if self.busy.is_some() {
            self.queued.push_back(text);
            self.notice(
                Level::Info,
                "Queued · will send when Ironize finishes this turn",
            );
        } else {
            self.start_turn(text);
        }
    }

    fn command(&mut self, line: &str) {
        let mut parts = line.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or_default();
        let arg = parts.next().map(str::trim).unwrap_or_default();
        match name {
            "/help" | "/?" => self.help(),
            "/exit" | "/quit" => self.quit(),
            "/expand" => self.toggle_expanded(),
            "/clear" | "/new" => {
                if self.busy.is_some() {
                    return self.notice(
                        Level::Warn,
                        "Wait for the current turn to finish (esc to interrupt)",
                    );
                }
                self.transcript.clear();
                self.transcript.push(Entry::Welcome(Welcome {
                    user: self.user.clone(),
                    model: self.model.clone(),
                    workspace: self.workspace.clone(),
                    cwd: self.cwd.clone(),
                }));
                self.todos.clear();
                self.queued.clear();
                self.actions.push(Action::ClearConversation);
            }
            "/model" => {
                if self.busy.is_some() {
                    return self.notice(
                        Level::Warn,
                        "Wait for the current turn to finish before switching models",
                    );
                }
                if arg.is_empty() {
                    self.picker = Some(ModelPicker {
                        models: None,
                        error: None,
                        filter: String::new(),
                        selected: 0,
                    });
                    self.actions.push(Action::FetchModels);
                } else {
                    self.actions.push(Action::SetModel(arg.to_string()));
                }
            }
            "/mode" => {
                let mode = match arg {
                    "" => None,
                    "default" | "ask" => Some(PermissionMode::Default),
                    "accept-edits" | "edits" => Some(PermissionMode::AcceptEdits),
                    "yolo" => Some(PermissionMode::Yolo),
                    other => {
                        return self.notice(
                            Level::Error,
                            format!("Unknown mode {other:?}; use default, accept-edits or yolo"),
                        )
                    }
                };
                self.cycle_mode(mode);
                self.notice(
                    Level::Info,
                    format!("Permission mode: {}", self.mode.get().label()),
                );
            }
            "/status" => {
                let text = format!(
                    "Account    {}\nModel      {}\nWorkspace  {}\nMode       {}\nDirectory  {}\nTokens     {} in · {} out",
                    self.user,
                    self.model,
                    self.workspace,
                    self.mode.get().label(),
                    self.cwd,
                    self.usage.input_tokens,
                    self.usage.output_tokens
                );
                self.notice(Level::Info, text);
            }
            "/todos" => {
                if self.todos.is_empty() {
                    self.notice(Level::Info, "No todos yet");
                } else {
                    let text: Vec<String> = self
                        .todos
                        .iter()
                        .map(|t| format!("{:?} · {}", t.status, t.content))
                        .collect();
                    self.notice(Level::Info, text.join("\n"));
                }
            }
            other => self.notice(
                Level::Error,
                format!("Unknown command {other} · type /help"),
            ),
        }
    }

    fn help(&mut self) {
        let mut text = String::from("Commands\n");
        for (name, args, description) in COMMANDS {
            text.push_str(&format!(
                "  {:<28} {description}\n",
                format!("{name} {args}")
            ));
        }
        text.push_str(
            "\nKeys\n  enter                        send (queued while Ironize works)\n  \
             shift+enter · ctrl+j · \\⏎    new line\n  \
             esc                          interrupt\n  \
             shift+tab                    cycle permission mode\n  \
             ↑ ↓                          history\n  \
             pgup pgdn · wheel            scroll\n  \
             ctrl+o                       expand tool output\n  \
             ctrl+c ctrl+c · ctrl+d       quit",
        );
        self.notice(Level::Info, text);
    }

    fn permission_key(&mut self, key: KeyEvent) {
        let Some(permission) = &mut self.permission else {
            return;
        };
        let decision = match key.code {
            KeyCode::Up => {
                permission.selected = permission.selected.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Tab => {
                permission.selected = (permission.selected + 1).min(2);
                None
            }
            KeyCode::Char('1') | KeyCode::Char('y') => Some(Decision::Allow),
            KeyCode::Char('2') | KeyCode::Char('a') => Some(Decision::AllowAlways),
            KeyCode::Char('3') | KeyCode::Char('n') | KeyCode::Esc => Some(Decision::Deny),
            KeyCode::Enter => {
                Some([Decision::Allow, Decision::AllowAlways, Decision::Deny][permission.selected])
            }
            KeyCode::BackTab => {
                self.cycle_mode(None);
                None
            }
            _ => None,
        };
        if let Some(decision) = decision {
            let permission = self.permission.take().expect("checked above");
            permission.request.respond(decision);
            self.set_activity(Activity::Thinking);
        }
    }

    fn picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        let count = picker.visible().len();
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(count.saturating_sub(1)),
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.filter.push(c);
                picker.selected = 0;
            }
            KeyCode::Enter => {
                let chosen = picker.visible().get(picker.selected).map(|m| m.id.clone());
                if let Some(id) = chosen {
                    self.picker = None;
                    if id != self.model {
                        self.actions.push(Action::SetModel(id));
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolOutput;

    fn app() -> App {
        App::new(
            Welcome {
                user: "ada@example.com".into(),
                model: "gpt-x".into(),
                workspace: "personal".into(),
                cwd: "/work".into(),
            },
            SharedMode::new(PermissionMode::Default),
            vec!["earlier prompt".into()],
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn submitting_starts_a_turn_and_queues_while_busy() {
        let mut app = app();
        type_text(&mut app, "fix the bug");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.take_actions(), [Action::Submit("fix the bug".into())]);
        assert!(app.busy.is_some());
        assert!(app.input.is_empty());

        type_text(&mut app, "and add tests");
        app.on_key(key(KeyCode::Enter));
        assert!(app.take_actions().is_empty(), "queued, not sent");

        app.on_event(AppEvent::Agent(AgentEvent::TextDelta("Done".into())));
        app.on_event(AppEvent::TurnFinished(TurnOutcome::Completed));
        assert_eq!(app.take_actions(), [Action::Submit("and add tests".into())]);
    }

    #[test]
    fn streams_text_and_tool_cards() {
        let mut app = app();
        app.start_turn("hi".into());
        app.on_event(AppEvent::Agent(AgentEvent::TextDelta("Hel".into())));
        app.on_event(AppEvent::Agent(AgentEvent::TextDelta("lo".into())));
        app.on_event(AppEvent::Agent(AgentEvent::ToolStarted {
            id: "t1".into(),
            label: "Read",
            subject: "a.rs".into(),
        }));
        assert_eq!(
            app.busy.as_ref().unwrap().activity,
            Activity::Tool("Read".into())
        );
        app.on_event(AppEvent::Agent(AgentEvent::ToolFinished {
            id: "t1".into(),
            output: ToolOutput::ok("x", "Read 3 lines"),
            declined: false,
        }));
        let entries = app.transcript.entries();
        assert!(matches!(&entries[2], Entry::Assistant(t) if t == "Hello"));
        assert!(
            matches!(&entries[3], Entry::Tool { status: ToolStatus::Succeeded, brief, .. } if brief == "Read 3 lines")
        );
    }

    #[test]
    fn esc_interrupts_and_ctrl_c_twice_quits() {
        let mut app = app();
        app.start_turn("long task".into());
        app.take_actions();
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.take_actions(), [Action::Interrupt]);
        app.on_event(AppEvent::TurnFinished(TurnOutcome::Cancelled));
        assert!(app.busy.is_none());

        app.on_key(ctrl('c'));
        assert!(!app.should_quit);
        app.on_key(ctrl('c'));
        assert!(app.should_quit);
    }

    #[test]
    fn slash_palette_completes_and_runs_commands() {
        let mut app = app();
        type_text(&mut app, "/mo");
        let names: Vec<_> = app.palette().iter().map(|c| c.0).collect();
        assert_eq!(names, ["/model", "/mode"]);
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.input.text(), "/mode ");
        type_text(&mut app, "yolo");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.mode.get(), PermissionMode::Yolo);
        assert_eq!(app.take_actions(), [Action::SaveMode(PermissionMode::Yolo)]);

        type_text(&mut app, "/model");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.take_actions(), [Action::FetchModels]);
        assert!(app.picker.is_some());
    }

    #[test]
    fn shift_tab_cycles_modes() {
        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.mode.get(), PermissionMode::AcceptEdits);
    }

    #[test]
    fn history_navigation_restores_draft() {
        let mut app = app();
        type_text(&mut app, "draft");
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.input.text(), "earlier prompt");
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.input.text(), "draft");
    }

    #[test]
    fn backslash_enter_inserts_newline() {
        let mut app = app();
        type_text(&mut app, "line one\\");
        app.on_key(key(KeyCode::Enter));
        type_text(&mut app, "line two");
        assert_eq!(app.input.text(), "line one\nline two");
        assert!(app.take_actions().is_empty());
    }

    #[tokio::test]
    async fn permission_prompt_answers_request() {
        use tokio::sync::oneshot;
        let mut app = app();
        app.start_turn("run it".into());
        let (tx, rx) = oneshot::channel();
        let request = crate::agent::tests_support::permission_request(tx);
        app.on_event(AppEvent::Agent(AgentEvent::Permission(request)));
        assert!(app.permission.is_some());
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(rx.await.unwrap(), Decision::AllowAlways);
        assert!(app.permission.is_none());
    }
}
