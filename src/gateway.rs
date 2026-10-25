//! A long-lived connection to the Cortex model gateway.
//!
//! Model calls are authenticated with a scoped launch token (30 minutes,
//! extended by heartbeats), never with the user's session. [`Gateway`] creates
//! the launch lazily, heartbeats it between turns, transparently replaces it
//! when it has expired, and revokes it on [`Gateway::close`].

use std::time::{Duration, Instant};

use crate::cortex::{ChatStream, CortexClient, Harness, LaunchTicket, Message, ToolDefinition};
use crate::error::{Error, Result};

/// Heartbeat when the token was last refreshed this long ago. Well inside the
/// server's 30-minute expiry.
const HEARTBEAT_AFTER: Duration = Duration::from_secs(5 * 60);

pub struct Gateway {
    client: CortexClient,
    workspace: String,
    harness: Harness,
    model: String,
    launch: Option<Launch>,
}

struct Launch {
    ticket: LaunchTicket,
    refreshed: Instant,
}

impl Gateway {
    pub fn new(
        client: CortexClient,
        workspace: impl Into<String>,
        harness: Harness,
        model: impl Into<String>,
    ) -> Self {
        Self {
            client,
            workspace: workspace.into(),
            harness,
            model: model.into(),
            launch: None,
        }
    }

    pub fn client(&self) -> &CortexClient {
        &self.client
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    pub fn harness(&self) -> Harness {
        self.harness
    }

    /// Switches models. The new launch is registered immediately so an
    /// unavailable model is reported now rather than on the next prompt.
    pub async fn set_model(&mut self, model: &str) -> Result<()> {
        let ticket = self
            .client
            .create_launch(&self.workspace, self.harness, model)
            .await?;
        self.close().await;
        self.model = ticket.model.clone();
        self.launch = Some(Launch {
            ticket,
            refreshed: Instant::now(),
        });
        Ok(())
    }

    /// Registers the launch now (if needed), validating model and workspace.
    pub async fn connect(&mut self) -> Result<()> {
        self.ticket().await.map(drop)
    }

    /// Streams one model turn, renewing the launch once if it was revoked or
    /// expired while idle.
    pub async fn stream(
        &mut self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<ChatStream> {
        let ticket = self.ticket().await?.clone();
        match self.client.stream_chat(&ticket, messages, tools).await {
            Err(Error::Unauthenticated(_)) => {
                self.launch = None;
                let ticket = self.ticket().await?.clone();
                self.client.stream_chat(&ticket, messages, tools).await
            }
            other => other,
        }
    }

    /// Revokes the current launch token, if any. Errors are ignored: the token
    /// expires on its own.
    pub async fn close(&mut self) {
        if let Some(launch) = self.launch.take() {
            let _ = self.client.finish_launch(&launch.ticket.launch_id, 0).await;
        }
    }

    async fn ticket(&mut self) -> Result<&LaunchTicket> {
        if let Some(launch) = &self.launch {
            if launch.refreshed.elapsed() >= HEARTBEAT_AFTER {
                match self.client.heartbeat_launch(&launch.ticket.launch_id).await {
                    Ok(()) => {}
                    // The session itself is gone: nothing to renew with.
                    Err(e @ Error::Unauthenticated(_)) => return Err(e),
                    // Launch expired or was finished server-side: start over.
                    Err(_) => self.launch = None,
                }
            }
        }
        match &mut self.launch {
            Some(launch) => {
                if launch.refreshed.elapsed() >= HEARTBEAT_AFTER {
                    launch.refreshed = Instant::now();
                }
            }
            None => {
                let ticket = self
                    .client
                    .create_launch(&self.workspace, self.harness, &self.model)
                    .await?;
                self.model = ticket.model.clone();
                self.launch = Some(Launch {
                    ticket,
                    refreshed: Instant::now(),
                });
            }
        }
        Ok(&self
            .launch
            .as_ref()
            .expect("launch was just ensured")
            .ticket)
    }
}
