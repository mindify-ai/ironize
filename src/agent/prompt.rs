//! The system prompt, including project instructions from the workspace.

use std::path::Path;

use crate::tools::ToolRegistry;

/// Instruction files read from the working directory, in priority order.
const INSTRUCTION_FILES: &[&str] = &["IRONIZE.md", "AGENTS.md", "CLAUDE.md"];
const MAX_INSTRUCTIONS_CHARS: usize = 20_000;

pub fn system_prompt(cwd: &Path, tools: &ToolRegistry) -> String {
    let tool_list: Vec<String> = tools
        .iter()
        .map(|t| format!("- `{}`: {}", t.name(), t.description()))
        .collect();
    let is_git = cwd.ancestors().any(|dir| dir.join(".git").exists());
    let mut prompt = format!(
        "You are Ironize, an AI coding agent by Mindify AI, running in the user's terminal \
and powered by Mindify Cortex. You help with software engineering: understanding code, \
fixing bugs, adding features, refactoring, and running commands.

# Environment
- Working directory: {cwd}
- Git repository: {git}
- Platform: {os} ({arch})
- Today's date: {date}

# How to work
- Use your tools to inspect the workspace rather than guessing. Search with `glob` and \
`grep`, read with `read_file`, and read a file before editing it.
- Prefer `edit_file` for changes to existing files; use `write_file` for new files or \
complete rewrites. Match the surrounding code's style and conventions.
- For tasks with three or more steps, track them with `todo_write` and keep it current.
- After changing code, verify it when you can (build, tests, linters) with `bash`.
- Never run destructive commands (deleting data, force-pushing, rewriting history) \
unless the user explicitly asked for exactly that.
- If the user declines a tool call, stop and ask how they would like to proceed.
- Be concise. Lead with the answer or result. Use Markdown; refer to code as \
`path:line`. Do not narrate each tool call; the user sees them.

# Tools
{tools}
",
        cwd = cwd.display(),
        git = if is_git { "yes" } else { "no" },
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        date = chrono::Local::now().format("%Y-%m-%d"),
        tools = tool_list.join("\n"),
    );
    for name in INSTRUCTION_FILES {
        if let Ok(text) = std::fs::read_to_string(cwd.join(name)) {
            let text: String = text.chars().take(MAX_INSTRUCTIONS_CHARS).collect();
            prompt.push_str(&format!(
                "\n# Project instructions ({name})\nThe user's project provides these instructions. Follow them.\n\n{}\n",
                text.trim()
            ));
            break;
        }
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_environment_tools_and_project_instructions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Always run `make check`.").unwrap();
        let prompt = system_prompt(dir.path(), &ToolRegistry::builtin());
        assert!(prompt.contains(&dir.path().display().to_string()));
        assert!(prompt.contains("`edit_file`"));
        assert!(prompt.contains("Project instructions (AGENTS.md)"));
        assert!(prompt.contains("Always run `make check`."));
    }
}
