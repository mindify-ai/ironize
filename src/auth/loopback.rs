//! Browser login through a one-shot loopback callback.

use std::time::Duration;

use rand::RngCore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

use super::token_login;
use crate::error::{Error, Result};
use crate::session::{normalize_base_url, Session};

/// Where to send the user, handed to the caller before Ironize starts waiting.
#[derive(Debug, Clone)]
pub struct LoginPrompt {
    pub authorize_url: Url,
}

/// Options for [`browser_login`].
#[derive(Debug, Clone)]
pub struct BrowserLogin {
    pub base_url: String,
    /// How long to wait for the browser to come back.
    pub timeout: Duration,
}

impl BrowserLogin {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            timeout: Duration::from_secs(300),
        }
    }
}

/// Runs the browser handoff:
///
/// 1. bind `127.0.0.1:<random port>` and mint a 128-bit `state`;
/// 2. call `on_prompt` with the Cortex authorize URL (open a browser, print it);
/// 3. Cortex signs the user in and redirects to
///    `http://127.0.0.1:<port>/callback?state=…&token=…&baseUrl=…`;
/// 4. check `state`, then verify the token against Cortex before returning.
pub async fn browser_login<F>(options: BrowserLogin, on_prompt: F) -> Result<Session>
where
    F: FnOnce(&LoginPrompt),
{
    let base = normalize_base_url(&options.base_url)?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let state = random_state();

    let mut redirect_uri = Url::parse(&format!("http://127.0.0.1:{port}/callback")).expect("valid");
    redirect_uri.query_pairs_mut().append_pair("state", &state);
    let mut authorize_url = base
        .join("/api/auth/cli/callback")
        .expect("static path joins");
    authorize_url
        .query_pairs_mut()
        .append_pair("redirect_uri", redirect_uri.as_str());
    on_prompt(&LoginPrompt { authorize_url });

    let callback = tokio::time::timeout(options.timeout, accept_callback(&listener, &state))
        .await
        .map_err(|_| {
            Error::Login("timed out waiting for the browser to finish signing in".into())
        })??;

    // The token is only trusted for the deployment we sent the user to. A
    // different `baseUrl` in the callback is accepted only if it is the same
    // origin (e.g. a trailing-slash or path difference).
    if let Some(reported) = callback.base_url.as_deref() {
        let reported = normalize_base_url(reported)?;
        if reported.origin() != base.origin() {
            return Err(Error::Login(format!(
                "Cortex reported a different deployment ({reported}) than the one requested ({base})"
            )));
        }
    }
    token_login(base.as_str(), callback.token).await
}

struct Callback {
    token: String,
    base_url: Option<String>,
}

/// Serves the loopback listener until a request carries the right `state` and
/// a token. Stray requests (favicon, probes, stale tabs) get a 400 and are
/// ignored instead of aborting the login.
async fn accept_callback(listener: &TcpListener, state: &str) -> Result<Callback> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let target = match read_request_target(&mut stream).await {
            Ok(target) => target,
            Err(_) => continue,
        };
        match parse_callback(&target, state) {
            Ok(callback) => {
                respond(
                    &mut stream,
                    200,
                    "Ironize is signed in to Mindify Cortex. You can close this window.",
                )
                .await;
                return Ok(callback);
            }
            Err(reason) => respond(&mut stream, 400, reason).await,
        }
    }
}

fn parse_callback(
    target: &str,
    expected_state: &str,
) -> std::result::Result<Callback, &'static str> {
    let url = Url::parse("http://127.0.0.1")
        .and_then(|base| base.join(target))
        .map_err(|_| "Invalid Ironize login request.")?;
    if url.path() != "/callback" {
        return Err("Not found.");
    }
    let mut state = None;
    let mut token = None;
    let mut base_url = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "state" => state = Some(value.into_owned()),
            "token" => token = Some(value.into_owned()),
            "baseUrl" => base_url = Some(value.into_owned()),
            _ => {}
        }
    }
    if !state.is_some_and(|s| constant_time_eq(s.as_bytes(), expected_state.as_bytes())) {
        return Err("Invalid Ironize login response.");
    }
    let token = token
        .filter(|t| !t.is_empty())
        .ok_or("Missing Cortex session token.")?;
    Ok(Callback { token, base_url })
}

/// Reads an HTTP/1 request head and returns the request target of a GET.
async fn read_request_target(stream: &mut TcpStream) -> std::io::Result<String> {
    const MAX_HEAD: usize = 16 * 1024;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let read_head = async {
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut chunk).await?;
            if n == 0 || buf.len() + n > MAX_HEAD {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        Ok::<_, std::io::Error>(())
    };
    tokio::time::timeout(Duration::from_secs(10), read_head)
        .await
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;

    let head = String::from_utf8_lossy(&buf);
    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("GET"), Some(target)) => Ok(target.to_string()),
        _ => Err(std::io::ErrorKind::InvalidData.into()),
    }
}

async fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_requires_matching_state_and_token() {
        let ok = parse_callback(
            "/callback?state=abc&token=t0k&baseUrl=https%3A%2F%2Fx.dev",
            "abc",
        )
        .unwrap();
        assert_eq!(ok.token, "t0k");
        assert_eq!(ok.base_url.as_deref(), Some("https://x.dev"));

        assert!(parse_callback("/callback?state=nope&token=t", "abc").is_err());
        assert!(parse_callback("/callback?token=t", "abc").is_err());
        assert!(parse_callback("/callback?state=abc", "abc").is_err());
        assert!(parse_callback("/callback?state=abc&token=", "abc").is_err());
        assert!(parse_callback("/favicon.ico", "abc").is_err());
    }

    #[test]
    fn state_is_128_bits_of_hex() {
        let state = random_state();
        assert_eq!(state.len(), 32);
        assert!(state.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(state, random_state());
    }
}
