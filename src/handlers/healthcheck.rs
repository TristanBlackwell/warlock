use std::sync::Arc;

use axum::{Json, extract::State};
use serde::Serialize;

use crate::app::AppState;
use crate::firecracker::CopyStrategy;

#[derive(Serialize)]
pub struct HealthResponse {
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

pub async fn healthcheck(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let capacity = Some(CapacityInfo {
        total_memory_mb: state.capacity.memory_mb,
        allocatable_memory_mb: state.capacity.allocatable_memory_mb(),
        total_vcpus: state.capacity.vcpus,
    });

    let copy_strategy = Some(match state.jailer.copy_strategy {
        CopyStrategy::Reflink => "reflink",
        CopyStrategy::Sparse => "sparse",
    });

    // Try to get VM info, but don't fail the healthcheck if the lock is
    // contended (try_lock avoids blocking).
    let vms = state.vms.try_lock().ok().map(|vms| {
        let allocated_vcpus: u8 = vms.values().map(|e| e.vcpus).sum();
        let allocated_memory_mb: u64 = vms.values().map(|e| e.memory_mb as u64).sum();
        VmInfo {
            count: vms.len(),
            allocated_vcpus,
            allocated_memory_mb,
        }
    });

    Json(HealthResponse {
        status: "healthy",
        version: env!("CARGO_PKG_VERSION"),
        capacity,
        vms,
        copy_strategy,
    })
}
