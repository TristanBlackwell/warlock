use anyhow::{Context, Result};
use reqwest::Client;
use serde::Serialize;
use std::time::Duration;
use tracing::{debug, error, info};
use uuid::Uuid;

#[derive(Clone)]
pub struct GatewayClient {
    client: Client,
    base_url: String,
    worker_id: String,
    worker_ip: String,
}

#[derive(Debug, Serialize)]
struct WorkerRegistration {
    worker_id: String,
    ip_address: String,
}

#[derive(Debug, Serialize)]
struct VmRegistration {
    vm_id: String,
    worker_id: String,
}

impl GatewayClient {
    /// Create a new gateway client
    ///
    /// Returns None if GATEWAY_URL is not set (gateway is optional).
    ///
    /// If GATEWAY_URL is set, WORKER_IP must also be set (explicitly required
    /// for production deployments). WORKER_ID defaults to hostname if not set.
    pub fn new() -> Result<Option<Self>> {
        let base_url = match std::env::var("GATEWAY_URL") {
            Ok(url) => url,
            Err(_) => {
                info!("GATEWAY_URL not set - running without gateway integration");
                return Ok(None);
            }
        };

        // If gateway is enabled, worker_id is required (fallback to hostname)
        let worker_id = std::env::var("WORKER_ID")
            .or_else(|_| {
                hostname::get()
                    .context("Failed to get hostname")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("Hostname contains invalid UTF-8"))
            })
            .context(
                "WORKER_ID not set and hostname detection failed - set WORKER_ID explicitly",
            )?;

        let worker_ip = std::env::var("WORKER_IP")
            .context("WORKER_IP must be set when GATEWAY_URL is configured")?;

        let client = Client::builder().timeout(Duration::from_secs(5)).build()?;

        info!(
            "Gateway client initialized: {} @ {} -> {}",
            worker_id, worker_ip, base_url
        );

        Ok(Some(Self {
            client,
            base_url,
            worker_id,
            worker_ip,
        }))
    }

    /// Register this worker with the gateway
    pub async fn register(&self) -> Result<()> {
        let url = format!("{}/worker/register", self.base_url);
        let body = WorkerRegistration {
            worker_id: self.worker_id.clone(),
            ip_address: self.worker_ip.clone(),
        };

        debug!("Registering worker with gateway: {:?}", body);

        self.client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to register with gateway")?
            .error_for_status()
            .context("Gateway returned error on registration")?;

        info!("Successfully registered with gateway");
        Ok(())
    }

    /// Send heartbeat to gateway
    pub async fn heartbeat(&self) -> Result<()> {
        let url = format!("{}/worker/{}/heartbeat", self.base_url, self.worker_id);

        debug!("Sending heartbeat to gateway");

        self.client
            .post(&url)
            .send()
            .await
            .context("Failed to send heartbeat")?
            .error_for_status()
            .context("Gateway returned error on heartbeat")?;

        debug!("Heartbeat sent successfully");
        Ok(())
    }

    /// Register a VM with the gateway
    pub async fn register_vm(&self, vm_id: Uuid) -> Result<()> {
        let url = format!("{}/vm/register", self.base_url);
        let body = VmRegistration {
            vm_id: vm_id.to_string(),
            worker_id: self.worker_id.clone(),
        };

        debug!("Registering VM with gateway: {:?}", body);

        self.client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to register VM with gateway")?
            .error_for_status()
            .context("Gateway returned error on VM registration")?;

        info!("VM {} registered with gateway", vm_id);
        Ok(())
    }

    /// Deregister a VM from the gateway
    pub async fn deregister_vm(&self, vm_id: Uuid) -> Result<()> {
        let url = format!("{}/vm/{}", self.base_url, vm_id);

        debug!("Deregistering VM from gateway: {}", vm_id);

        self.client
            .delete(&url)
            .send()
            .await
            .context("Failed to deregister VM from gateway")?
            .error_for_status()
            .context("Gateway returned error on VM deregistration")?;

        info!("VM {} deregistered from gateway", vm_id);
        Ok(())
    }
}

/// Spawn the heartbeat background task
pub fn spawn_heartbeat_task(client: GatewayClient) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));

        loop {
            interval.tick().await;

            if let Err(e) = client.heartbeat().await {
                error!("Failed to send heartbeat to gateway: {:#}", e);
            }
        }
    });
}
