//! The agent's task list, shown live in the UI.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{parse_args, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Todo {
    pub content: String,
    pub status: TodoStatus,
}

pub struct TodoWrite;

#[derive(Deserialize)]
struct TodoArgs {
    todos: Vec<Todo>,
}

#[async_trait]
impl Tool for TodoWrite {
    fn name(&self) -> &'static str {
        "todo_write"
    }

    fn label(&self) -> &'static str {
        "Todos"
    }

    fn description(&self) -> &'static str {
        "Replace your task list for the current request. Use it for work with three \
         or more steps: add the steps up front, keep exactly one `in_progress`, and \
         mark each `completed` as soon as it is done. The user sees the list live."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn subject(&self, args: &Value, _ctx: &ToolContext) -> String {
        let n = args
            .get("todos")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        format!("{n} item{}", if n == 1 { "" } else { "s" })
    }

    async fn run(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> ToolOutput {
        let args: TodoArgs = match parse_args(args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        *ctx.todos.lock().expect("todo lock poisoned") = args.todos.clone();
        let done = args
            .todos
            .iter()
            .filter(|t| t.status == TodoStatus::Completed)
            .count();
        let listing: Vec<String> = args
            .todos
            .iter()
            .map(|t| {
                let mark = match t.status {
                    TodoStatus::Completed => "[x]",
                    TodoStatus::InProgress => "[~]",
                    TodoStatus::Pending => "[ ]",
                };
                format!("{mark} {}", t.content)
            })
            .collect();
        ToolOutput::ok(
            format!("Todo list updated:\n{}", listing.join("\n")),
            format!("{done}/{} done", args.todos.len()),
        )
        .with_display(ToolDisplay::Todos(args.todos))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{ctx, run};
    use super::*;

    #[tokio::test]
    async fn replaces_shared_list() {
        let (_dir, ctx) = ctx();
        let out = run(
            &TodoWrite,
            &ctx,
            json!({ "todos": [
                { "content": "Write tests", "status": "completed" },
                { "content": "Fix bug", "status": "in_progress" }
            ] }),
        )
        .await;
        assert_eq!(out.brief, "1/2 done");
        assert_eq!(ctx.todos.lock().unwrap().len(), 2);
        assert!(
            run(
                &TodoWrite,
                &ctx,
                json!({ "todos": [{ "content": "x", "status": "bogus" }] })
            )
            .await
            .is_error
        );
    }
}
