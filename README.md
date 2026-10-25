# Iron

An open-source, high-performance AI agent harness written in Rust, with
technologies from Mindify AI.

Iron signs in to [Mindify Cortex](https://cortex.mindifyai.dev) and runs models
through the Cortex gateway, so your Cortex catalog, plan, workspace policy and
usage limits apply without a separate provider key.

## Install

```sh
cargo install --git https://github.com/mindify-ai/Iron
```

## Quick start

```sh
iron login                      # opens your browser to sign in to Cortex
iron status                     # verifies the session with Cortex
iron models                     # gateway models available to you
iron ask -m <model-id> "Summarize RFC 8252 in two sentences"
iron logout                     # revokes the session and deletes it locally
```

## Signing in

| Method | Command | When to use it |
| --- | --- | --- |
| Browser (default) | `iron login` | Interactive machines. Works with every sign-in method Cortex offers (email, Google, GitHub, …). |
| Browser, URL only | `iron login --no-browser` | SSH sessions where you can open the link on another device that can reach this machine's loopback port. |
| Email + password | `iron login --email you@example.com` | Headless machines. Prompts for the password, or reads `IRON_PASSWORD`. |
| Existing token | `echo "$TOKEN" \| iron login --token-stdin` | Reusing a session issued elsewhere. |
| Environment | `CORTEX_TOKEN=… iron status` | CI. Nothing is written to disk. |

Use `--base-url` (or `CORTEX_BASE_URL`) to target another Cortex deployment,
such as `http://localhost:3000` during development.

### How the browser login works

Iron uses the same handoff as the TypeScript Cortex CLI, so it needs no
server-side changes:

1. Iron binds a one-shot listener on `127.0.0.1:<random port>` and generates a
   128-bit random `state`.
2. It opens `<cortex>/api/auth/cli/callback?redirect_uri=http://127.0.0.1:<port>/callback?state=…`.
   Cortex accepts only loopback `/callback` URIs that carry a `state`.
3. After you sign in, Cortex redirects to the listener with the Better Auth
   session token.
4. Iron checks `state` (constant time), rejects a token reported for a
   different deployment, and verifies the token with
   `GET /api/auth/get-session` before saving anything.

### Security notes

- The session is stored at `~/.config/iron/session.json` (override the
  directory with `IRON_CONFIG_DIR`), mode `0600` in a `0700` directory, and
  written atomically.
- Tokens are sent only over HTTPS; plain `http` is accepted only for
  `localhost`/loopback deployments.
- HTTP redirects are never followed, so a bearer token is never replayed to
  another host.
- Model calls never use the long-lived session. `iron ask` registers a gateway
  launch (`POST /api/gateway/launches`), which returns a 30-minute token scoped
  to your user, workspace, harness and model. Iron heartbeats it while the
  model runs and revokes it when the call finishes.
- Tokens are redacted from all `Debug` output.

## Using Iron as a library

```rust,no_run
use iron::auth::{self, BrowserLogin};
use iron::cortex::{ChatMessage, CortexClient, Harness};
use iron::SessionStore;

# async fn demo() -> iron::Result<()> {
let store = SessionStore::default_location()?;
let session = match store.load()? {
    Some(session) => session,
    None => {
        let session = auth::browser_login(BrowserLogin::new(iron::DEFAULT_BASE_URL), |p| {
            println!("Sign in at {}", p.authorize_url);
        })
        .await?;
        store.save(&session)?;
        session
    }
};

let cortex = CortexClient::new(session)?;
let ticket = cortex.create_launch("personal", Harness::Opencode, "gpt-5.6-sol").await?;
let answer = cortex.complete(&ticket, &[ChatMessage::user("Hello")]).await?;
cortex.finish_launch(&ticket.launch_id, 0).await?;
# Ok(()) }
```

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The integration tests in `tests/` run every login flow against a mock Cortex
server; no network access or Cortex account is needed.

## License

Apache-2.0
