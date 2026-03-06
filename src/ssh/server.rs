use anyhow::{Context, Result};
use russh::server::{Config, Server};
use russh_keys::key::KeyPair;
use std::sync::Arc;
use tracing::info;

use crate::app::AppState;
use crate::ssh::session::SessionHandler;

/// Warlock SSH server for VM console access.
pub struct WarlockSshServer {
    app_state: Arc<AppState>,
}

impl WarlockSshServer {
    pub fn new(app_state: Arc<AppState>) -> Self {
        Self { app_state }
    }

    /// Run the SSH server on the specified port.
    pub async fn run(mut self, port: u16) -> Result<()> {
        // Load or generate persistent SSH host key
        // For now, we'll generate a new key each time. In production, this should
        // be persisted to disk to avoid "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!"
        // errors when the server restarts.
        // TODO: Implement proper key persistence once we determine the correct russh_keys API
        let host_key = KeyPair::generate_ed25519()
            .ok_or_else(|| anyhow::anyhow!("Failed to generate SSH host key"))?;
        
        info!("Generated ephemeral SSH host key (TODO: persist to disk)");

        let config = Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(3600)),
            auth_rejection_time: std::time::Duration::from_secs(1),
            auth_rejection_time_initial: Some(std::time::Duration::from_secs(0)),
            keys: vec![host_key],
            ..Default::default()
        };

        let config = Arc::new(config);
        let bind_addr = ("0.0.0.0", port);

        info!("SSH server listening on {}:{}", bind_addr.0, bind_addr.1);

        self.run_on_address(config, bind_addr)
            .await
            .context("SSH server failed")?;

        Ok(())
    }
}

impl Server for WarlockSshServer {
    type Handler = SessionHandler;

    fn new_client(&mut self, _peer_addr: Option<std::net::SocketAddr>) -> Self::Handler {
        SessionHandler::new(self.app_state.clone())
    }
}
