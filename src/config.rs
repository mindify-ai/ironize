//! User preferences and prompt history, stored next to the session.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agent::PermissionMode;
use crate::error::Result;
use crate::session::{config_dir, write_private};

const HISTORY_LIMIT: usize = 500;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Gateway model used when `--model` is not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// `personal` or an organization ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

impl Config {
    fn path() -> Result<PathBuf> {
        Ok(config_dir()?.join("config.json"))
    }

    /// Loads the config; a missing or unreadable file yields the defaults.
    pub fn load() -> Self {
        Self::path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|body| serde_json::from_str(&body).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let body = format!("{}\n", serde_json::to_string_pretty(self)?);
        write_private(&Self::path()?, body.as_bytes())
    }
}

/// Previously submitted prompts, oldest first.
pub fn load_history() -> Vec<String> {
    config_dir()
        .ok()
        .and_then(|d| std::fs::read_to_string(d.join("history.json")).ok())
        .and_then(|body| serde_json::from_str::<Vec<String>>(&body).ok())
        .unwrap_or_default()
}

pub fn save_history(history: &[String]) -> Result<()> {
    let start = history.len().saturating_sub(HISTORY_LIMIT);
    let body = serde_json::to_string(&history[start..])?;
    write_private(&config_dir()?.join("history.json"), body.as_bytes())
}
