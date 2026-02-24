use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
};
use firecracker_rs_sdk::{
    firecracker::FirecrackerOption,
    models::{BootSource, Drive, InstanceInfo, MachineConfiguration},
};
use tracing::debug;
use uuid::Uuid;

use crate::app::AppState;
use crate::error::ApiError;

pub async fn create(State(_state): State<Arc<AppState>>) -> Result<Json<InstanceInfo>, ApiError> {
    let firecracker =
        std::env::var("FIRECRACKER_BIN").unwrap_or_else(|_| "firecracker".to_string());

    // Path at which you want to place the socket at
    const API_SOCK: &str = "/tmp/firecracker.socket";

    // Path to the kernel image
    const KERNEL: &str = "/foo/bar/vmlinux.bin";

    // Path to the rootfs
    const ROOTFS: &str = "/foo/bar/rootfs.ext4";

    // Build an instance with desired options
    let mut instance = FirecrackerOption::new(firecracker)
        .api_sock(API_SOCK)
        .id("test-instance")
        .build()?;

    debug!("Firecracker socket instance created");

    // First start the `firecracker` process
    instance.start_vmm().await?;

    debug!("Firecracker socket instance started");

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

    debug!("Machine configuration applied");

    // Guest Boot Source
    instance
        .put_guest_boot_source(&BootSource {
            boot_args: Some("console=ttyS0 reboot=k panic=1 pci=off".into()),
            initrd_path: None,
            kernel_image_path: KERNEL.into(),
        })
        .await?;

    debug!("Boot source added");

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

    debug!("Guest drive added");
    debug!("Starting instance...");

    // Start the instance
    instance.start().await?;

    debug!("Instance started");

    let desc = instance.describe_instance().await?;

    debug!("Instance details: {:#?}", desc);

    Ok(Json(desc))
}

pub async fn get(
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<Uuid>,
) -> Result<Json<InstanceInfo>, ApiError> {
    todo!()
}

pub async fn delete(
    State(_state): State<Arc<AppState>>,
    Path(_id): Path<Uuid>,
) -> Result<Json<InstanceInfo>, ApiError> {
    todo!()
}
