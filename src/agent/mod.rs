//! The agent loop: stream a model turn, run the tools it calls (asking the
//! user where required), feed the results back, and repeat until the model
//! answers without calling a tool.

mod prompt;

use std::collections::HashSet;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub use prompt::system_prompt;

use crate::cortex::chat::{ReplyBuilder, StreamEvent, Usage};
use crate::cortex::{Message, ToolCall, ToolDefinition};
use crate::gateway::Gateway;
use crate::tools::{Preview, Todo, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolRegistry};

/// Model round-trips allowed in one turn before Ironize stops to check in.
const MAX_STEPS: usize = 100;

/// Which tool calls run without asking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    /// Ask before editing files, running commands, or using the network.
    #[default]
    Default,
    /// Edit files freely; still ask for commands and network access.
    AcceptEdits,
    /// Never ask.
    Yolo,
}

impl PermissionMode {
    pub fn requires_approval(self, kind: ToolKind) -> bool {
        !matches!(
            (self, kind),
            (_, ToolKind::Read)
                | (PermissionMode::Yolo, _)
                | (PermissionMode::AcceptEdits, ToolKind::Edit)
        )
    }

    pub fn next(self) -> Self {
        match self {
            PermissionMode::Default => PermissionMode::AcceptEdits,
            PermissionMode::AcceptEdits => PermissionMode::Yolo,
            PermissionMode::Yolo => PermissionMode::Default,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PermissionMode::Default => "ask before changes",
            PermissionMode::AcceptEdits => "accept edits",
            PermissionMode::Yolo => "yolo — never ask",
        }
    }

    fn to_u8(self) -> u8 {
        self as u8
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => PermissionMode::AcceptEdits,
            2 => PermissionMode::Yolo,
            _ => PermissionMode::Default,
        }
    }
}

/// A permission mode the UI can change while a turn is running.
#[derive(Debug, Clone, Default)]
pub struct SharedMode(Arc<AtomicU8>);

impl SharedMode {
    pub fn new(mode: PermissionMode) -> Self {
        Self(Arc::new(AtomicU8::new(mode.to_u8())))
    }

    pub fn get(&self) -> PermissionMode {
        PermissionMode::from_u8(self.0.load(Ordering::Relaxed))
    }

    pub fn set(&self, mode: PermissionMode) {
        self.0.store(mode.to_u8(), Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Allow this tool for the rest of the session.
    AllowAlways,
    Deny,
}

/// A tool call waiting for the user's decision.
#[derive(Debug)]
pub struct PermissionRequest {
    pub tool: &'static str,
    pub label: &'static str,
    pub subject: String,
    pub kind: ToolKind,
    pub preview: Preview,
    respond: oneshot::Sender<Decision>,
}

impl PermissionRequest {
    pub fn respond(self, decision: Decision) {
        let _ = self.respond.send(decision);
    }
}

#[derive(Debug)]
pub enum AgentEvent {
    /// A request went to the model; waiting for its first token.
    Thinking,
    TextDelta(String),
    ToolStarted {
        id: String,
        label: &'static str,
        subject: String,
    },
    ToolFinished {
        id: String,
        output: ToolOutput,
        declined: bool,
    },
    Permission(PermissionRequest),
    Todos(Vec<Todo>),
    /// Cumulative token usage for the session.
    Usage(Usage),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Cancelled,
    /// The user denied a tool call; the model is waiting for direction.
    Denied,
    StepLimit,
    Failed(String),
}

pub struct Agent {
    gateway: Gateway,
    tools: ToolRegistry,
    definitions: Vec<ToolDefinition>,
    ctx: ToolContext,
    messages: Vec<Message>,
    mode: SharedMode,
    always_allow: HashSet<&'static str>,
    usage: Usage,
}

impl Agent {
    pub fn new(gateway: Gateway, ctx: ToolContext, mode: SharedMode) -> Self {
        let tools = ToolRegistry::builtin();
        let definitions = tools.definitions();
        let mut agent = Self {
            gateway,
            tools,
            definitions,
            ctx,
            messages: Vec::new(),
            mode,
            always_allow: HashSet::new(),
            usage: Usage::default(),
        };
        agent.reset();
        agent
    }

    pub fn gateway(&self) -> &Gateway {
        &self.gateway
    }

    pub fn gateway_mut(&mut self) -> &mut Gateway {
        &mut self.gateway
    }

    pub fn context(&self) -> &ToolContext {
        &self.ctx
    }

    pub fn usage(&self) -> Usage {
        self.usage
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Starts a fresh conversation (keeping session-wide permissions).
    pub fn reset(&mut self) {
        self.messages = vec![Message::system(system_prompt(&self.ctx.cwd, &self.tools))];
        self.ctx.todos.lock().expect("todo lock poisoned").clear();
    }

    /// Runs one user turn to completion, reporting progress on `events`.
    pub async fn run_turn(
        &mut self,
        input: String,
        events: &mpsc::UnboundedSender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> TurnOutcome {
        self.messages.push(Message::user(input));
        for _ in 0..MAX_STEPS {
            let _ = events.send(AgentEvent::Thinking);
            let stream = tokio::select! {
                s = self.gateway.stream(&self.messages, &self.definitions) => s,
                _ = cancel.cancelled() => return TurnOutcome::Cancelled,
            };
            let mut stream = match stream {
                Ok(s) => s,
                Err(e) => return TurnOutcome::Failed(e.to_string()),
            };

            let mut reply = ReplyBuilder::default();
            loop {
                let event = tokio::select! {
                    e = stream.next() => e,
                    _ = cancel.cancelled() => {
                        self.keep_partial(reply);
                        return TurnOutcome::Cancelled;
                    }
                };
                match event {
                    None => break,
                    Some(Ok(event)) => {
                        if let StreamEvent::Text(text) = &event {
                            let _ = events.send(AgentEvent::TextDelta(text.clone()));
                        }
                        reply.apply(&event);
                    }
                    Some(Err(e)) => {
                        self.keep_partial(reply);
                        return TurnOutcome::Failed(e.to_string());
                    }
                }
            }

            if let Some(usage) = reply.usage {
                self.usage.input_tokens += usage.input_tokens;
                self.usage.output_tokens += usage.output_tokens;
                let _ = events.send(AgentEvent::Usage(self.usage));
            }
            let calls = reply.tool_calls();
            self.messages.push(reply.into_message());
            if calls.is_empty() {
                return TurnOutcome::Completed;
            }

            let mut stopped: Option<TurnOutcome> = None;
            for call in calls {
                let content = match &stopped {
                    Some(_) => "Not run: the turn was stopped before this call.".to_string(),
                    None => {
                        let (content, stop) = self.run_call(&call, events, cancel).await;
                        stopped = stop;
                        content
                    }
                };
                self.messages.push(Message::Tool {
                    tool_call_id: call.id.clone(),
                    content,
                });
            }
            if let Some(outcome) = stopped {
                return outcome;
            }
        }
        TurnOutcome::StepLimit
    }

    /// Keeps streamed text from an interrupted reply so the model knows what
    /// the user already saw. Unfinished tool calls are dropped.
    fn keep_partial(&mut self, reply: ReplyBuilder) {
        if !reply.text.is_empty() {
            self.messages.push(Message::Assistant {
                content: Some(reply.text),
                tool_calls: Vec::new(),
            });
        }
    }

    async fn run_call(
        &mut self,
        call: &ToolCall,
        events: &mpsc::UnboundedSender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> (String, Option<TurnOutcome>) {
        let id = call.id.clone();
        let finish_as = |output: ToolOutput, declined: bool| {
            let content = output.content.clone();
            let _ = events.send(AgentEvent::ToolFinished {
                id: id.clone(),
                output,
                declined,
            });
            content
        };
        let finish = |output: ToolOutput| finish_as(output, false);

        let Some(tool) = self.tools.get(&call.function.name) else {
            let _ = events.send(AgentEvent::ToolStarted {
                id: id.clone(),
                label: "Tool",
                subject: call.function.name.clone(),
            });
            let names: Vec<_> = self.tools.iter().map(|t| t.name()).collect();
            let message = format!(
                "unknown tool {:?}; available tools: {}",
                call.function.name,
                names.join(", ")
            );
            return (finish(ToolOutput::error(message)), None);
        };
        let raw = call.function.arguments.trim();
        let args: Value = match serde_json::from_str(if raw.is_empty() { "{}" } else { raw }) {
            Ok(v) => v,
            Err(e) => {
                let _ = events.send(AgentEvent::ToolStarted {
                    id: id.clone(),
                    label: tool.label(),
                    subject: String::new(),
                });
                return (
                    finish(ToolOutput::error(format!(
                        "arguments are not valid JSON: {e}"
                    ))),
                    None,
                );
            }
        };
        let subject = tool.subject(&args, &self.ctx);
        let _ = events.send(AgentEvent::ToolStarted {
            id: id.clone(),
            label: tool.label(),
            subject: subject.clone(),
        });

        if self.mode.get().requires_approval(tool.kind())
            && !self.always_allow.contains(tool.name())
        {
            let (respond, decision) = oneshot::channel();
            let request = PermissionRequest {
                tool: tool.name(),
                label: tool.label(),
                subject,
                kind: tool.kind(),
                preview: tool.preview(&args, &self.ctx).await,
                respond,
            };
            let _ = events.send(AgentEvent::Permission(request));
            let decision = tokio::select! {
                d = decision => d.unwrap_or(Decision::Deny),
                _ = cancel.cancelled() => {
                    let mut out = ToolOutput::error("interrupted by the user");
                    out.brief = "Interrupted".into();
                    return (finish(out), Some(TurnOutcome::Cancelled));
                }
            };
            match decision {
                Decision::Allow => {}
                Decision::AllowAlways => {
                    self.always_allow.insert(tool.name());
                }
                Decision::Deny => {
                    let mut out = ToolOutput::error(
                        "The user declined this tool call. Stop and wait for the user to tell you how to proceed.",
                    );
                    out.brief = "Declined".into();
                    return (finish_as(out, true), Some(TurnOutcome::Denied));
                }
            }
        }

        let output = tool.run(args, &self.ctx, cancel.child_token()).await;
        if let ToolDisplay::Todos(todos) = &output.display {
            let _ = events.send(AgentEvent::Todos(todos.clone()));
        }
        let stop = cancel.is_cancelled().then_some(TurnOutcome::Cancelled);
        (finish(output), stop)
    }
}

#[doc(hidden)]
pub mod tests_support {
    use super::*;

    /// A permission request for UI tests.
    pub fn permission_request(respond: oneshot::Sender<Decision>) -> PermissionRequest {
        PermissionRequest {
            tool: "bash",
            label: "Bash",
            subject: "cargo test".into(),
            kind: ToolKind::Execute,
            preview: Preview::Command("cargo test".into()),
            respond,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_modes() {
        use ToolKind::*;
        let d = PermissionMode::Default;
        assert!(!d.requires_approval(Read));
        assert!(
            d.requires_approval(Edit)
                && d.requires_approval(Execute)
                && d.requires_approval(Network)
        );
        let a = PermissionMode::AcceptEdits;
        assert!(!a.requires_approval(Edit) && a.requires_approval(Execute));
        let y = PermissionMode::Yolo;
        assert!(!y.requires_approval(Execute) && !y.requires_approval(Network));
        assert_eq!(d.next().next().next(), d);

        let shared = SharedMode::new(PermissionMode::AcceptEdits);
        assert_eq!(shared.get(), PermissionMode::AcceptEdits);
        shared.set(PermissionMode::Yolo);
        assert_eq!(shared.clone().get(), PermissionMode::Yolo);
    }
}
