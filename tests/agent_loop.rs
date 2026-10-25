//! The agent loop end to end: Cortex gateway (mocked) → tool calls → results.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ironize::agent::{Agent, AgentEvent, Decision, PermissionMode, SharedMode, TurnOutcome};
use ironize::cortex::{CortexClient, Harness, Message};
use ironize::gateway::Gateway;
use ironize::tools::ToolContext;
use ironize::Session;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const TOKEN: &str = "session-token";

fn sse(chunks: &[Value]) -> ResponseTemplate {
    let mut body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    body.push_str("data: [DONE]\n\n");
    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

fn text_reply(text: &str) -> ResponseTemplate {
    sse(&[
        json!({ "choices": [{ "index": 0, "delta": { "content": text }, "finish_reason": "stop" }] }),
        json!({ "choices": [], "usage": { "prompt_tokens": 100, "completion_tokens": 10 } }),
    ])
}

fn tool_reply(id: &str, name: &str, args: Value) -> ResponseTemplate {
    let args = args.to_string();
    let (a, b) = args.split_at(args.len() / 2);
    sse(&[
        json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [{ "index": 0, "id": id, "type": "function", "function": { "name": name, "arguments": a } }] } }] }),
        json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [{ "index": 0, "function": { "arguments": b } }] }, "finish_reason": "tool_calls" }] }),
    ])
}

/// Replies from a script, one per request, and records each request body.
struct Script {
    replies: Vec<ResponseTemplate>,
    next: AtomicUsize,
    bodies: Arc<Mutex<Vec<Value>>>,
}

impl Respond for Script {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&request.body).unwrap());
        let i = self.next.fetch_add(1, Ordering::SeqCst);
        self.replies
            .get(i)
            .cloned()
            .unwrap_or_else(|| text_reply("(script exhausted)"))
    }
}

struct Fixture {
    server: MockServer,
    bodies: Arc<Mutex<Vec<Value>>>,
    dir: tempfile::TempDir,
}

async fn setup(replies: Vec<ResponseTemplate>) -> Fixture {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("POST"))
        .and(path("/api/gateway/launches"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "launchId": "launch-1", "token": "cgw_scoped", "expiresAt": "2026-10-25T03:00:00.000Z",
            "baseUrl": format!("{base}/api"), "model": "gpt-5.6-sol", "workspace": "personal"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/gateway/launches/launch-1/finish"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;
    let bodies = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(header("authorization", "Bearer cgw_scoped"))
        .respond_with(Script {
            replies,
            next: AtomicUsize::new(0),
            bodies: bodies.clone(),
        })
        .mount(&server)
        .await;
    Fixture {
        server,
        bodies,
        dir: tempfile::tempdir().unwrap(),
    }
}

fn agent(h: &Fixture, mode: PermissionMode) -> Agent {
    let client = CortexClient::new(Session::new(TOKEN, &h.server.uri()).unwrap()).unwrap();
    let gateway = Gateway::new(client, "personal", Harness::Opencode, "gpt-5.6-sol");
    Agent::new(
        gateway,
        ToolContext::new(h.dir.path().to_path_buf()),
        SharedMode::new(mode),
    )
}

/// Runs a turn, answering permission prompts with `decision`.
async fn run(
    agent: &mut Agent,
    prompt: &str,
    decision: Decision,
) -> (TurnOutcome, Vec<AgentEvent>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let collector = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            if let AgentEvent::Permission(request) = event {
                request.respond(decision);
                continue;
            }
            events.push(event);
        }
        events
    });
    let outcome = agent.run_turn(prompt.into(), &tx, &cancel).await;
    drop(tx);
    (outcome, collector.await.unwrap())
}

#[tokio::test]
async fn runs_tools_and_feeds_results_back() {
    let h = setup(vec![
        tool_reply(
            "call_1",
            "write_file",
            json!({ "path": "hello.txt", "content": "hi there\n" }),
        ),
        tool_reply("call_2", "read_file", json!({ "path": "hello.txt" })),
        text_reply("Created **hello.txt**."),
    ])
    .await;
    let mut agent = agent(&h, PermissionMode::AcceptEdits);
    let (outcome, events) = run(&mut agent, "make a file", Decision::Deny).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(h.dir.path().join("hello.txt")).unwrap(),
        "hi there\n"
    );

    let labels: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStarted { label, subject, .. } => Some(format!("{label}({subject})")),
            _ => None,
        })
        .collect();
    assert_eq!(labels, ["Write(hello.txt)", "Read(hello.txt)"]);
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::Usage(u) if u.input_tokens == 100)));

    // The third request carries the whole exchange in OpenAI format.
    let bodies = h.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    let last = &bodies[2];
    assert_eq!(last["model"], "gpt-5.6-sol");
    assert_eq!(last["stream"], true);
    assert!(last["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["function"]["name"] == "bash"));
    let messages = last["messages"].as_array().unwrap();
    let roles: Vec<_> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(
        roles,
        ["system", "user", "assistant", "tool", "assistant", "tool"]
    );
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["name"],
        "write_file"
    );
    assert_eq!(messages[5]["tool_call_id"], "call_2");
    assert!(messages[5]["content"]
        .as_str()
        .unwrap()
        .contains("1\thi there"));

    // The reply is kept for the next turn.
    assert!(
        matches!(agent.messages().last(), Some(Message::Assistant { content: Some(c), .. }) if c == "Created **hello.txt**.")
    );
}

#[tokio::test]
async fn declined_tool_stops_the_turn() {
    let h = setup(vec![
        tool_reply(
            "call_1",
            "bash",
            json!({ "command": "touch should-not-exist" }),
        ),
        text_reply("never requested"),
    ])
    .await;
    let mut agent = agent(&h, PermissionMode::Default);
    let (outcome, events) = run(&mut agent, "run it", Decision::Deny).await;
    assert_eq!(outcome, TurnOutcome::Denied);
    assert!(!h.dir.path().join("should-not-exist").exists());
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolFinished { declined: true, .. })));
    assert_eq!(
        h.bodies.lock().unwrap().len(),
        1,
        "no follow-up request after a denial"
    );
    // The conversation stays valid: the tool call has a result.
    assert!(
        matches!(agent.messages().last(), Some(Message::Tool { tool_call_id, .. }) if tool_call_id == "call_1")
    );
}

#[tokio::test]
async fn approved_and_always_allowed_tools_run() {
    let h = setup(vec![
        tool_reply("c1", "bash", json!({ "command": "echo one > a.txt" })),
        tool_reply("c2", "bash", json!({ "command": "echo two > b.txt" })),
        text_reply("done"),
    ])
    .await;
    let mut agent = agent(&h, PermissionMode::Default);
    let (outcome, events) = run(&mut agent, "go", Decision::AllowAlways).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    assert!(h.dir.path().join("a.txt").exists() && h.dir.path().join("b.txt").exists());
    // Only one prompt: the second call was covered by "always".
    let started = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolStarted { .. }))
        .count();
    assert_eq!(started, 2);
}

#[tokio::test]
async fn bad_tool_calls_are_reported_to_the_model() {
    let h = setup(vec![
        sse(&[json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [
            { "index": 0, "id": "c1", "type": "function", "function": { "name": "teleport", "arguments": "{}" } },
            { "index": 1, "id": "c2", "type": "function", "function": { "name": "read_file", "arguments": "{not json" } }
        ] }, "finish_reason": "tool_calls" }] })]),
        text_reply("sorry"),
    ])
    .await;
    let mut agent = agent(&h, PermissionMode::Default);
    let (outcome, _) = run(&mut agent, "go", Decision::Deny).await;
    assert_eq!(outcome, TurnOutcome::Completed);
    let bodies = h.bodies.lock().unwrap();
    let messages = bodies[1]["messages"].as_array().unwrap();
    let results: Vec<_> = messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert!(results[0].contains("unknown tool \"teleport\""));
    assert!(results[1].contains("not valid JSON"));
}

#[tokio::test]
async fn gateway_errors_fail_the_turn_cleanly() {
    let server = MockServer::start().await;
    Mock::given(path("/api/gateway/launches"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({ "error": "Model is incompatible with this harness." })),
        )
        .mount(&server)
        .await;
    let h = Fixture {
        server,
        bodies: Default::default(),
        dir: tempfile::tempdir().unwrap(),
    };
    let mut agent = agent(&h, PermissionMode::Default);
    let (outcome, _) = run(&mut agent, "hi", Decision::Deny).await;
    assert!(
        matches!(outcome, TurnOutcome::Failed(ref e) if e.contains("incompatible")),
        "{outcome:?}"
    );
}
