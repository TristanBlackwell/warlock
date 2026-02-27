use std::{collections::HashMap, path::PathBuf, sync::Arc};

use axum::{
    routing::{delete, get},
    Router,
};
use firecracker_rs_sdk::instance::Instance;
use tokio::sync::Mutex;
use tower_http::{catch_panic::CatchPanicLayer, trace::TraceLayer};
use uuid::Uuid;

use crate::{capacity::Capacity, firecracker::JailerConfig, handlers};

/// A running VM and the resources allocated to it.
pub struct VmEntry {
    pub instance: Instance,
    pub vcpus: u8,
    pub memory_mb: u32,
    /// Path to the per-VM rootfs copy (cleaned up on delete/shutdown).
    pub rootfs_copy: Option<PathBuf>,
}

pub struct AppState {
    pub capacity: Capacity,
    pub jailer: JailerConfig,
    pub vms: Mutex<HashMap<Uuid, VmEntry>>,
}

pub fn create_app(capacity: Capacity, jailer: JailerConfig) -> (Router, Arc<AppState>) {
    let state = Arc::new(AppState {
        capacity,
        jailer,
        vms: Mutex::new(HashMap::new()),
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
