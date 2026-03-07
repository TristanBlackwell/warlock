use anyhow::{Result, anyhow};
use async_trait::async_trait;
use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId, CryptoVec};
use russh_keys::key::PublicKey;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::app::{AppState, VmEntry};
use crate::ssh::auth::parse_vm_id_from_username;
use crate::ssh::authorized_keys::is_key_authorized;

/// SSH session state for a single client connection.
pub struct SessionHandler {
    app_state: Arc<AppState>,
    vm_id: Option<Uuid>,
    uds_writer: Option<mpsc::UnboundedSender<Vec<u8>>>,
    channel_id: Option<ChannelId>,
}

impl SessionHandler {
    pub fn new(app_state: Arc<AppState>) -> Self {
        Self {
            app_state,
            vm_id: None,
            uds_writer: None,
            channel_id: None,
        }
    }
}

#[async_trait]
impl Handler for SessionHandler {
    type Error = anyhow::Error;

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        debug!(username = user, "SSH auth attempt with public key");

        // Parse username to get VM ID
        match parse_vm_id_from_username(user)? {
            Some(vm_id) => {
                // Check if VM exists and is running
                let vms = self.app_state.vms.lock().await;
                match vms.get(&vm_id) {
                    Some(VmEntry::Running { resources, .. }) => {
                        // Validate the SSH key against the VM's authorized keys
                        match is_key_authorized(key, &resources.ssh_keys) {
                            Ok(true) => {
                                info!(
                                    vm_id = %vm_id,
                                    username = user,
                                    key_type = key.name(),
                                    "SSH auth successful"
                                );
                                self.vm_id = Some(vm_id);
                                Ok(Auth::Accept)
                            }
                            Ok(false) => {
                                warn!(
                                    vm_id = %vm_id,
                                    username = user,
                                    key_type = key.name(),
                                    "Rejecting auth - SSH key not authorized"
                                );
                                Ok(Auth::Reject {
                                    proceed_with_methods: None,
                                })
                            }
                            Err(e) => {
                                warn!(
                                    vm_id = %vm_id,
                                    error = ?e,
                                    "Error validating SSH key"
                                );
                                Ok(Auth::Reject {
                                    proceed_with_methods: None,
                                })
                            }
                        }
                    }
                    Some(VmEntry::Creating(_)) => {
                        warn!(vm_id = %vm_id, "Rejecting auth - VM still creating");
                        Ok(Auth::Reject {
                            proceed_with_methods: None,
                        })
                    }
                    None => {
                        warn!(vm_id = %vm_id, "Rejecting auth - VM not found");
                        Ok(Auth::Reject {
                            proceed_with_methods: None,
                        })
                    }
                }
            }
            None => {
                warn!(username = user, "Rejecting auth - invalid username format");
                Ok(Auth::Reject {
                    proceed_with_methods: None,
                })
            }
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        debug!("SSH channel opened");
        self.channel_id = Some(channel.id());
        Ok(true)
    }

    async fn pty_request(
        &mut self,
        _channel: ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        debug!(
            term = term,
            cols = col_width,
            rows = row_height,
            "PTY request"
        );
        // Accept PTY request
        Ok(())
    }

    async fn shell_request(
        &mut self,
        _channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let vm_id = self.vm_id.ok_or_else(|| anyhow!("No VM selected"))?;
        let channel_id = self.channel_id.ok_or_else(|| anyhow!("No channel"))?;

        info!(vm_id = %vm_id, "Shell request - connecting to vsock");

        // Get vsock UDS path
        let vsock_uds_path = {
            let vms = self.app_state.vms.lock().await;
            let vm_entry = vms.get(&vm_id).ok_or_else(|| anyhow!("VM not found"))?;

            match vm_entry {
                VmEntry::Running { resources, .. } => resources
                    .vsock_uds_path
                    .as_ref()
                    .ok_or_else(|| anyhow!("VM has no vsock configured"))?
                    .clone(),
                VmEntry::Creating(_) => {
                    return Err(anyhow!("VM is still creating"));
                }
            }
        };

        // Connect to vsock UDS
        let mut uds_stream = UnixStream::connect(&vsock_uds_path).await?;

        // Send CONNECT handshake
        let connect_msg = format!("CONNECT {}\n", 1024);
        uds_stream.write_all(connect_msg.as_bytes()).await?;

        // Read "OK {port}\n" response
        let mut handshake_buf = [0u8; 32];
        let n = uds_stream.read(&mut handshake_buf).await?;
        let response = std::str::from_utf8(&handshake_buf[..n])?;

        if !response.starts_with("OK ") {
            return Err(anyhow!("vsock handshake failed: {}", response.trim()));
        }

        info!(vm_id = %vm_id, "vsock handshake successful");

        // Split the stream
        let (mut read_half, mut write_half) = tokio::io::split(uds_stream);

        // Create channel for sending data to UDS
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.uds_writer = Some(tx);

        // Spawn task to forward SSH → UDS
        tokio::spawn(async move {
            while let Some(data) = rx.recv().await {
                if let Err(e) = write_half.write_all(&data).await {
                    warn!(error = ?e, "UDS write error");
                    break;
                }
            }
        });

        // Spawn task to forward UDS → SSH
        let handle = session.handle();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];

            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => {
                        debug!("UDS stream closed");
                        break;
                    }
                    Ok(n) => {
                        // Convert slice to CryptoVec
                        let data = CryptoVec::from_slice(&buf[..n]);
                        if let Err(e) = handle.data(channel_id, data).await {
                            warn!(error = ?e, "Failed to send data to SSH client");
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(error = ?e, "UDS read error");
                        break;
                    }
                }
            }

            let _ = handle.close(channel_id).await;
        });

        Ok(())
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Forward SSH input to vsock UDS via channel
        if let Some(ref tx) = self.uds_writer {
            let _ = tx.send(data.to_vec());
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        debug!(cols = col_width, rows = row_height, "Window change request");

        // Send terminal resize escape sequence to guest
        // Format: ESC[8;<height>;<width>t
        let resize_msg = format!("\x1b[8;{};{}t", row_height, col_width);

        if let Some(ref tx) = self.uds_writer {
            let _ = tx.send(resize_msg.into_bytes());
        }

        Ok(())
    }
}
