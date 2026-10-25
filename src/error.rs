use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    /// The user is not signed in, or the saved session was rejected.
    #[error("not signed in to Cortex: {0}")]
    Unauthenticated(String),

    /// Cortex answered with a non-success status.
    #[error("Cortex API error (HTTP {status}): {message}")]
    Api { status: u16, message: String },

    /// Cortex answered, but not with the JSON this client expects (an HTML
    /// error page, a proxy interstitial, or an older server).
    #[error("Cortex returned an unexpected response while trying to {operation}; the server may not support this Ironize version yet")]
    UnexpectedResponse { operation: String },

    #[error("invalid Cortex base URL {url:?}: {reason}")]
    InvalidBaseUrl { url: String, reason: String },

    #[error("login failed: {0}")]
    Login(String),

    #[error(transparent)]
    Http(#[from] reqwest::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
