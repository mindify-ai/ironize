//! Finding files by name and searching their contents. Both respect
//! `.gitignore` and skip hidden files, like ripgrep.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{
    parse_args, str_arg, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, MAX_OUTPUT_CHARS,
};

const MAX_GLOB_RESULTS: usize = 200;
const MAX_GREP_RESULTS: usize = 250;
const MAX_MATCH_LINE_CHARS: usize = 300;

fn walker(root: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
}

fn search_root(ctx: &ToolContext, path: Option<&str>) -> PathBuf {
    match path.filter(|p| !p.is_empty()) {
        Some(p) => ctx.resolve(p),
        None => ctx.cwd.clone(),
    }
}

pub struct Glob;

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &'static str {
        "glob"
    }

    fn label(&self) -> &'static str {
        "Glob"
    }

    fn description(&self) -> &'static str {
        "Find files by glob pattern, e.g. `**/*.rs` or `src/**/mod.rs`. Patterns match \
         paths relative to the search directory; `*` does not cross `/`, `**` does. \
         Results are sorted by modification time, newest first."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern." },
                "path": { "type": "string", "description": "Directory to search. Defaults to the working directory." }
            },
            "required": ["pattern"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn subject(&self, args: &Value, _ctx: &ToolContext) -> String {
        str_arg(args, "pattern").to_string()
    }

    async fn run(&self, args: Value, ctx: &ToolContext, _cancel: CancellationToken) -> ToolOutput {
        let args: GlobArgs = match parse_args(args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let matcher = match globset::GlobBuilder::new(&args.pattern)
            .literal_separator(true)
            .build()
        {
            Ok(g) => g.compile_matcher(),
            Err(e) => return ToolOutput::error(format!("invalid glob: {e}")),
        };
        let root = search_root(ctx, args.path.as_deref());
        let display_ctx = ctx.clone();
        let found = tokio::task::spawn_blocking(move || {
            let mut found: Vec<(SystemTime, PathBuf)> = walker(&root)
                .flatten()
                .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                .filter(|e| {
                    e.path()
                        .strip_prefix(&root)
                        .is_ok_and(|rel| matcher.is_match(rel))
                })
                .map(|e| {
                    let modified = e
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    (modified, e.into_path())
                })
                .collect();
            found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            found
        })
        .await
        .unwrap_or_default();

        let total = found.len();
        if total == 0 {
            return ToolOutput::ok("No files matched.", "No matches");
        }
        let mut lines: Vec<String> = found
            .iter()
            .take(MAX_GLOB_RESULTS)
            .map(|(_, p)| display_ctx.display(p))
            .collect();
        let shown = lines.clone();
        if total > MAX_GLOB_RESULTS {
            lines.push(format!(
                "… {} more; narrow the pattern",
                total - MAX_GLOB_RESULTS
            ));
        }
        let plural = if total == 1 { "" } else { "s" };
        ToolOutput::ok(lines.join("\n"), format!("Found {total} file{plural}"))
            .with_display(ToolDisplay::Lines(shown))
    }
}

pub struct Grep;

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
    #[serde(default)]
    output_mode: OutputMode,
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OutputMode {
    #[default]
    Content,
    FilesWithMatches,
    Count,
}

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn label(&self) -> &'static str {
        "Grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents with a regular expression (Rust regex syntax). \
         `output_mode` is `content` (matching lines as path:line:text, the default), \
         `files_with_matches`, or `count`. Filter files with `glob`, e.g. `*.rs`."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression." },
                "path": { "type": "string", "description": "File or directory to search. Defaults to the working directory." },
                "glob": { "type": "string", "description": "Only search files whose name (or relative path, if it contains /) matches." },
                "case_insensitive": { "type": "boolean" },
                "output_mode": { "type": "string", "enum": ["content", "files_with_matches", "count"] }
            },
            "required": ["pattern"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn subject(&self, args: &Value, _ctx: &ToolContext) -> String {
        str_arg(args, "pattern").to_string()
    }

    async fn run(&self, args: Value, ctx: &ToolContext, cancel: CancellationToken) -> ToolOutput {
        let args: GrepArgs = match parse_args(args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let regex = match regex::RegexBuilder::new(&args.pattern)
            .case_insensitive(args.case_insensitive)
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(format!("invalid regex: {e}")),
        };
        let filter = match args.glob.as_deref().filter(|g| !g.is_empty()) {
            Some(g) => match globset::Glob::new(g) {
                Ok(g) => Some((g.compile_matcher(), g.glob().contains('/'))),
                Err(e) => return ToolOutput::error(format!("invalid glob: {e}")),
            },
            None => None,
        };
        let root = search_root(ctx, args.path.as_deref());
        let mode = args.output_mode;
        let ctx2 = ctx.clone();

        let results = tokio::task::spawn_blocking(move || {
            let mut results: Vec<String> = Vec::new();
            let mut files = 0usize;
            let mut total = 0usize;
            for entry in walker(&root).flatten() {
                if cancel.is_cancelled() {
                    break;
                }
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let path = entry.path();
                if let Some((matcher, by_path)) = &filter {
                    let subject = if *by_path {
                        path.strip_prefix(&root).unwrap_or(path)
                    } else {
                        Path::new(entry.file_name())
                    };
                    if !matcher.is_match(subject) {
                        continue;
                    }
                }
                let Ok(bytes) = std::fs::read(path) else {
                    continue;
                };
                if bytes.iter().take(8192).any(|b| *b == 0) {
                    continue;
                }
                let text = String::from_utf8_lossy(&bytes);
                let shown = ctx2.display(path);
                let mut in_file = 0;
                for (i, line) in text.lines().enumerate() {
                    if !regex.is_match(line) {
                        continue;
                    }
                    in_file += 1;
                    if mode == OutputMode::Content && results.len() < MAX_GREP_RESULTS {
                        let line: String =
                            line.trim_end().chars().take(MAX_MATCH_LINE_CHARS).collect();
                        results.push(format!("{shown}:{}:{line}", i + 1));
                    }
                }
                if in_file > 0 {
                    files += 1;
                    total += in_file;
                    match mode {
                        OutputMode::FilesWithMatches if results.len() < MAX_GREP_RESULTS => {
                            results.push(shown)
                        }
                        OutputMode::Count if results.len() < MAX_GREP_RESULTS => {
                            results.push(format!("{shown}:{in_file}"))
                        }
                        _ => {}
                    }
                }
            }
            (results, files, total)
        })
        .await
        .unwrap_or_default();

        let (lines, files, total) = results;
        if total == 0 {
            return ToolOutput::ok("No matches found.", "No matches");
        }
        let mut content = lines.join("\n");
        if lines.len() >= MAX_GREP_RESULTS {
            content.push_str("\n… results truncated; narrow the pattern or path");
        }
        let content = super::truncate_middle(&content, MAX_OUTPUT_CHARS);
        let brief = format!(
            "{total} match{} in {files} file{}",
            if total == 1 { "" } else { "es" },
            if files == 1 { "" } else { "s" }
        );
        ToolOutput::ok(content, brief).with_display(ToolDisplay::Lines(lines))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{ctx, run};
    use super::*;

    fn tree() -> (tempfile::TempDir, ToolContext) {
        let (dir, ctx) = ctx();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"Hello\");\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("src/nested/lib.rs"), "pub fn hello() {}\n").unwrap();
        std::fs::write(root.join("README.md"), "hello world\n").unwrap();
        std::fs::write(root.join("target/out.rs"), "fn hello() {}\n").unwrap();
        (dir, ctx)
    }

    #[tokio::test]
    async fn glob_respects_separators_and_gitignore() {
        let (_dir, ctx) = tree();
        let out = run(&Glob, &ctx, json!({ "pattern": "**/*.rs" })).await;
        let mut found: Vec<_> = out.content.lines().collect();
        found.sort();
        assert_eq!(found, ["src/main.rs", "src/nested/lib.rs"]);

        let out = run(&Glob, &ctx, json!({ "pattern": "src/*.rs" })).await;
        assert_eq!(out.content, "src/main.rs");
        assert_eq!(
            run(&Glob, &ctx, json!({ "pattern": "*.py" })).await.brief,
            "No matches"
        );
    }

    #[tokio::test]
    async fn grep_modes() {
        let (_dir, ctx) = tree();
        let out = run(
            &Grep,
            &ctx,
            json!({ "pattern": "hello", "case_insensitive": true, "glob": "*.rs" }),
        )
        .await;
        let mut lines: Vec<_> = out.content.lines().collect();
        lines.sort();
        assert_eq!(
            lines,
            [
                "src/main.rs:2:    println!(\"Hello\");",
                "src/nested/lib.rs:1:pub fn hello() {}"
            ]
        );
        assert_eq!(out.brief, "2 matches in 2 files");

        let out = run(
            &Grep,
            &ctx,
            json!({ "pattern": "hello", "output_mode": "files_with_matches" }),
        )
        .await;
        let mut files: Vec<_> = out.content.lines().collect();
        files.sort();
        assert_eq!(files, ["README.md", "src/nested/lib.rs"]);

        let out = run(
            &Grep,
            &ctx,
            json!({ "pattern": "fn", "output_mode": "count", "path": "src/main.rs" }),
        )
        .await;
        assert!(out.content.ends_with(":1"), "{}", out.content);
        assert!(run(&Grep, &ctx, json!({ "pattern": "(" })).await.is_error);
    }
}
