//! Creating and editing files.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{
    diff_lines, parse_args, str_arg, Preview, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput,
};

pub struct WriteFile;

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn label(&self) -> &'static str {
        "Write"
    }

    fn description(&self) -> &'static str {
        "Create a file, or overwrite one completely, with the given content. Parent \
         directories are created. Prefer `edit_file` for changes to existing files."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, absolute or relative to the working directory." },
                "content": { "type": "string", "description": "The complete new file content." }
            },
            "required": ["path", "content"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    fn subject(&self, args: &Value, ctx: &ToolContext) -> String {
        ctx.display(&ctx.resolve(str_arg(args, "path")))
    }

    async fn preview(&self, args: &Value, ctx: &ToolContext) -> Preview {
        let path = ctx.resolve(str_arg(args, "path"));
        let old = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        Preview::Diff(diff_lines(&old, str_arg(args, "content")))
    }

    async fn run(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> ToolOutput {
        let args: WriteArgs = match parse_args(args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let path = ctx.resolve(&args.path);
        let shown = ctx.display(&path);
        let old = tokio::fs::read_to_string(&path).await.ok();
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::error(format!("cannot create {}: {e}", parent.display()));
            }
        }
        if let Err(e) = tokio::fs::write(&path, &args.content).await {
            return ToolOutput::error(format!("cannot write {shown}: {e}"));
        }
        let lines = args.content.lines().count();
        let (content, brief) = match old {
            Some(_) => (
                format!("Overwrote {shown} ({lines} lines)."),
                format!("Wrote {lines} lines"),
            ),
            None => (
                format!("Created {shown} ({lines} lines)."),
                format!("Created with {lines} lines"),
            ),
        };
        let diff = diff_lines(old.as_deref().unwrap_or_default(), &args.content);
        ToolOutput::ok(content, brief).with_display(ToolDisplay::Diff(diff))
    }
}

pub struct EditFile;

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

impl EditArgs {
    /// Applies the edit to `text`, or explains why it cannot be applied.
    fn apply(&self, text: &str) -> Result<(String, usize), String> {
        if self.old_string.is_empty() {
            return Err("old_string is empty; use write_file to create a file".into());
        }
        if self.old_string == self.new_string {
            return Err("old_string and new_string are identical".into());
        }
        let count = text.matches(&self.old_string).count();
        match count {
            0 => Err("old_string was not found in the file. Read the file again and copy the text exactly, including whitespace".into()),
            n if n > 1 && !self.replace_all => Err(format!(
                "old_string occurs {n} times. Add surrounding context to make it unique, or set replace_all"
            )),
            n if self.replace_all => Ok((text.replace(&self.old_string, &self.new_string), n)),
            _ => Ok((text.replacen(&self.old_string, &self.new_string, 1), 1)),
        }
    }
}

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn label(&self) -> &'static str {
        "Edit"
    }

    fn description(&self) -> &'static str {
        "Replace an exact string in a file. `old_string` must match the file exactly \
         (including indentation) and be unique unless `replace_all` is true. Read the \
         file first."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, absolute or relative to the working directory." },
                "old_string": { "type": "string", "description": "Exact text to replace." },
                "new_string": { "type": "string", "description": "Replacement text." },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence. Default false." }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    fn subject(&self, args: &Value, ctx: &ToolContext) -> String {
        ctx.display(&ctx.resolve(str_arg(args, "path")))
    }

    async fn preview(&self, args: &Value, ctx: &ToolContext) -> Preview {
        let Ok(edit) = serde_json::from_value::<EditArgs>(args.clone()) else {
            return Preview::None;
        };
        let Ok(old) = tokio::fs::read_to_string(ctx.resolve(&edit.path)).await else {
            return Preview::Text("The file does not exist.".into());
        };
        match edit.apply(&old) {
            Ok((new, _)) => Preview::Diff(diff_lines(&old, &new)),
            Err(e) => Preview::Text(e),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> ToolOutput {
        let args: EditArgs = match parse_args(args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let path = ctx.resolve(&args.path);
        let shown = ctx.display(&path);
        let old = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) => return ToolOutput::error(format!("cannot read {shown}: {e}")),
        };
        let (new, count) = match args.apply(&old) {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(e),
        };
        if let Err(e) = tokio::fs::write(&path, &new).await {
            return ToolOutput::error(format!("cannot write {shown}: {e}"));
        }
        let diff = diff_lines(&old, &new);
        let added = diff
            .iter()
            .filter(|l| l.kind == super::DiffKind::Added)
            .count();
        let removed = diff
            .iter()
            .filter(|l| l.kind == super::DiffKind::Removed)
            .count();
        let plural = if count == 1 { "" } else { "s" };
        ToolOutput::ok(
            format!("Edited {shown}: replaced {count} occurrence{plural}."),
            format!("+{added} -{removed}"),
        )
        .with_display(ToolDisplay::Diff(diff))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{ctx, run};
    use super::*;

    #[tokio::test]
    async fn write_creates_parents_and_reports_diff() {
        let (dir, ctx) = ctx();
        let out = run(
            &WriteFile,
            &ctx,
            json!({ "path": "a/b/c.txt", "content": "x\ny\n" }),
        )
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap(),
            "x\ny\n"
        );
        assert_eq!(out.brief, "Created with 2 lines");
        assert!(matches!(out.display, ToolDisplay::Diff(ref d) if d.len() == 3));
    }

    #[tokio::test]
    async fn edit_requires_unique_match() {
        let (dir, ctx) = ctx();
        let file = dir.path().join("f.rs");
        std::fs::write(&file, "let a = 1;\nlet a = 1;\nlet b = 2;\n").unwrap();

        let dup = run(
            &EditFile,
            &ctx,
            json!({ "path": "f.rs", "old_string": "let a = 1;", "new_string": "let a = 3;" }),
        )
        .await;
        assert!(dup.is_error && dup.content.contains("occurs 2 times"));

        let missing = run(
            &EditFile,
            &ctx,
            json!({ "path": "f.rs", "old_string": "nope", "new_string": "x" }),
        )
        .await;
        assert!(missing.is_error && missing.content.contains("not found"));

        let ok = run(
            &EditFile,
            &ctx,
            json!({ "path": "f.rs", "old_string": "let b = 2;", "new_string": "let b = 4;" }),
        )
        .await;
        assert!(!ok.is_error, "{}", ok.content);
        assert_eq!(ok.brief, "+1 -1");

        let all = run(&EditFile, &ctx, json!({ "path": "f.rs", "old_string": "a = 1", "new_string": "a = 9", "replace_all": true })).await;
        assert!(all.content.contains("2 occurrences"));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "let a = 9;\nlet a = 9;\nlet b = 4;\n"
        );
    }

    #[tokio::test]
    async fn edit_preview_shows_the_diff_without_writing() {
        let (dir, ctx) = ctx();
        std::fs::write(dir.path().join("f.txt"), "old\n").unwrap();
        let preview = EditFile
            .preview(
                &json!({ "path": "f.txt", "old_string": "old", "new_string": "new" }),
                &ctx,
            )
            .await;
        assert!(matches!(preview, Preview::Diff(_)));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "old\n"
        );
    }
}
