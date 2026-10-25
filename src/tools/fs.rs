//! Reading files and listing directories.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{parse_args, str_arg, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput};

const DEFAULT_READ_LINES: usize = 2000;
const MAX_LINE_CHARS: usize = 2000;
const MAX_LIST_ENTRIES: usize = 500;

pub struct ReadFile;

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn label(&self) -> &'static str {
        "Read"
    }

    fn description(&self) -> &'static str {
        "Read a text file. Returns lines prefixed with their 1-based line numbers \
         (`   42\\t…`). Reads up to 2000 lines; use `offset` and `limit` for larger \
         files. Always read a file before editing it."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, absolute or relative to the working directory." },
                "offset": { "type": "integer", "minimum": 1, "description": "1-based line to start from." },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines to read." }
            },
            "required": ["path"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn subject(&self, args: &Value, ctx: &ToolContext) -> String {
        ctx.display(&ctx.resolve(str_arg(args, "path")))
    }

    async fn run(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> ToolOutput {
        let args: ReadArgs = match parse_args(args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let path = ctx.resolve(&args.path);
        if path.is_dir() {
            return ToolOutput::error(format!(
                "{} is a directory; use `ls` instead",
                ctx.display(&path)
            ));
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::error(format!("cannot read {}: {e}", ctx.display(&path))),
        };
        if bytes.iter().take(8192).any(|b| *b == 0) {
            return ToolOutput::error(format!(
                "{} looks like a binary file ({} bytes)",
                ctx.display(&path),
                bytes.len()
            ));
        }
        let text = String::from_utf8_lossy(&bytes);
        if text.is_empty() {
            return ToolOutput::ok("(the file is empty)", "Empty file");
        }
        let total = text.lines().count();
        let start = args.offset.unwrap_or(1).max(1);
        let limit = args.limit.unwrap_or(DEFAULT_READ_LINES).max(1);
        if start > total {
            return ToolOutput::error(format!(
                "offset {start} is past the end of the file ({total} lines)"
            ));
        }
        let mut out = String::new();
        let mut shown = 0;
        for (i, line) in text.lines().enumerate().skip(start - 1).take(limit) {
            let line: String = if line.chars().count() > MAX_LINE_CHARS {
                line.chars()
                    .take(MAX_LINE_CHARS)
                    .chain("…".chars())
                    .collect()
            } else {
                line.to_string()
            };
            out.push_str(&format!("{:>6}\t{line}\n", i + 1));
            shown += 1;
        }
        let end = start + shown - 1;
        if end < total {
            out.push_str(&format!(
                "\n[showing lines {start}-{end} of {total}; pass offset={} to continue]\n",
                end + 1
            ));
        }
        let brief = if shown == total {
            format!("Read {total} lines")
        } else {
            format!("Read lines {start}-{end} of {total}")
        };
        ToolOutput::ok(out, brief)
    }
}

pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &'static str {
        "ls"
    }

    fn label(&self) -> &'static str {
        "List"
    }

    fn description(&self) -> &'static str {
        "List a directory's entries (directories first, marked with a trailing /). \
         Use `glob` to find files recursively."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to list. Defaults to the working directory." }
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn subject(&self, args: &Value, ctx: &ToolContext) -> String {
        match str_arg(args, "path") {
            "" => ".".into(),
            p => ctx.display(&ctx.resolve(p)),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> ToolOutput {
        let path = match str_arg(&args, "path") {
            "" => ctx.cwd.clone(),
            p => ctx.resolve(p),
        };
        let mut reader = match tokio::fs::read_dir(&path).await {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(format!("cannot list {}: {e}", ctx.display(&path))),
        };
        let mut entries = Vec::new();
        while let Ok(Some(entry)) = reader.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            entries.push((!is_dir, if is_dir { format!("{name}/") } else { name }));
        }
        entries.sort();
        let total = entries.len();
        let mut lines: Vec<String> = entries
            .into_iter()
            .take(MAX_LIST_ENTRIES)
            .map(|(_, n)| n)
            .collect();
        if total > MAX_LIST_ENTRIES {
            lines.push(format!("… and {} more", total - MAX_LIST_ENTRIES));
        }
        let content = if lines.is_empty() {
            "(empty directory)".to_string()
        } else {
            lines.join("\n")
        };
        ToolOutput::ok(content, format!("{total} entries")).with_display(ToolDisplay::Lines(lines))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{ctx, run};
    use super::*;

    #[tokio::test]
    async fn reads_with_line_numbers_and_ranges() {
        let (dir, ctx) = ctx();
        let body: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.path().join("a.txt"), body).unwrap();

        let out = run(&ReadFile, &ctx, json!({ "path": "a.txt" })).await;
        assert!(!out.is_error);
        assert!(out.content.starts_with("     1\tline 1\n"));
        assert_eq!(out.brief, "Read 10 lines");

        let out = run(
            &ReadFile,
            &ctx,
            json!({ "path": "a.txt", "offset": 4, "limit": 2 }),
        )
        .await;
        assert_eq!(
            out.content.lines().take(2).collect::<Vec<_>>(),
            ["     4\tline 4", "     5\tline 5"]
        );
        assert!(out.content.contains("pass offset=6"));

        assert!(
            run(&ReadFile, &ctx, json!({ "path": "missing.txt" }))
                .await
                .is_error
        );
        assert!(
            run(&ReadFile, &ctx, json!({ "path": "a.txt", "offset": 99 }))
                .await
                .is_error
        );
        std::fs::write(dir.path().join("bin"), [0u8, 1, 2]).unwrap();
        assert!(run(&ReadFile, &ctx, json!({ "path": "bin" }))
            .await
            .content
            .contains("binary"));
    }

    #[tokio::test]
    async fn lists_directories_first() {
        let (dir, ctx) = ctx();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        let out = run(&ListDir, &ctx, json!({})).await;
        assert_eq!(out.content, "src/\na.txt\nb.txt");
    }
}
