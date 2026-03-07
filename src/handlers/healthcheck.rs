use std::sync::Arc;

use axum::{Json, extract::State};
use serde::Serialize;

use crate::app::AppState;
use crate::firecracker::CopyStrategy;

// ── Liveness probe (/internal/health) ──

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

/// Minimal liveness probe. Returns 200 if the process is alive and can
/// serve HTTP. No lock acquisition, no I/O — as cheap as possible.
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

// ── Readiness probe (/internal/ready) ──

#[derive(Serialize)]
pub struct ReadyResponse {
    pub status: &'static str,
    pub version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<CapacityInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vms: Option<VmInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_strategy: Option<&'static str>,
}

#[derive(Serialize)]
pub struct CapacityInfo {
    pub total_memory_mb: u64,
    pub allocatable_memory_mb: u64,
    pub total_vcpus: u8,
}

#[derive(Serialize)]
pub struct VmInfo {
    pub count: usize,
    pub allocated_vcpus: u8,
    pub allocated_memory_mb: u64,
}

/// Enriched readiness probe. Reports capacity, VM allocations, and copy
/// strategy. Suitable for load-balancer health checks that need to know
/// the server can actually do useful work.
///
/// Uses `try_lock` for the VM map so a contended mutex doesn't block
/// the probe — the `vms` field is simply omitted if the lock can't be
/// acquired immediately.
pub async fn ready(State(state): State<Arc<AppState>>) -> Json<ReadyResponse> {
    let capacity = Some(CapacityInfo {
        total_memory_mb: state.capacity.memory_mb,
        allocatable_memory_mb: state.capacity.allocatable_memory_mb(),
        total_vcpus: state.capacity.vcpus,
    });

    let copy_strategy = Some(match state.jailer.copy_strategy {
        CopyStrategy::Reflink => "reflink",
        CopyStrategy::Sparse => "sparse",
    });

    // Try to get VM info, but don't fail the readiness probe if the lock
    // is contended (try_lock avoids blocking).
    let vms = state.vms.try_lock().ok().map(|vms| {
        let allocated_vcpus: u8 = vms.values().map(|e| e.resources().vcpus).sum();
        let allocated_memory_mb: u64 = vms.values().map(|e| e.resources().memory_mb as u64).sum();
        VmInfo {
            count: vms.len(),
            allocated_vcpus,
            allocated_memory_mb,
        }
    });

    Json(ReadyResponse {
        status: "ready",
        version: env!("CARGO_PKG_VERSION"),
        capacity,
        vms,
        copy_strategy,
    })
}
