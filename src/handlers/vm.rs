use std::sync::Arc;

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
use crate::firecracker::{JAILER_GID, JAILER_UID};

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

/// Builds the cgroup configuration for a jailed VM based on the detected
/// cgroup version and requested resources.
fn build_cgroup_config(cgroup_version: usize, vcpus: u8, memory_mb: u32) -> Vec<(String, String)> {
    // Memory limit: VM allocation + 50 MB overhead for the Firecracker process
    let memory_limit_bytes = ((memory_mb as u64) + 50) * 1024 * 1024;
    // CPU quota: 100% of one physical core per vCPU (100_000 us per 100_000 us period)
    let cpu_quota = (vcpus as u64) * 100_000;

    match cgroup_version {
        2 => vec![
            ("cpu.max".into(), format!("{} 100000", cpu_quota)),
            ("memory.max".into(), memory_limit_bytes.to_string()),
        ],
        _ => vec![
            ("cpu.cfs_quota_us".into(), cpu_quota.to_string()),
            ("cpu.cfs_period_us".into(), "100000".into()),
            (
                "memory.limit_in_bytes".into(),
                memory_limit_bytes.to_string(),
            ),
        ],
    }
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    body: Option<Json<CreateVmRequest>>,
) -> Result<(StatusCode, Json<CreateVmResponse>), ApiError> {
    let req = body.map(|Json(r)| r);
    let (vcpus, memory_mb) = validate_vm_config(&req)?;

    let vm_id = Uuid::new_v4();

    // Path to the kernel image (on the host — the SDK hard-links it into the chroot)
    const KERNEL: &str = "/opt/firecracker/vmlinux";

    // Path to the rootfs (on the host — the SDK hard-links it into the chroot)
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
            kernel_image_path: KERNEL.into(),
        })
        .await?;

    debug!(vm_id = %vm_id, "Boot source added");

    // Guest Drives — same automatic linking as the boot source.
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

    // Entry is dropped here — the SDK's FStack sends SIGTERM to the Firecracker
    // process and cleans up the socket file and jailer workspace directory.
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

    // ── Cgroup configuration ──

    #[test]
    fn cgroup_v2_config() {
        let cgroups = build_cgroup_config(2, 1, 128);
        assert_eq!(cgroups.len(), 2);
        // 1 vCPU = 100_000 us quota per 100_000 us period
        assert_eq!(cgroups[0], ("cpu.max".into(), "100000 100000".into()));
        // 128 MB + 50 MB overhead = 178 MB in bytes
        let expected_mem = ((128u64 + 50) * 1024 * 1024).to_string();
        assert_eq!(cgroups[1], ("memory.max".into(), expected_mem));
    }

    #[test]
    fn cgroup_v2_config_multi_vcpu() {
        let cgroups = build_cgroup_config(2, 4, 256);
        // 4 vCPUs = 400_000 us quota
        assert_eq!(cgroups[0], ("cpu.max".into(), "400000 100000".into()));
        let expected_mem = ((256u64 + 50) * 1024 * 1024).to_string();
        assert_eq!(cgroups[1], ("memory.max".into(), expected_mem));
    }

    #[test]
    fn cgroup_v1_config() {
        let cgroups = build_cgroup_config(1, 2, 256);
        assert_eq!(cgroups.len(), 3);
        assert_eq!(
            cgroups[0],
            ("cpu.cfs_quota_us".into(), "200000".into())
        );
        assert_eq!(
            cgroups[1],
            ("cpu.cfs_period_us".into(), "100000".into())
        );
        let expected_mem = ((256u64 + 50) * 1024 * 1024).to_string();
        assert_eq!(
            cgroups[2],
            ("memory.limit_in_bytes".into(), expected_mem)
        );
    }
}
