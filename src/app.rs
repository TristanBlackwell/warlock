use std::{collections::HashMap, path::PathBuf, sync::Arc};

use axum::{
    routing::{delete, get},
    Router,
};
use firecracker_rs_sdk::instance::Instance;
use tokio::sync::Mutex;
use tower_http::{catch_panic::CatchPanicLayer, trace::TraceLayer};
use uuid::Uuid;

use crate::{
    capacity::Capacity,
    firecracker::{network::NatHandles, JailerConfig},
    handlers,
    vm::network::SubnetPool,
};

/// A running VM and the resources allocated to it.
pub struct VmEntry {
    pub instance: Instance,
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
}

pub struct AppState {
    pub capacity: Capacity,
    pub jailer: JailerConfig,
    pub vms: Mutex<HashMap<Uuid, VmEntry>>,
    pub subnet_pool: Mutex<SubnetPool>,
}

pub fn create_app(capacity: Capacity, jailer: JailerConfig) -> (Router, Arc<AppState>) {
    let state = Arc::new(AppState {
        capacity,
        jailer,
        vms: Mutex::new(HashMap::new()),
        subnet_pool: Mutex::new(SubnetPool::new()),
    });

    let router = Router::new()
        .route("/internal/hc", get(handlers::healthcheck::healthcheck))
        .route("/vm", get(handlers::vm::list).post(handlers::vm::create))
        .route("/vm/{id}", get(handlers::vm::get))
        .route("/vm/{id}", delete(handlers::vm::delete))
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new())
        .with_state(state.clone());

    (router, state)
}
