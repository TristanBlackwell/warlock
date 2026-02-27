use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use firecracker_rs_sdk::{
    firecracker::FirecrackerOption,
    jailer::{ChrootStrategy, JailerOption},
    models::{BootSource, Drive, InstanceInfo, InstanceState, MachineConfiguration},
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::app::{AppState, VmEntry};
use crate::error::ApiError;
use crate::firecracker::{JAILER_GID, JAILER_UID, VM_IMAGES_DIR};
use crate::vm::config::{build_cgroup_config, validate_vm_config};
use crate::vm::rootfs::{cleanup_rootfs_copy, copy_rootfs};

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

#[derive(Debug, Serialize)]
pub struct VmSummary {
    pub id: Uuid,
    pub vcpus: u8,
    pub memory_mb: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<InstanceState>,
}

#[derive(Debug, Serialize)]
pub struct ListVmsResponse {
    pub vms: Vec<VmSummary>,
    pub count: usize,
}

pub async fn list(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListVmsResponse>, ApiError> {
    let mut vms = state.vms.lock().await;

    let mut summaries = Vec::with_capacity(vms.len());

    for (id, entry) in vms.iter_mut() {
        let instance_state = match entry.instance.describe_instance().await {
            Ok(info) => Some(info.state),
            Err(e) => {
                error!(vm_id = %id, error = ?e, "Failed to query VM state");
                None
            }
        };

        summaries.push(VmSummary {
            id: *id,
            vcpus: entry.vcpus,
            memory_mb: entry.memory_mb,
            state: instance_state,
        });
    }

    let count = summaries.len();
    Ok(Json(ListVmsResponse {
        vms: summaries,
        count,
    }))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    body: Option<Json<CreateVmRequest>>,
) -> Result<(StatusCode, Json<CreateVmResponse>), ApiError> {
    let req = body.map(|Json(r)| r);
    let (req_vcpus, req_memory) = match &req {
        Some(r) => (r.vcpus, r.memory_mb),
        None => (None, None),
    };

    let (vcpus, memory_mb) = validate_vm_config(req_vcpus, req_memory)
        .map_err(|e| ApiError::unprocessable(e.to_string()))?;

    let vm_id = Uuid::new_v4();

    // Resolve symlinks so the SDK hard-links the real files into the chroot
    // (symlink targets are outside the chroot and won't be visible to Firecracker).
    let kernel = std::fs::canonicalize("/opt/firecracker/vmlinux")
        .context("Failed to resolve kernel path")?;
    let base_rootfs = std::fs::canonicalize("/opt/firecracker/rootfs.ext4")
        .context("Failed to resolve rootfs path")?;

    // Create a per-VM copy of the rootfs so each VM has its own writable disk.
    // On reflink-capable filesystems this is instant; otherwise a sparse copy.
    let vm_rootfs = PathBuf::from(VM_IMAGES_DIR).join(format!("{}.ext4", vm_id));
    copy_rootfs(
        &state.jailer.copy_strategy,
        &base_rootfs,
        &vm_rootfs,
        JAILER_UID,
        JAILER_GID,
    )?;

    debug!(vm_id = %vm_id, "Rootfs copied to {}", vm_rootfs.display());

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

    // Configure Firecracker options (passed through to firecracker via jailer's --)
    let mut fc_opts = FirecrackerOption::new(&state.jailer.firecracker_path);
    fc_opts.log_path(Some("firecracker.log")).level("Info");

    // Build cgroup configuration for resource isolation
    let cgroups = build_cgroup_config(state.jailer.cgroup_version, vcpus, memory_mb);

    // Route jailer stderr to a per-VM log file so errors aren't silently swallowed
    let jailer_stderr = format!("/tmp/warlock-jailer-{}.log", vm_id);

    // Build a jailed instance
    let mut instance = JailerOption::new(
        &state.jailer.jailer_path,
        &state.jailer.firecracker_path,
        vm_id.to_string(),
        JAILER_GID,
        JAILER_UID,
    )
    .firecracker_option(Some(&fc_opts))
    .chroot_strategy(ChrootStrategy::NaiveLinkStrategy)
    .new_pid_ns(Some(true))
    .cgroup_version(Some(state.jailer.cgroup_version))
    .cgroup(cgroups)
    .stderr(&jailer_stderr)
    .remove_jailer_workspace_dir()
    .build()?;

    debug!(vm_id = %vm_id, "Jailed Firecracker instance created");

    // Start the jailer process (which spawns firecracker inside the chroot)
    instance.start_vmm().await?;

    debug!(vm_id = %vm_id, "Jailed Firecracker VMM started");

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

    // Guest Boot Source — the SDK hard-links the kernel into the chroot and
    // rewrites the path for Firecracker automatically.
    instance
        .put_guest_boot_source(&BootSource {
            boot_args: Some("console=ttyS0 reboot=k panic=1 pci=off".into()),
            initrd_path: None,
            kernel_image_path: kernel.clone(),
        })
        .await?;

    debug!(vm_id = %vm_id, "Boot source added");

    // Guest Drives — each VM gets its own writable rootfs copy.
    instance
        .put_guest_drive_by_id(&Drive {
            drive_id: "rootfs".into(),
            partuuid: None,
            is_root_device: true,
            cache_type: None,
            is_read_only: false,
            path_on_host: vm_rootfs.clone(),
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
            rootfs_copy: Some(vm_rootfs),
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

    // Capture the rootfs copy path before dropping the entry
    let rootfs_copy = entry.rootfs_copy.clone();

    // Entry is dropped here — the SDK's FStack sends SIGTERM to the Firecracker
    // process and cleans up the socket file and jailer workspace directory.
    drop(entry);

    // Clean up the per-VM rootfs copy (outside the jailer workspace)
    if let Some(ref path) = rootfs_copy {
        cleanup_rootfs_copy(&id, path);
    }

    info!(vm_id = %id, "VM instance terminated and cleaned up");

    Ok(Json(serde_json::json!({ "id": id, "deleted": true })))
}
