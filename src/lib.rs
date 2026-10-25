//! Ironize — an AI agent harness and terminal UI written in Rust, powered by
//! Mindify Cortex.
//!
//! * [`auth`] signs a user in (browser loopback handoff, email + password, or
//!   a pre-issued token) and verifies the resulting Better Auth session.
//! * [`session`] and [`config`] persist the session and preferences privately.
//! * [`cortex`] is the typed HTTP client for the Cortex control plane and the
//!   model gateway; [`gateway`] keeps a scoped launch token alive.
//! * [`tools`] are the built-in tools (files, search, shell, web, todos).
//! * [`agent`] runs the tool-calling loop with permission checks.
//! * [`tui`] is the interactive terminal interface.

pub mod agent;
pub mod auth;
pub mod config;
pub mod cortex;
pub mod error;
pub mod gateway;
pub mod session;
pub mod tools;
pub mod tui;

pub use cortex::CortexClient;
pub use error::{Error, Result};
pub use session::{Session, SessionStore};

/// Production Cortex deployment used when nothing else is configured.
pub const DEFAULT_BASE_URL: &str = "https://cortex.mindifyai.dev";
