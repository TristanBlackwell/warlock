use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use firecracker_rs_sdk::{
    firecracker::FirecrackerOption,
    models::{BootSource, Drive, InstanceInfo, InstanceState, MachineConfiguration},
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::app::{AppState, VmEntry};
use crate::error::ApiError;

const DEFAULT_VCPUS: u8 = 1;
const DEFAULT_MEMORY_MB: u32 = 128;
const MIN_MEMORY_MB: u32 = 128;
const MAX_VCPUS: u8 = 32;

#[derive(Debug, Deserialize)]
pub struct CreateVmRequest {
    /// Number of vCPUs. Must be 1 or an even number up to 32.
    pub vcpus: Option<u8>,
    /// Memory in megabytes. Must be at least 128.
    pub memory_mb: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct CreateVmResponse {
    pub id: Uuid,
    pub vcpus: u8,
    pub memory_mb: u32,
    pub state: InstanceState,
    pub vmm_version: String,
}

/// Validates the requested VM configuration and returns the resolved (vcpus, memory_mb).
fn validate_vm_config(req: &Option<CreateVmRequest>) -> Result<(u8, u32), ApiError> {
    let vcpus = req.as_ref().and_then(|r| r.vcpus).unwrap_or(DEFAULT_VCPUS);
    let memory_mb = req
        .as_ref()
        .and_then(|r| r.memory_mb)
        .unwrap_or(DEFAULT_MEMORY_MB);

    if vcpus == 0 || vcpus > MAX_VCPUS || (vcpus > 1 && vcpus % 2 != 0) {
        return Err(ApiError::unprocessable(
            "vcpus must be 1 or an even number between 2 and 32",
        ));
    }

    if memory_mb < MIN_MEMORY_MB {
        return Err(ApiError::unprocessable(format!(
            "memory_mb must be at least {}",
            MIN_MEMORY_MB,
        )));
    }

    Ok((vcpus, memory_mb))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    body: Option<Json<CreateVmRequest>>,
) -> Result<(StatusCode, Json<CreateVmResponse>), ApiError> {
    let req = body.map(|Json(r)| r);
    let (vcpus, memory_mb) = validate_vm_config(&req)?;

    let firecracker =
        std::env::var("FIRECRACKER_BIN").unwrap_or_else(|_| "firecracker".to_string());

    let vm_id = Uuid::new_v4();
    let socket_path = format!("/tmp/warlock-{}.socket", vm_id);
    let log_path = format!("/tmp/warlock-{}.log", vm_id);

    // Clean up any stale socket file from a previous run
    if std::path::Path::new(&socket_path).exists() {
        warn!(vm_id = %vm_id, "Removing stale socket file at {}", socket_path);
        let _ = std::fs::remove_file(&socket_path);
    }

    // Path to the kernel image
    const KERNEL: &str = "/opt/firecracker/vmlinux";

    // Path to the rootfs
    const ROOTFS: &str = "/opt/firecracker/rootfs.ext4";

    // Lock the VM map for the entire create operation to prevent race conditions
    // on the capacity check.
    let mut vms = state.vms.lock().await;

    // Check host capacity
    let allocated_vcpus: u8 = vms.values().map(|e| e.vcpus).sum();
    let allocated_memory: u64 = vms.values().map(|e| e.memory_mb as u64).sum();

    let available_vcpus = state.capacity.vcpus.saturating_sub(allocated_vcpus);
    let available_memory = state
        .capacity
        .allocatable_memory_mb()
        .saturating_sub(allocated_memory);

    if (vcpus as u64) > available_vcpus as u64 {
        return Err(ApiError::conflict(format!(
            "Insufficient vCPUs: requested {} but only {} available",
            vcpus, available_vcpus,
        )));
    }

    if (memory_mb as u64) > available_memory {
        return Err(ApiError::conflict(format!(
            "Insufficient memory: requested {} MB but only {} MB available",
            memory_mb, available_memory,
        )));
    }

    info!(vm_id = %vm_id, vcpus, memory_mb, "Creating VM instance");

    // Build an instance with desired options
    let mut instance = FirecrackerOption::new(firecracker)
        .api_sock(&socket_path)
        .id(vm_id.to_string())
        .log_path(Some(&log_path))
        .level("Info")
        .build()?;

    debug!(vm_id = %vm_id, "Firecracker socket instance created");

    // Start the firecracker process
    instance.start_vmm().await?;

    debug!(vm_id = %vm_id, "Firecracker VMM started");

    instance
        .put_machine_configuration(&MachineConfiguration {
            cpu_template: None,
            smt: None,
            mem_size_mib: memory_mb as isize,
            track_dirty_pages: None,
            vcpu_count: vcpus as isize,
            huge_pages: None,
        })
        .await?;

    debug!(vm_id = %vm_id, "Machine configuration applied");

    // Guest Boot Source
    instance
        .put_guest_boot_source(&BootSource {
            boot_args: Some("console=ttyS0 reboot=k panic=1 pci=off".into()),
            initrd_path: None,
            kernel_image_path: KERNEL.into(),
        })
        .await?;

    debug!(vm_id = %vm_id, "Boot source added");

    // Guest Drives
    instance
        .put_guest_drive_by_id(&Drive {
            drive_id: "rootfs".into(),
            partuuid: None,
            is_root_device: true,
            cache_type: None,
            is_read_only: false,
            path_on_host: ROOTFS.into(),
            rate_limiter: None,
            io_engine: None,
            socket: None,
        })
        .await?;

    debug!(vm_id = %vm_id, "Guest drive added");

    // Start the instance. This returns as soon as Firecracker accepts the
    // action — the guest OS may still be booting, so we return 202 Accepted.
    instance.start().await?;

    let desc = instance.describe_instance().await?;

    info!(vm_id = %vm_id, state = ?desc.state, "VM instance started");

    // Register the instance in state
    vms.insert(
        vm_id,
        VmEntry {
            instance,
            vcpus,
            memory_mb,
        },
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateVmResponse {
            id: vm_id,
            vcpus,
            memory_mb,
            state: desc.state,
            vmm_version: desc.vmm_version,
        }),
    ))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<InstanceInfo>, ApiError> {
    let mut vms = state.vms.lock().await;

    let entry = vms
        .get_mut(&id)
        .ok_or_else(|| ApiError::not_found("VM not found"))?;

    let desc = entry.instance.describe_instance().await?;

    Ok(Json(desc))
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut vms = state.vms.lock().await;

    let mut entry = vms
        .remove(&id)
        .ok_or_else(|| ApiError::not_found("VM not found"))?;

    info!(vm_id = %id, "Stopping VM instance");

    // Attempt graceful shutdown via Ctrl+Alt+Del
    if let Err(e) = entry.instance.stop().await {
        error!(vm_id = %id, error = ?e, "Graceful stop failed, instance will be force-terminated on drop");
    }

    // Instance is dropped here — the SDK's FStack sends SIGTERM and cleans up the socket file
    drop(entry);

    info!(vm_id = %id, "VM instance terminated and cleaned up");

    Ok(Json(serde_json::json!({ "id": id, "deleted": true })))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;

    fn req(vcpus: Option<u8>, memory_mb: Option<u32>) -> Option<CreateVmRequest> {
        Some(CreateVmRequest { vcpus, memory_mb })
    }

    // ── Defaults ──

    #[test]
    fn defaults_when_no_body() {
        let (vcpus, memory_mb) = validate_vm_config(&None).unwrap();
        assert_eq!(vcpus, 1);
        assert_eq!(memory_mb, 128);
    }

    #[test]
    fn defaults_when_fields_are_none() {
        let (vcpus, memory_mb) = validate_vm_config(&req(None, None)).unwrap();
        assert_eq!(vcpus, 1);
        assert_eq!(memory_mb, 128);
    }

    // ── Valid vCPU values ──

    #[test]
    fn accepts_1_vcpu() {
        let (vcpus, _) = validate_vm_config(&req(Some(1), None)).unwrap();
        assert_eq!(vcpus, 1);
    }

    #[test]
    fn accepts_even_vcpus() {
        for n in [2, 4, 8, 16, 32] {
            let (vcpus, _) = validate_vm_config(&req(Some(n), None)).unwrap();
            assert_eq!(vcpus, n);
        }
    }

    // ── Invalid vCPU values ──

    #[test]
    fn rejects_0_vcpus() {
        let err = validate_vm_config(&req(Some(0), None)).unwrap_err();
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn rejects_odd_vcpus_greater_than_1() {
        for n in [3, 5, 7, 15, 31] {
            let err = validate_vm_config(&req(Some(n), None)).unwrap_err();
            assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
        }
    }

    #[test]
    fn rejects_vcpus_over_32() {
        let err = validate_vm_config(&req(Some(34), None)).unwrap_err();
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    // ── Valid memory values ──

    #[test]
    fn accepts_minimum_memory() {
        let (_, memory_mb) = validate_vm_config(&req(None, Some(128))).unwrap();
        assert_eq!(memory_mb, 128);
    }

    #[test]
    fn accepts_large_memory() {
        let (_, memory_mb) = validate_vm_config(&req(None, Some(4096))).unwrap();
        assert_eq!(memory_mb, 4096);
    }

    // ── Invalid memory values ──

    #[test]
    fn rejects_memory_below_minimum() {
        let err = validate_vm_config(&req(None, Some(64))).unwrap_err();
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}
