use std::{collections::HashMap, path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get},
};
use firecracker_rs_sdk::instance::Instance;
use tokio::sync::Mutex;
use tower_http::{catch_panic::CatchPanicLayer, trace::TraceLayer};
use uuid::Uuid;

use crate::{
    capacity::Capacity,
    firecracker::{JailerConfig, network::NatHandles},
    gateway_client::GatewayClient,
    handlers,
    vm::network::SubnetPool,
};

/// Resources allocated to a VM, shared across all lifecycle states.
pub struct VmResources {
    pub vcpus: u8,
    pub memory_mb: u32,
    /// Path to the per-VM rootfs copy (cleaned up on delete/shutdown).
    pub rootfs_copy: Option<PathBuf>,
    /// Name of the tap device (e.g. `fc0`), if networking is configured.
    pub tap_name: Option<String>,
    /// Subnet pool index for this VM's /30 allocation.
    pub subnet_index: Option<u16>,
    /// nftables rule handles for NAT cleanup.
    pub nat_handles: Option<NatHandles>,
    /// Guest IP address assigned to this VM.
    pub guest_ip: Option<String>,
    /// Path to the vsock Unix domain socket (for console access).
    pub vsock_uds_path: Option<PathBuf>,
    /// SSH public keys authorized to access this VM's console.
    /// Each key is in OpenSSH authorized_keys format (e.g., "ssh-ed25519 AAAA...").
    pub ssh_keys: Vec<String>,
}

/// A VM entry in the state map. The variant determines what operations
/// are valid — the Firecracker `Instance` only exists in the `Running`
/// state, enforced at the type level.
pub enum VmEntry {
    /// Resources are reserved and capacity is accounted for, but the
    /// Firecracker instance is still being set up. The vms lock is NOT
    /// held during this phase, allowing other handlers to proceed.
    Creating(VmResources),
    /// Firecracker instance is active and queryable.
    Running {
        instance: Instance,
        resources: VmResources,
    },
}

impl VmEntry {
    /// Returns the status label for API responses.
    pub fn status(&self) -> &'static str {
        match self {
            VmEntry::Creating(_) => "creating",
            VmEntry::Running { .. } => "running",
        }
    }

    /// Returns a reference to the VM's allocated resources.
    pub fn resources(&self) -> &VmResources {
        match self {
            VmEntry::Creating(r) | VmEntry::Running { resources: r, .. } => r,
        }
    }
}

pub struct AppState {
    pub capacity: Capacity,
    pub jailer: JailerConfig,
    pub vms: Mutex<HashMap<Uuid, VmEntry>>,
    pub subnet_pool: Mutex<SubnetPool>,
    pub gateway_client: Option<GatewayClient>,
}

pub fn create_app(capacity: Capacity, jailer: JailerConfig, gateway_client: Option<GatewayClient>) -> (Router, Arc<AppState>) {
    let state = Arc::new(AppState {
        capacity,
        jailer,
        vms: Mutex::new(HashMap::new()),
        subnet_pool: Mutex::new(SubnetPool::new()),
        gateway_client,
    });

    let router = Router::new()
        .route("/internal/health", get(handlers::healthcheck::health))
        .route("/internal/ready", get(handlers::healthcheck::ready))
        .route("/vm", get(handlers::vm::list).post(handlers::vm::create))
        .route("/vm/{id}", get(handlers::vm::get))
        .route("/vm/{id}", delete(handlers::vm::delete))
        .fallback(fallback)
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new())
        .with_state(state.clone());

    (router, state)
}

async fn fallback() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": "Not found" })),
    )
}
