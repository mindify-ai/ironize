//! End-to-end auth flows against a mock Cortex server.

use std::time::Duration;

use ironize::auth::{self, BrowserLogin};
use ironize::cortex::chat::StreamEvent;
use ironize::cortex::{CortexClient, Harness, Message};
use ironize::{Error, Session};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "session-token-123";

fn get_session_ok() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "session": { "token": TOKEN, "expiresAt": "2026-11-01T00:00:00.000Z", "userId": "u1" },
        "user": { "id": "u1", "name": "Ada Lovelace", "email": "ada@example.com" }
    }))
}

async fn mock_get_session(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/auth/get-session"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(get_session_ok())
        .mount(server)
        .await;
    // Better Auth answers `null` for unknown tokens.
    Mock::given(method("GET"))
        .and(path("/api/auth/get-session"))
        .respond_with(ResponseTemplate::new(200).set_body_string("null"))
        .with_priority(10)
        .mount(server)
        .await;
}

fn plain_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// Plays the browser's part: follows the authorize URL's redirect target back
/// to Ironize's loopback listener, as Cortex's `/api/auth/cli/callback` would.
fn complete_in_browser(
    authorize_url: &url::Url,
    token: &str,
    base_url: &str,
    state_override: Option<&str>,
) {
    let redirect_uri = authorize_url
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.into_owned())
        .expect("authorize URL carries redirect_uri");
    let mut callback = url::Url::parse(&redirect_uri).unwrap();
    if let Some(state) = state_override {
        let pairs: Vec<_> = callback
            .query_pairs()
            .map(|(k, v)| {
                let v = if k == "state" {
                    state.to_string()
                } else {
                    v.into_owned()
                };
                (k.into_owned(), v)
            })
            .collect();
        callback.query_pairs_mut().clear().extend_pairs(pairs);
    }
    callback
        .query_pairs_mut()
        .append_pair("token", token)
        .append_pair("baseUrl", base_url);
    tokio::spawn(async move {
        let _ = plain_client().get(callback).send().await;
    });
}

#[tokio::test]
async fn browser_login_receives_and_verifies_token() {
    let server = MockServer::start().await;
    mock_get_session(&server).await;
    let base = server.uri();

    let session = auth::browser_login(BrowserLogin::new(&base), |prompt| {
        let url = &prompt.authorize_url;
        assert_eq!(url.path(), "/api/auth/cli/callback");
        let redirect = url
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .unwrap()
            .1
            .into_owned();
        assert!(redirect.starts_with("http://127.0.0.1:"));
        assert!(redirect.contains("/callback?state="));
        // A stray request first must not abort the login.
        let stray = url::Url::parse(&redirect)
            .unwrap()
            .join("/favicon.ico")
            .unwrap();
        tokio::spawn(async move {
            let _ = plain_client().get(stray).send().await;
        });
        complete_in_browser(url, TOKEN, &base, None);
    })
    .await
    .expect("login succeeds");

    assert_eq!(session.token, TOKEN);
    assert_eq!(
        session.user.as_ref().unwrap().email.as_deref(),
        Some("ada@example.com")
    );
}

#[tokio::test]
async fn browser_login_ignores_wrong_state_and_times_out() {
    let server = MockServer::start().await;
    mock_get_session(&server).await;
    let base = server.uri();
    let mut options = BrowserLogin::new(&base);
    options.timeout = Duration::from_millis(500);

    let result = auth::browser_login(options, |prompt| {
        complete_in_browser(&prompt.authorize_url, TOKEN, &base, Some("forged"));
    })
    .await;
    assert!(matches!(result, Err(Error::Login(msg)) if msg.contains("timed out")));
}

#[tokio::test]
async fn browser_login_rejects_token_for_another_deployment() {
    let server = MockServer::start().await;
    mock_get_session(&server).await;

    let result = auth::browser_login(BrowserLogin::new(server.uri()), |prompt| {
        complete_in_browser(
            &prompt.authorize_url,
            TOKEN,
            "https://evil.example.com",
            None,
        );
    })
    .await;
    assert!(matches!(result, Err(Error::Login(msg)) if msg.contains("different deployment")));
}

#[tokio::test]
async fn browser_login_rejects_unverifiable_token() {
    let server = MockServer::start().await;
    mock_get_session(&server).await;
    let base = server.uri();

    let result = auth::browser_login(BrowserLogin::new(&base), |prompt| {
        complete_in_browser(&prompt.authorize_url, "forged-token", &base, None);
    })
    .await;
    assert!(matches!(result, Err(Error::Unauthenticated(_))));
}

#[tokio::test]
async fn password_login_uses_bearer_header_token() {
    let server = MockServer::start().await;
    mock_get_session(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/auth/sign-in/email"))
        .and(header("origin", server.uri().as_str()))
        .and(body_json(
            json!({ "email": "ada@example.com", "password": "hunter2", "rememberMe": true }),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-auth-token", TOKEN)
                .set_body_json(
                    json!({ "redirect": false, "token": "ignored", "user": { "id": "u1" } }),
                ),
        )
        .mount(&server)
        .await;

    let session = auth::password_login(&server.uri(), "ada@example.com", "hunter2")
        .await
        .unwrap();
    assert_eq!(session.token, TOKEN);
    assert_eq!(session.user.unwrap().name.as_deref(), Some("Ada Lovelace"));
}

#[tokio::test]
async fn password_login_reports_bad_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/auth/sign-in/email"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "code": "INVALID_EMAIL_OR_PASSWORD", "message": "Invalid email or password"
        })))
        .mount(&server)
        .await;

    let err = auth::password_login(&server.uri(), "ada@example.com", "wrong")
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Login(ref msg) if msg == "Invalid email or password"),
        "{err}"
    );
}

#[tokio::test]
async fn html_and_redirect_responses_are_not_mistaken_for_success() {
    let server = MockServer::start().await;
    Mock::given(path("/api/auth/get-session"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("<!DOCTYPE html><html></html>", "text/html"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/api/gateway"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", "/sign-in"))
        .mount(&server)
        .await;

    let client = CortexClient::new(Session::new(TOKEN, &server.uri()).unwrap()).unwrap();
    assert!(matches!(
        client.verify().await,
        Err(Error::UnexpectedResponse { .. })
    ));
    assert!(matches!(
        client.models("personal", Harness::Opencode).await,
        Err(Error::Unauthenticated(_))
    ));
}

#[tokio::test]
async fn gateway_launch_scopes_model_calls_to_launch_token() {
    let server = MockServer::start().await;
    let base = server.uri();

    Mock::given(method("GET"))
        .and(path("/api/gateway"))
        .and(query_param("workspace", "personal"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalog": [
                { "id": "gpt-5.6-sol", "provider": "openai", "displayName": "GPT-5.6 Sol", "enabled": true,
                  "supportsTools": true, "compatibleHarnesses": ["codex", "opencode"] },
                { "id": "claude-only", "provider": "anthropic", "displayName": "Claude", "enabled": true,
                  "supportsTools": true, "compatibleHarnesses": ["claude", "ironize-future"] },
                { "id": "disabled", "provider": "openai", "displayName": "Off", "enabled": false,
                  "supportsTools": true, "compatibleHarnesses": ["opencode"] }
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/gateway/launches"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(body_json(json!({ "workspace": "personal", "harness": "opencode", "model": "gpt-5.6-sol", "target": "cloud" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "launchId": "launch-1", "token": "cgw_scoped", "expiresAt": "2026-10-25T03:00:00.000Z",
            "baseUrl": format!("{base}/api"), "model": "gpt-5.6-sol", "workspace": "personal"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(header("authorization", "Bearer cgw_scoped"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello \"}}]}\n\n",
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"from Cortex\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            ),
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/gateway/launches/launch-1/finish"))
        .and(body_json(json!({ "exitCode": 0 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .expect(1)
        .mount(&server)
        .await;

    let client = CortexClient::new(Session::new(TOKEN, &base).unwrap()).unwrap();
    let models = client.models("personal", Harness::Opencode).await.unwrap();
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["gpt-5.6-sol"]
    );

    let ticket = client
        .create_launch("personal", Harness::Opencode, "gpt-5.6-sol")
        .await
        .unwrap();
    assert!(!format!("{ticket:?}").contains("cgw_scoped"));
    let mut stream = client
        .stream_chat(&ticket, &[Message::user("hi")], &[])
        .await
        .unwrap();
    let mut answer = String::new();
    while let Some(event) = stream.next().await {
        if let StreamEvent::Text(text) = event.unwrap() {
            answer.push_str(&text);
        }
    }
    assert_eq!(answer, "Hello from Cortex");
    client.finish_launch(&ticket.launch_id, 0).await.unwrap();
}

#[tokio::test]
async fn sign_out_revokes_session() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/auth/sign-out"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
        .expect(1)
        .mount(&server)
        .await;

    let client = CortexClient::new(Session::new(TOKEN, &server.uri()).unwrap()).unwrap();
    client.sign_out().await.unwrap();
}
