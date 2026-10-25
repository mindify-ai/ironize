//! HTTP client for the Mindify Cortex platform.

mod types;

pub use types::*;

use reqwest::{header, Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use url::Url;

use crate::error::{Error, Result};
use crate::session::{normalize_base_url, Session};

const USER_AGENT: &str = concat!("iron/", env!("CARGO_PKG_VERSION"));
/// Better Auth's session cookie. Most routes read the bearer header through the
/// `bearer()` plugin; the cookie mirrors the TypeScript CLI for older routes.
const SESSION_COOKIE: &str = "better-auth.session_token";

/// An authenticated Cortex client bound to one session.
#[derive(Debug, Clone)]
pub struct CortexClient {
    http: reqwest::Client,
    base_url: Url,
    session: Session,
}

impl CortexClient {
    pub fn new(session: Session) -> Result<Self> {
        Ok(Self {
            http: http_client()?,
            base_url: normalize_base_url(&session.base_url)?,
            session,
        })
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Asks Cortex who this token belongs to. Fails with
    /// [`Error::Unauthenticated`] when the session is unknown or expired.
    pub async fn verify(&self) -> Result<AuthSession> {
        let response = self
            .send(
                self.request(Method::GET, "/api/auth/get-session")?,
                "verify the session",
            )
            .await?;
        let session: Option<AuthSession> = read_json(response, "verify the session").await?;
        session.ok_or_else(|| {
            Error::Unauthenticated("the session is expired or was revoked; run `iron login`".into())
        })
    }

    /// Revokes the session on the server (`POST /api/auth/sign-out`).
    pub async fn sign_out(&self) -> Result<()> {
        let request = self
            .request(Method::POST, "/api/auth/sign-out")?
            .header(header::ORIGIN, self.origin())
            .json(&serde_json::json!({}));
        self.send(request, "sign out").await.map(drop)
    }

    /// The workspace gateway catalog, filtered to models usable by `harness`.
    pub async fn models(&self, workspace: &str, harness: Harness) -> Result<Vec<CatalogEntry>> {
        let request = self
            .request(Method::GET, "/api/gateway")?
            .query(&[("workspace", workspace)]);
        let response = self.send(request, "list gateway models").await?;
        let overview: GatewayOverview = read_json(response, "list gateway models").await?;
        Ok(overview
            .catalog
            .into_iter()
            .filter(|m| m.enabled && m.supports(harness))
            .collect())
    }

    /// Registers a gateway launch and returns its scoped, short-lived token.
    pub async fn create_launch(
        &self,
        workspace: &str,
        harness: Harness,
        model: &str,
    ) -> Result<LaunchTicket> {
        let request =
            self.request(Method::POST, "/api/gateway/launches")?
                .json(&serde_json::json!({
                    "workspace": workspace,
                    "harness": harness,
                    "model": model,
                    "target": "cloud",
                }));
        let response = self.send(request, "create a gateway launch").await?;
        read_json(response, "create a gateway launch").await
    }

    /// Keeps a launch token alive while the agent runs.
    pub async fn heartbeat_launch(&self, launch_id: &str) -> Result<()> {
        let path = format!(
            "/api/gateway/launches/{}/heartbeat",
            path_segment(launch_id)
        );
        self.send(
            self.request(Method::POST, &path)?,
            "heartbeat a gateway launch",
        )
        .await
        .map(drop)
    }

    /// Revokes a launch token and records the agent's exit code.
    pub async fn finish_launch(&self, launch_id: &str, exit_code: i32) -> Result<()> {
        let path = format!("/api/gateway/launches/{}/finish", path_segment(launch_id));
        let request = self
            .request(Method::POST, &path)?
            .json(&serde_json::json!({ "exitCode": exit_code }));
        self.send(request, "finish a gateway launch")
            .await
            .map(drop)
    }

    /// One non-streaming OpenAI Chat Completions turn through the gateway,
    /// authenticated with the launch token rather than the session.
    pub async fn complete(
        &self,
        ticket: &LaunchTicket,
        messages: &[ChatMessage],
    ) -> Result<String> {
        let url = format!(
            "{}/v1/chat/completions",
            ticket.base_url.trim_end_matches('/')
        );
        let request = self
            .http
            .post(url)
            .bearer_auth(&ticket.token)
            .json(&serde_json::json!({
                "model": ticket.model,
                "messages": messages,
                "stream": false,
            }));
        let response = self.send(request, "call the model gateway").await?;
        let completion: ChatCompletion = read_json(response, "call the model gateway").await?;
        Ok(completion
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default())
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let url = self
            .base_url
            .join(path)
            .map_err(|e| Error::InvalidBaseUrl {
                url: self.base_url.to_string(),
                reason: e.to_string(),
            })?;
        Ok(self
            .http
            .request(method, url)
            .bearer_auth(&self.session.token)
            .header(
                header::COOKIE,
                format!("{SESSION_COOKIE}={}", self.session.token),
            ))
    }

    fn origin(&self) -> String {
        self.base_url.origin().ascii_serialization()
    }

    async fn send(&self, request: RequestBuilder, operation: &str) -> Result<Response> {
        check_status(request.send().await?, operation).await
    }
}

/// Builds the shared HTTP client. Redirects are disabled so a bearer token is
/// never replayed to a different host, and so an auth redirect to `/sign-in`
/// surfaces as an error instead of an HTML page.
pub(crate) fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(300))
        .build()?)
}

pub(crate) async fn check_status(response: Response, operation: &str) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    if status.is_redirection() {
        return Err(Error::Unauthenticated(format!(
            "Cortex redirected instead of answering while trying to {operation}; run `iron login`"
        )));
    }
    let content_type = content_type(&response);
    let body = response.text().await.unwrap_or_default();
    let message = error_message(&body, &content_type).unwrap_or_else(|| {
        status
            .canonical_reason()
            .unwrap_or("request failed")
            .to_string()
    });
    if status == StatusCode::UNAUTHORIZED {
        return Err(Error::Unauthenticated(message));
    }
    Err(Error::Api {
        status: status.as_u16(),
        message,
    })
}

pub(crate) async fn read_json<T: DeserializeOwned>(
    response: Response,
    operation: &str,
) -> Result<T> {
    let body = response.text().await?;
    serde_json::from_str(&body).map_err(|_| Error::UnexpectedResponse {
        operation: operation.into(),
    })
}

fn content_type(response: &Response) -> String {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Pulls a human-readable message out of a Cortex, Better Auth, or OpenAI-style
/// error body, without ever echoing an HTML page.
fn error_message(body: &str, content_type: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        let candidates = [
            value.get("message"),
            value.get("error"),
            value.get("error").and_then(|e| e.get("message")),
        ];
        if let Some(text) = candidates.into_iter().flatten().find_map(|v| v.as_str()) {
            return Some(text.to_string());
        }
    }
    let trimmed = body.trim();
    let looks_like_html = content_type.contains("text/html")
        || trimmed
            .get(..15)
            .is_some_and(|s| s.eq_ignore_ascii_case("<!doctype html>"))
        || trimmed.starts_with("<html");
    (!trimmed.is_empty() && !looks_like_html).then(|| trimmed.chars().take(500).collect())
}

fn path_segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_prefers_structured_fields() {
        assert_eq!(
            error_message(r#"{"error":"nope"}"#, "").as_deref(),
            Some("nope")
        );
        assert_eq!(
            error_message(r#"{"message":"bad"}"#, "").as_deref(),
            Some("bad")
        );
        assert_eq!(
            error_message(r#"{"error":{"message":"deep","type":"x"}}"#, "").as_deref(),
            Some("deep")
        );
        assert_eq!(
            error_message("plain text", "text/plain").as_deref(),
            Some("plain text")
        );
        assert_eq!(error_message("<!DOCTYPE html><p>hi</p>", ""), None);
        assert_eq!(error_message("<p>hi</p>", "text/html; charset=utf-8"), None);
    }
}
