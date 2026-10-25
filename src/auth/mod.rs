//! Signing in to Mindify Cortex.
//!
//! Three ways to obtain a session, all ending in a verified [`Session`]:
//!
//! * [`browser_login`] — the default. Opens Cortex's `/api/auth/cli/callback`
//!   handoff in a browser and receives the session token on a one-shot
//!   loopback listener (RFC 8252), guarded by a random `state`. Works with
//!   every sign-in method Cortex offers (email, Google, GitHub, …).
//! * [`password_login`] — headless email + password sign-in against Better
//!   Auth, for machines without a browser.
//! * [`token_login`] — adopts an existing session token (e.g. from CI).

mod loopback;

pub use loopback::{browser_login, BrowserLogin, LoginPrompt};

use reqwest::header;

use crate::cortex::{self, CortexClient, EmailSignInResponse};
use crate::error::{Error, Result};
use crate::session::{normalize_base_url, Session};

/// Better Auth's `bearer()` plugin returns the session token in this header.
const AUTH_TOKEN_HEADER: &str = "set-auth-token";

/// Signs in with email and password (`POST /api/auth/sign-in/email`) and
/// verifies the resulting session.
pub async fn password_login(base_url: &str, email: &str, password: &str) -> Result<Session> {
    let base = normalize_base_url(base_url)?;
    let url = base
        .join("/api/auth/sign-in/email")
        .expect("static path joins");
    let response = cortex::http_client()?
        .post(url)
        // Better Auth checks the Origin of state-changing requests against its
        // trusted origins; the deployment's own origin is always trusted.
        .header(header::ORIGIN, base.origin().ascii_serialization())
        .json(&serde_json::json!({ "email": email, "password": password, "rememberMe": true }))
        .send()
        .await?;
    let response = cortex::check_status(response, "sign in")
        .await
        .map_err(|e| match e {
            Error::Unauthenticated(msg) => Error::Login(msg),
            other => other,
        })?;

    let header_token = response
        .headers()
        .get(AUTH_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body: EmailSignInResponse = cortex::read_json(response, "sign in").await?;
    let token = header_token
        .or(body.token)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| Error::Login("Cortex did not return a session token".into()))?;

    token_login(base.as_str(), token).await
}

/// Verifies an existing session token and returns it as a [`Session`] with the
/// signed-in user attached.
pub async fn token_login(base_url: &str, token: impl Into<String>) -> Result<Session> {
    let session = Session::new(token, base_url)?;
    let verified = CortexClient::new(session.clone())?.verify().await?;
    Ok(Session {
        user: Some(verified.user),
        ..session
    })
}
