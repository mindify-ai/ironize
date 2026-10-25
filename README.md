# Ironize

An open-source, high-performance AI agent harness for your terminal, written in
Rust by Mindify AI and powered by [Mindify Cortex](https://cortex.mindifyai.dev).

Ironize runs models through the Cortex gateway, so your Cortex catalog, plan,
workspace policy and usage limits apply without a separate provider key.

```
╭──────────────────────────────────────────────────────────────────────────╮
│ ✻ Welcome to Ironize  v0.2.0                                             │
│   The agent harness by Mindify AI, powered by Cortex                     │
╰──────────────────────────────────────────────────────────────────────────╯

› What is in this project?

● Let me look at the project first.

● Bash(ls -1)
  ⎿ exit code 0 · 0.0s
    Cargo.toml
    src

● The project has 2 entries: the Cargo.toml manifest and the src directory.

╭──────────────────────────────────────────────────────────────────────────╮
│ › Ask Ironize to build, fix or explain…  (/ for commands)                │
╰──────────────────────────────────────────────────────────────────────────╯
  ⏵⏵ accept edits on (shift+tab to cycle)        gpt-5.6-sol · personal · ↑3.0k ↓174
```

## Install

```sh
cargo install --git https://github.com/mindify-ai/Iron
```

## Quick start

```sh
ironize                         # sign in (first run) and open the terminal UI
ironize "fix the failing test"  # open the UI with a first prompt
ironize run --yes "add a CHANGELOG entry for the last commit"   # headless
```

## The terminal UI

- **Streaming answers** rendered as Markdown: headings, lists, code blocks, quotes, tables.
- **Tool cards** for every tool call, with live spinners, results, command output and
  colored diffs. Press `ctrl+o` to expand full output.
- **Permission prompts** before changes, with a preview of the command or diff. Choose
  *Yes*, *Yes, don't ask again for this tool*, or *No, and tell Ironize what to do instead*.
- **Permission modes**, cycled with `shift+tab`: *ask before changes* → *accept edits* → *yolo*.
- **Live task list** while the agent works through multi-step tasks.
- **Slash commands** with an inline palette (`/` then `tab` to complete).
- **Model picker** (`/model`) over your workspace's gateway catalog, with type-to-filter.
- **Queued follow-ups**: type while Ironize works; the message is sent when the turn ends.
- Prompt **history**, multi-line input, mouse-wheel scrolling and token usage in the footer.

| Command | What it does |
| --- | --- |
| `/help` | Commands and keyboard shortcuts |
| `/model [id]` | Switch the model (remembered for next time) |
| `/mode [default\|accept-edits\|yolo]` | Change when Ironize asks before acting |
| `/clear` | Start a new conversation |
| `/status` | Account, model, workspace, mode and token usage |
| `/todos` | Show the current task list |
| `/expand` | Toggle full tool output |
| `/exit` | Quit |

| Key | Action |
| --- | --- |
| `enter` | Send (or queue while Ironize works) |
| `shift+enter`, `ctrl+j`, `\` + `enter` | New line |
| `esc` | Interrupt the current turn |
| `shift+tab` | Cycle permission mode |
| `↑` / `↓` | Prompt history |
| `pgup` / `pgdn`, mouse wheel | Scroll (hold `shift` to select text with the mouse) |
| `ctrl+o` | Expand tool output |
| `ctrl+c` twice, `ctrl+d` | Quit |

## Tools

| Tool | Kind | What it does |
| --- | --- | --- |
| `read_file` | read | Read a file with line numbers, with offset/limit paging |
| `write_file` | edit | Create or overwrite a file |
| `edit_file` | edit | Replace an exact, unique string (or every occurrence) |
| `ls` | read | List a directory |
| `glob` | read | Find files by pattern, respecting `.gitignore` |
| `grep` | read | Regex search across files, respecting `.gitignore` |
| `bash` | execute | Run a shell command with a timeout; interruptible |
| `web_fetch` | network | Fetch a URL; HTML is converted to text |
| `todo_write` | read | Maintain the live task list |

Read tools never ask. Edit, execute and network tools ask in the default mode;
*accept edits* stops asking for file changes; *yolo* never asks.

Ironize reads project instructions from `IRONIZE.md`, `AGENTS.md` or `CLAUDE.md`
in the working directory and follows them.

## Signing in

| Method | Command | When to use it |
| --- | --- | --- |
| Browser (default) | `ironize login`, or just run `ironize` | Interactive machines. Works with every sign-in method Cortex offers. |
| Browser, URL only | `ironize login --no-browser` | When you'd rather open the link yourself. |
| Email + password | `ironize login --email you@example.com` | Headless machines. Prompts for the password, or reads `IRONIZE_PASSWORD`. |
| Existing token | `echo "$TOKEN" \| ironize login --token-stdin` | Reusing a session issued elsewhere. |
| Environment | `CORTEX_TOKEN=… ironize run …` | CI. Nothing is written to disk. |

`ironize status` verifies the session, `ironize logout` revokes it on Cortex and
deletes it locally, and `ironize models` lists the models available to you. Use
`--base-url` (or `CORTEX_BASE_URL`) to target another Cortex deployment.

### How the browser login works

Ironize uses the same handoff as the TypeScript Cortex CLI, so it needs no
server-side changes:

1. Ironize binds a one-shot listener on `127.0.0.1:<random port>` and generates a
   128-bit random `state`.
2. It opens `<cortex>/api/auth/cli/callback?redirect_uri=http://127.0.0.1:<port>/callback?state=…`.
3. After you sign in, Cortex redirects to the listener with the session token.
4. Ironize checks `state` (constant time), rejects a token reported for a
   different deployment, and verifies it with `GET /api/auth/get-session`
   before saving anything.

### Security notes

- The session lives at `~/.config/ironize/session.json` (override the directory
  with `IRONIZE_CONFIG_DIR`), mode `0600`, written atomically. Preferences
  (`config.json`) and prompt history (`history.json`) sit next to it.
- Tokens are sent only over HTTPS; plain `http` is accepted only for loopback.
  HTTP redirects are never followed with a token attached.
- Model calls never use the long-lived session. Ironize registers a gateway
  launch, which returns a 30-minute token scoped to your user, workspace and
  model; it is heartbeated while in use, renewed if it lapses, and revoked on exit.
- Tokens are redacted from all `Debug` output.

## Using Ironize as a library

```rust,no_run
use ironize::agent::{Agent, PermissionMode, SharedMode};
use ironize::cortex::{CortexClient, Harness};
use ironize::gateway::Gateway;
use ironize::tools::ToolContext;
use ironize::SessionStore;

# async fn demo() -> anyhow::Result<()> {
let session = SessionStore::default_location()?.load()?.expect("run `ironize login`");
let gateway = Gateway::new(CortexClient::new(session)?, "personal", Harness::Opencode, "gpt-5.6-sol");
let mut agent = Agent::new(gateway, ToolContext::new(std::env::current_dir()?), SharedMode::new(PermissionMode::Yolo));

let (events, mut rx) = tokio::sync::mpsc::unbounded_channel();
tokio::spawn(async move { while let Some(event) = rx.recv().await { println!("{event:?}") } });
let outcome = agent.run_turn("Summarize this repo".into(), &events, &Default::default()).await;
agent.gateway_mut().close().await;
# Ok(()) }
```

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The tests cover login flows, the SSE stream decoder, every tool, the agent loop
(against a mock Cortex gateway), and the TUI (state machine and rendered
screens). No network access or Cortex account is needed.

## License

Apache-2.0
