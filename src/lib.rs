//! Iron — an AI agent harness written in Rust.
//!
//! This crate currently provides the foundation every Iron agent needs: an
//! authenticated connection to the Mindify Cortex platform.
//!
//! * [`auth`] signs a user in (browser loopback handoff, email + password, or
//!   a pre-issued token) and verifies the resulting Better Auth session.
//! * [`session`] persists that session privately on disk.
//! * [`cortex`] is the typed HTTP client for the Cortex control plane and the
//!   model gateway, including short-lived scoped launch tokens.

pub mod auth;
pub mod cortex;
pub mod error;
pub mod session;

pub use cortex::CortexClient;
pub use error::{Error, Result};
pub use session::{Session, SessionStore};

/// Production Cortex deployment used when nothing else is configured.
pub const DEFAULT_BASE_URL: &str = "https://cortex.mindifyai.dev";
