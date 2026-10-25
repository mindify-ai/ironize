//! Built-in tools the agent can call.

mod bash;
mod edit;
mod fetch;
mod fs;
mod search;
mod todo;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::cortex::ToolDefinition;

pub use bash::Bash;
pub use edit::{EditFile, WriteFile};
pub use fetch::WebFetch;
pub use fs::{ListDir, ReadFile};
pub use search::{Glob, Grep};
pub use todo::{Todo, TodoStatus, TodoWrite};

/// Largest tool result sent back to the model, in characters.
pub const MAX_OUTPUT_CHARS: usize = 30_000;

/// What a tool can do, which decides whether it needs the user's approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// Reads the workspace or updates harness state. Never needs approval.
    Read,
    /// Changes files.
    Edit,
    /// Runs arbitrary commands.
    Execute,
    /// Reaches the network.
    Network,
}

/// Shared state handed to every tool call.
#[derive(Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub todos: Arc<Mutex<Vec<Todo>>>,
    pub http: reqwest::Client,
}

impl ToolContext {
    pub fn new(cwd: PathBuf) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ironize/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::limited(5))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("static HTTP client configuration is valid");
        Self {
            cwd,
            todos: Default::default(),
            http,
        }
    }

    /// Resolves a model-supplied path against the working directory.
    pub fn resolve(&self, path: &str) -> PathBuf {
        let path = path.trim();
        let expanded = match path.strip_prefix("~/") {
            Some(rest) => dirs::home_dir()
                .map(|h| h.join(rest))
                .unwrap_or_else(|| PathBuf::from(path)),
            None => PathBuf::from(path),
        };
        if expanded.is_absolute() {
            expanded
        } else {
            self.cwd.join(expanded)
        }
    }

    /// A path for display: relative to the working directory when inside it.
    pub fn display(&self, path: &Path) -> String {
        path.strip_prefix(&self.cwd)
            .ok()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Hunk,
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

/// What the user sees about a tool call, besides its title.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ToolDisplay {
    #[default]
    None,
    /// Output lines; the UI shows the first few.
    Lines(Vec<String>),
    Diff(Vec<DiffLine>),
    Todos(Vec<Todo>),
}

/// Shown when asking for approval, before the tool runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Preview {
    #[default]
    None,
    Command(String),
    Diff(Vec<DiffLine>),
    Text(String),
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Sent back to the model.
    pub content: String,
    pub is_error: bool,
    /// One-line result for the UI, e.g. "Read 120 lines".
    pub brief: String,
    pub display: ToolDisplay,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>, brief: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            brief: brief.into(),
            display: ToolDisplay::None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            brief: message.lines().next().unwrap_or_default().to_string(),
            content: format!("Error: {message}"),
            is_error: true,
            display: ToolDisplay::None,
        }
    }

    pub fn with_display(mut self, display: ToolDisplay) -> Self {
        self.display = display;
        self
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    /// Wire name the model calls, e.g. `read_file`.
    fn name(&self) -> &'static str;
    /// Short name for the UI, e.g. `Read`.
    fn label(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// JSON Schema of the arguments.
    fn parameters(&self) -> Value;
    fn kind(&self) -> ToolKind;
    /// The call's subject for the UI, e.g. the path or command.
    fn subject(&self, args: &Value, ctx: &ToolContext) -> String;

    async fn preview(&self, _args: &Value, _ctx: &ToolContext) -> Preview {
        Preview::None
    }

    async fn run(&self, args: Value, ctx: &ToolContext, cancel: CancellationToken) -> ToolOutput;
}

/// Parses tool arguments, turning schema mistakes into a message the model can
/// act on.
pub(crate) fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, ToolOutput> {
    serde_json::from_value(args).map_err(|e| ToolOutput::error(format!("invalid arguments: {e}")))
}

pub(crate) fn str_arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Keeps the head and tail of long output so neither the command nor its
/// final error is lost.
pub fn truncate_middle(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let keep = max_chars / 2;
    let head: String = text.chars().take(keep).collect();
    let tail: String = text.chars().skip(count - keep).collect();
    format!(
        "{head}\n\n… [{} characters truncated] …\n\n{tail}",
        count - 2 * keep
    )
}

/// Builds a unified diff between two versions of a file.
pub fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut lines = Vec::new();
    for group in diff.grouped_ops(2) {
        let (first, last) = (&group[0], &group[group.len() - 1]);
        let old_range = first.old_range().start..last.old_range().end;
        let new_range = first.new_range().start..last.new_range().end;
        lines.push(DiffLine {
            kind: DiffKind::Hunk,
            text: format!(
                "@@ -{},{} +{},{} @@",
                old_range.start + 1,
                old_range.len(),
                new_range.start + 1,
                new_range.len()
            ),
        });
        for op in &group {
            for change in diff.iter_changes(op) {
                let kind = match change.tag() {
                    similar::ChangeTag::Equal => DiffKind::Context,
                    similar::ChangeTag::Insert => DiffKind::Added,
                    similar::ChangeTag::Delete => DiffKind::Removed,
                };
                lines.push(DiffLine {
                    kind,
                    text: change.value().trim_end_matches(['\n', '\r']).to_string(),
                });
            }
        }
    }
    lines
}

/// The tool set exposed to the model.
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn builtin() -> Self {
        Self {
            tools: vec![
                Arc::new(ReadFile),
                Arc::new(WriteFile),
                Arc::new(EditFile),
                Arc::new(ListDir),
                Arc::new(Glob),
                Arc::new(Grep),
                Arc::new(Bash),
                Arc::new(WebFetch),
                Arc::new(TodoWrite),
            ],
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.iter()
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|t| ToolDefinition::function(t.name(), t.description(), t.parameters()))
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub fn ctx() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        (dir, ctx)
    }

    pub async fn run(tool: &dyn Tool, ctx: &ToolContext, args: Value) -> ToolOutput {
        tool.run(args, ctx, CancellationToken::new()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_both_ends() {
        let text = format!("{}{}", "a".repeat(100), "z".repeat(100));
        let out = truncate_middle(&text, 20);
        assert!(out.starts_with("aaaaaaaaaa\n"));
        assert!(out.ends_with("\nzzzzzzzzzz"));
        assert!(out.contains("180 characters truncated"));
        assert_eq!(truncate_middle("short", 20), "short");
    }

    #[test]
    fn diff_marks_changes() {
        let lines = diff_lines("a\nb\nc\n", "a\nB\nc\n");
        let kinds: Vec<_> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                DiffKind::Hunk,
                DiffKind::Context,
                DiffKind::Removed,
                DiffKind::Added,
                DiffKind::Context
            ]
        );
        assert_eq!(lines[0].text, "@@ -1,3 +1,3 @@");
    }

    #[test]
    fn every_tool_has_a_valid_object_schema() {
        let registry = ToolRegistry::builtin();
        let mut names = std::collections::HashSet::new();
        for tool in registry.iter() {
            assert!(names.insert(tool.name()), "duplicate tool {}", tool.name());
            let schema = tool.parameters();
            assert_eq!(schema["type"], "object", "{}", tool.name());
            assert!(schema["properties"].is_object(), "{}", tool.name());
        }
    }

    #[test]
    fn resolves_relative_and_absolute_paths() {
        let ctx = ToolContext::new(PathBuf::from("/work"));
        assert_eq!(ctx.resolve("src/a.rs"), PathBuf::from("/work/src/a.rs"));
        assert_eq!(ctx.resolve("/etc/hosts"), PathBuf::from("/etc/hosts"));
        assert_eq!(ctx.display(Path::new("/work/src/a.rs")), "src/a.rs");
        assert_eq!(ctx.display(Path::new("/etc/hosts")), "/etc/hosts");
    }
}
