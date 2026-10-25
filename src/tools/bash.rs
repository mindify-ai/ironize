//! Running shell commands.

use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{
    parse_args, str_arg, truncate_middle, Preview, Tool, ToolContext, ToolDisplay, ToolKind,
    ToolOutput, MAX_OUTPUT_CHARS,
};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;

pub struct Bash;

#[derive(Deserialize)]
struct BashArgs {
    command: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

fn shell() -> (String, &'static str) {
    if cfg!(windows) {
        ("cmd".into(), "/C")
    } else if std::path::Path::new("/bin/bash").exists() {
        ("/bin/bash".into(), "-c")
    } else {
        ("/bin/sh".into(), "-c")
    }
}

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn label(&self) -> &'static str {
        "Bash"
    }

    fn description(&self) -> &'static str {
        "Run a shell command in the working directory and return its exit code, stdout \
         and stderr. Each call starts a fresh shell, so `cd` does not persist; chain \
         with `&&` instead. Not interactive: stdin is closed. Default timeout 120s, \
         max 600s. Use the dedicated tools to read, search and edit files."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command to run." },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS }
            },
            "required": ["command"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }

    fn subject(&self, args: &Value, _ctx: &ToolContext) -> String {
        str_arg(args, "command").to_string()
    }

    async fn preview(&self, args: &Value, _ctx: &ToolContext) -> Preview {
        Preview::Command(str_arg(args, "command").to_string())
    }

    async fn run(&self, args: Value, ctx: &ToolContext, cancel: CancellationToken) -> ToolOutput {
        let args: BashArgs = match parse_args(args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let timeout = Duration::from_secs(
            args.timeout_secs
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .clamp(1, MAX_TIMEOUT_SECS),
        );
        let (program, flag) = shell();
        let mut command = tokio::process::Command::new(program);
        command
            .arg(flag)
            .arg(&args.command)
            .current_dir(&ctx.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("IRONIZE", "1")
            .env("PAGER", "cat")
            .env("GIT_PAGER", "cat")
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);

        let started = Instant::now();
        let child = match command.spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("failed to start the shell: {e}")),
        };
        #[cfg(unix)]
        let group = child.id();
        let output = tokio::select! {
            output = child.wait_with_output() => output,
            _ = tokio::time::sleep(timeout) => {
                #[cfg(unix)]
                kill_group(group);
                return ToolOutput::error(format!("command timed out after {}s", timeout.as_secs()));
            }
            _ = cancel.cancelled() => {
                #[cfg(unix)]
                kill_group(group);
                return ToolOutput::error("command interrupted by the user");
            }
        };
        let output = match output {
            Ok(o) => o,
            Err(e) => return ToolOutput::error(format!("command failed: {e}")),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code();
        let mut content = String::new();
        if !stdout.trim().is_empty() {
            content.push_str(stdout.trim_end());
        }
        if !stderr.trim().is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("[stderr]\n");
            content.push_str(stderr.trim_end());
        }
        let status = match code {
            Some(c) => format!("exit code {c}"),
            None => "terminated by a signal".to_string(),
        };
        let lines: Vec<String> = content.lines().map(str::to_owned).collect();
        let body = if content.is_empty() {
            "(no output)".to_string()
        } else {
            truncate_middle(&content, MAX_OUTPUT_CHARS)
        };
        let elapsed = started.elapsed().as_secs_f32();
        let brief = format!("{status} · {elapsed:.1}s");
        ToolOutput {
            content: format!("{body}\n[{status}]"),
            is_error: code != Some(0),
            brief,
            display: ToolDisplay::Lines(lines),
        }
    }
}

/// Kills the command's whole process group, so background children and
/// pipelines die with it.
#[cfg(unix)]
fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{ctx, run};
    use super::*;

    #[tokio::test]
    async fn runs_in_working_directory_and_reports_exit_code() {
        let (dir, ctx) = ctx();
        std::fs::write(dir.path().join("marker.txt"), "").unwrap();
        let out = run(&Bash, &ctx, json!({ "command": "ls && echo oops >&2" })).await;
        assert!(!out.is_error);
        assert!(out.content.contains("marker.txt"));
        assert!(out.content.contains("[stderr]\noops"));
        assert!(out.content.ends_with("[exit code 0]"));

        let fail = run(&Bash, &ctx, json!({ "command": "exit 3" })).await;
        assert!(fail.is_error);
        assert!(fail.content.contains("exit code 3"));
    }

    #[tokio::test]
    async fn times_out_and_can_be_cancelled() {
        let (_dir, ctx) = ctx();
        let out = run(
            &Bash,
            &ctx,
            json!({ "command": "sleep 5", "timeout_secs": 1 }),
        )
        .await;
        assert!(out.is_error && out.content.contains("timed out"));

        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.cancel();
        });
        let started = Instant::now();
        let out = Bash
            .run(json!({ "command": "sleep 5" }), &ctx, cancel)
            .await;
        assert!(out.content.contains("interrupted"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
