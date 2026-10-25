//! Wire types for the Cortex endpoints Ironize uses.

use serde::{Deserialize, Serialize};

use crate::session::SessionUser;

/// `GET /api/auth/get-session` (Better Auth). The server answers `null` when
/// the token is unknown or expired.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthSession {
    pub session: AuthSessionInfo,
    pub user: SessionUser,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSessionInfo {
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// `POST /api/auth/sign-in/email` (Better Auth).
#[derive(Debug, Clone, Deserialize)]
pub struct EmailSignInResponse {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub user: Option<SessionUser>,
}

/// The coding-agent protocol a gateway launch is registered for. Cortex scopes
/// the model catalog and workspace policy per harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Claude,
    Codex,
    Gemini,
    /// OpenAI Chat Completions, the protocol Ironize's gateway client speaks.
    Opencode,
}

impl Harness {
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Gemini => "gemini",
            Harness::Opencode => "opencode",
        }
    }
}

/// One model from the workspace gateway catalog (`GET /api/gateway`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: String,
    pub provider: String,
    pub display_name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Kept as strings so a harness added server-side does not break parsing.
    #[serde(default)]
    pub compatible_harnesses: Vec<String>,
}

impl CatalogEntry {
    pub fn supports(&self, harness: Harness) -> bool {
        self.compatible_harnesses
            .iter()
            .any(|h| h == harness.as_str())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GatewayOverview {
    #[serde(default)]
    pub catalog: Vec<CatalogEntry>,
}

/// A short-lived (30 minute) gateway token scoped to one user, workspace,
/// harness and model. The long-lived session token never leaves Ironize.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchTicket {
    pub launch_id: String,
    pub token: String,
    pub expires_at: String,
    /// The gateway root, e.g. `https://cortex.mindifyai.dev/api`.
    pub base_url: String,
    pub model: String,
    pub workspace: String,
}

impl std::fmt::Debug for LaunchTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchTicket")
            .field("launch_id", &self.launch_id)
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("workspace", &self.workspace)
            .finish()
    }
}
