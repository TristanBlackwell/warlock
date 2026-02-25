use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
};
use firecracker_rs_sdk::{
    firecracker::FirecrackerOption,
    models::{BootSource, Drive, InstanceInfo, MachineConfiguration},
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::app::AppState;
use crate::error::ApiError;

pub async fn create(State(state): State<Arc<AppState>>) -> Result<Json<InstanceInfo>, ApiError> {
    let firecracker =
        std::env::var("FIRECRACKER_BIN").unwrap_or_else(|_| "firecracker".to_string());

    let vm_id = Uuid::new_v4();
    let socket_path = format!("/tmp/warlock-{}.socket", vm_id);

    // Clean up any stale socket file from a previous run
    if std::path::Path::new(&socket_path).exists() {
        warn!(vm_id = %vm_id, "Removing stale socket file at {}", socket_path);
        let _ = std::fs::remove_file(&socket_path);
    }

    // Path to the kernel image
    const KERNEL: &str = "/foo/bar/vmlinux.bin";

    // Path to the rootfs
    const ROOTFS: &str = "/foo/bar/rootfs.ext4";

    info!(vm_id = %vm_id, "Creating VM instance");

    // Build an instance with desired options
    let mut instance = FirecrackerOption::new(firecracker)
        .api_sock(&socket_path)
        .id(vm_id.to_string())
        .build()?;

    debug!(vm_id = %vm_id, "Firecracker socket instance created");

    // Start the firecracker process
    instance.start_vmm().await?;

    debug!(vm_id = %vm_id, "Firecracker VMM started");

    instance
        .put_machine_configuration(&MachineConfiguration {
            cpu_template: None,
            smt: None,
            mem_size_mib: 124,
            track_dirty_pages: None,
            vcpu_count: 1,
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

    // Start the instance
    instance.start().await?;

    let desc = instance.describe_instance().await?;

    info!(vm_id = %vm_id, state = ?desc.state, "VM instance started");

    // Register the instance in state
    state.vms.lock().await.insert(vm_id, instance);

    Ok(Json(desc))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<InstanceInfo>, ApiError> {
    let mut vms = state.vms.lock().await;

    let instance = vms
        .get_mut(&id)
        .ok_or_else(|| ApiError::not_found("VM not found"))?;

    let desc = instance.describe_instance().await?;

    Ok(Json(desc))
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut vms = state.vms.lock().await;

    let mut instance = vms
        .remove(&id)
        .ok_or_else(|| ApiError::not_found("VM not found"))?;

    info!(vm_id = %id, "Stopping VM instance");

    // Attempt graceful shutdown via Ctrl+Alt+Del
    if let Err(e) = instance.stop().await {
        error!(vm_id = %id, error = ?e, "Graceful stop failed, instance will be force-terminated on drop");
    }

    // Instance is dropped here — the SDK's FStack sends SIGTERM and cleans up the socket file
    drop(instance);

    info!(vm_id = %id, "VM instance terminated and cleaned up");

    Ok(Json(serde_json::json!({ "id": id, "deleted": true })))
}
