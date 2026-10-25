//! Session model and private on-disk persistence.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{Error, Result};
use crate::DEFAULT_BASE_URL;

/// Environment variable carrying a pre-issued Cortex session token, for CI and
/// other non-interactive use. Shared with the TypeScript Cortex CLI.
pub const TOKEN_ENV: &str = "CORTEX_TOKEN";
/// Environment variable overriding the Cortex deployment.
pub const BASE_URL_ENV: &str = "CORTEX_BASE_URL";

/// The signed-in user, as reported by Cortex.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUser {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

/// A Cortex (Better Auth) session: a bearer token bound to a deployment.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub token: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<SessionUser>,
}

impl Session {
    pub fn new(token: impl Into<String>, base_url: &str) -> Result<Self> {
        Ok(Self {
            token: token.into(),
            base_url: normalize_base_url(base_url)?.to_string(),
            user: None,
        })
    }
}

// Never print the bearer token, even in debug output.
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("token", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("user", &self.user)
            .finish()
    }
}

/// Parses a Cortex base URL down to its origin.
///
/// Bearer tokens and passwords are sent to this host, so plain `http` is only
/// accepted for loopback development servers.
pub fn normalize_base_url(raw: &str) -> Result<Url> {
    let invalid = |reason: &str| Error::InvalidBaseUrl {
        url: raw.to_string(),
        reason: reason.to_string(),
    };
    let url = Url::parse(raw.trim()).map_err(|e| invalid(&e.to_string()))?;
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(&url) => {}
        "http" => return Err(invalid("plain http is only allowed for localhost")),
        _ => return Err(invalid("expected an https:// URL")),
    }
    if url.host_str().is_none() {
        return Err(invalid("missing host"));
    }
    let origin = url.origin().ascii_serialization();
    Url::parse(&origin).map_err(|e| invalid(&e.to_string()))
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// `~/.config/ironize` (or the platform equivalent), overridable with
/// `IRONIZE_CONFIG_DIR`.
pub fn config_dir() -> Result<PathBuf> {
    match std::env::var_os("IRONIZE_CONFIG_DIR") {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => dirs::config_dir()
            .map(|d| d.join("ironize"))
            .ok_or_else(|| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "could not determine the user config directory",
                ))
            }),
    }
}

/// Reads and writes the saved session under the Ironize config directory.
#[derive(Debug, Clone)]
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    /// `session.json` in [`config_dir`].
    pub fn default_location() -> Result<Self> {
        Ok(Self::at(config_dir()?.join("session.json")))
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The active session: `CORTEX_TOKEN` wins over the saved file.
    pub fn load(&self) -> Result<Option<Session>> {
        if let Some(token) = std::env::var(TOKEN_ENV).ok().filter(|t| !t.is_empty()) {
            let base_url =
                std::env::var(BASE_URL_ENV).unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
            return Session::new(token, &base_url).map(Some);
        }
        self.load_saved()
    }

    /// The session saved by `ironize login`, ignoring the environment.
    pub fn load_saved(&self) -> Result<Option<Session>> {
        match std::fs::read_to_string(&self.path) {
            Ok(body) => Ok(Some(serde_json::from_str(&body)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, session: &Session) -> Result<()> {
        let body = format!("{}\n", serde_json::to_string_pretty(session)?);
        write_private(&self.path, body.as_bytes())
    }

    /// Removes the saved session. Succeeds if there was none.
    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Writes a file readable only by the current user (0600 in a 0700 dir on
/// Unix), replacing it atomically so a crash never leaves a torn session.
pub(crate) fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;

    let tmp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_is_reduced_to_origin() {
        let url = normalize_base_url("https://cortex.mindifyai.dev/chat/abc?x=1").unwrap();
        assert_eq!(url.as_str(), "https://cortex.mindifyai.dev/");
    }

    #[test]
    fn plain_http_only_for_loopback() {
        assert!(normalize_base_url("http://localhost:3000").is_ok());
        assert!(normalize_base_url("http://127.0.0.1:3000").is_ok());
        assert!(normalize_base_url("http://[::1]:3000").is_ok());
        assert!(normalize_base_url("http://cortex.example.com").is_err());
        assert!(normalize_base_url("ftp://cortex.example.com").is_err());
        assert!(normalize_base_url("not a url").is_err());
    }

    #[test]
    fn debug_output_redacts_token() {
        let session = Session::new("secret-token", "https://cortex.mindifyai.dev").unwrap();
        assert!(!format!("{session:?}").contains("secret-token"));
    }

    #[test]
    fn store_round_trips_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path().join("nested").join("session.json"));
        assert!(store.load_saved().unwrap().is_none());

        let mut session = Session::new("tok", "https://cortex.mindifyai.dev").unwrap();
        session.user = Some(SessionUser {
            id: "u1".into(),
            name: Some("Ada".into()),
            email: Some("ada@example.com".into()),
        });
        store.save(&session).unwrap();
        assert_eq!(store.load_saved().unwrap(), Some(session));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        store.clear().unwrap();
        store.clear().unwrap();
        assert!(store.load_saved().unwrap().is_none());
    }
}
