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
    instance::Instance,
    jailer::{ChrootStrategy, JailerOption},
    models::{
        BootSource, Drive, InstanceInfo, InstanceState, MachineConfiguration, NetworkInterface,
        Vsock,
    },
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::app::{AppState, VmEntry, VmResources};
use crate::error::ApiError;
use crate::firecracker::network::{add_nat_rules, create_tap, delete_tap, remove_nat_rules};
use crate::firecracker::{JAILER_GID, JAILER_UID};
use crate::vm::config::{build_cgroup_config, validate_vm_config};
use crate::vm::network::build_network_boot_args;
use crate::vm::rootfs::{cleanup_rootfs_copy, copy_rootfs};

#[derive(Debug, Deserialize)]
pub struct CreateVmRequest {
    /// Number of vCPUs. Must be 1 or an even number up to 32.
    pub vcpus: Option<u8>,
    /// Memory in megabytes. Must be at least 128.
    pub memory_mb: Option<u32>,
    /// SSH public keys authorized to access this VM's console.
    /// Each key must be in OpenSSH format (e.g., "ssh-ed25519 AAAA...").
    /// If not provided, the VM will have no SSH access.
    #[serde(default)]
    pub ssh_keys: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateVmResponse {
    pub id: Uuid,
    pub vcpus: u8,
    pub memory_mb: u32,
    pub state: InstanceState,
    pub vmm_version: String,
    pub guest_ip: String,
}

#[derive(Debug, Serialize)]
pub struct VmSummary {
    pub id: Uuid,
    pub vcpus: u8,
    pub memory_mb: u32,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<InstanceState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guest_ip: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ListVmsResponse {
    pub vms: Vec<VmSummary>,
    pub count: usize,
}

pub async fn list(State(state): State<Arc<AppState>>) -> Result<Json<ListVmsResponse>, ApiError> {
    let mut vms = state.vms.lock().await;

    let mut summaries = Vec::with_capacity(vms.len());

    for (id, entry) in vms.iter_mut() {
        let instance_state = match entry {
            VmEntry::Running { instance, .. } => match instance.describe_instance().await {
                Ok(info) => Some(info.state),
                Err(e) => {
                    error!(vm_id = %id, error = ?e, "Failed to query VM state");
                    None
                }
            },
            VmEntry::Creating(_) => None,
        };

        let r = entry.resources();
        summaries.push(VmSummary {
            id: *id,
            vcpus: r.vcpus,
            memory_mb: r.memory_mb,
            status: entry.status(),
            state: instance_state,
            guest_ip: r.guest_ip.clone(),
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

    // ── Blocking I/O: copy rootfs ──
    // The rootfs copy shells out to `cp` and `chown` which can take seconds
    // on a slow disk. Running them on the tokio runtime thread would block
    // all other requests, so we offload to a blocking thread.
    let kernel = state.jailer.kernel_path.clone();
    let base_rootfs = state.jailer.rootfs_path.clone();
    let copy_strategy = state.jailer.copy_strategy.clone();
    let vm_rootfs = state.jailer.vm_images_dir.join(format!("{}.ext4", vm_id));
    let vm_rootfs_clone = vm_rootfs.clone();

    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        copy_rootfs(
            &copy_strategy,
            &base_rootfs,
            &vm_rootfs_clone,
            JAILER_UID,
            JAILER_GID,
        )
    })
    .await
    .context("Rootfs copy task panicked")?
    .context("Failed to prepare rootfs")?;

    debug!(vm_id = %vm_id, "Rootfs copied to {}", vm_rootfs.display());

    // ── Networking: allocate subnet ──
    let subnet = {
        let mut pool = state.subnet_pool.lock().await;
        pool.allocate()
            .ok_or_else(|| ApiError::conflict("No available network subnets"))?
    };

    debug!(vm_id = %vm_id, tap = %subnet.tap_name, guest_ip = %subnet.guest_ip, "Subnet allocated");

    // ── Networking: create tap + NAT rules ──
    // These shell out to `ip` and `nft` — fast commands but still blocking I/O.
    let tap_name = subnet.tap_name.clone();
    let tap_ip = subnet.tap_ip;
    let guest_ip_addr = subnet.guest_ip;
    let host_iface = state.jailer.host_interface.clone();

    let nat_result = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        create_tap(&tap_name, &tap_ip)?;

        match add_nat_rules(&guest_ip_addr, &tap_name, &host_iface) {
            Ok(handles) => Ok(handles),
            Err(e) => {
                // Roll back: delete tap
                delete_tap(&tap_name);
                Err(e)
            }
        }
    })
    .await
    .context("Networking setup task panicked")?;

    let nat_handles = match nat_result {
        Ok(handles) => handles,
        Err(e) => {
            // Roll back subnet allocation
            state.subnet_pool.lock().await.release(subnet.index);
            return Err(ApiError::from(e));
        }
    };

    // ── Reserve capacity: insert placeholder ──
    // Lock the VM map briefly to check capacity and insert a Creating
    // placeholder. This reserves resources so concurrent creates can't
    // overcommit, but releases the lock immediately so other handlers
    // (list, get, delete) aren't blocked during the slow SDK setup.
    let guest_ip = subnet.guest_ip.to_string();

    // Build vsock UDS paths:
    //  - placeholder_path: temporary file the SDK hard-links into the chroot
    //  - runtime_path: where Firecracker actually creates the socket (inside the chroot)
    //
    // The SDK's NaiveLinkStrategy extracts the filename from the placeholder path,
    // hard-links it into the jailer workspace dir, then passes the relative filename
    // to Firecracker. Firecracker creates the real socket at that relative path
    // inside its chroot.
    let vsock_filename = format!("{}.sock", vm_id);
    let vsock_placeholder_path = PathBuf::from("/srv/jailer/vsock").join(&vsock_filename);
    let vsock_runtime_path = PathBuf::from(format!(
        "/srv/jailer/firecracker/{}/root/{}",
        vm_id, vsock_filename
    ));

    {
        let mut vms = state.vms.lock().await;

        // Check host capacity (counts both Creating and Running entries)
        let allocated_vcpus: u8 = vms.values().map(|e| e.resources().vcpus).sum();
        let allocated_memory: u64 = vms.values().map(|e| e.resources().memory_mb as u64).sum();

        let available_vcpus = state.capacity.vcpus.saturating_sub(allocated_vcpus);
        let available_memory = state
            .capacity
            .allocatable_memory_mb()
            .saturating_sub(allocated_memory);

        if (vcpus as u64) > available_vcpus as u64 {
            // Roll back networking
            remove_nat_rules(&nat_handles);
            delete_tap(&subnet.tap_name);
            state.subnet_pool.lock().await.release(subnet.index);
            return Err(ApiError::conflict(format!(
                "Insufficient vCPUs: requested {} but only {} available",
                vcpus, available_vcpus,
            )));
        }

        if (memory_mb as u64) > available_memory {
            // Roll back networking
            remove_nat_rules(&nat_handles);
            delete_tap(&subnet.tap_name);
            state.subnet_pool.lock().await.release(subnet.index);
            return Err(ApiError::conflict(format!(
                "Insufficient memory: requested {} MB but only {} MB available",
                memory_mb, available_memory,
            )));
        }

        // Insert Creating placeholder — reserves capacity in the map.
        // Store the runtime UDS path so the console handler can find the socket.
        vms.insert(
            vm_id,
            VmEntry::Creating(VmResources {
                vcpus,
                memory_mb,
                rootfs_copy: Some(vm_rootfs.clone()),
                tap_name: Some(subnet.tap_name.clone()),
                subnet_index: Some(subnet.index),
                nat_handles: Some(nat_handles),
                guest_ip: Some(guest_ip.clone()),
                vsock_uds_path: Some(vsock_runtime_path),
                ssh_keys: req.as_ref().map(|r| r.ssh_keys.clone()).unwrap_or_default(),
            }),
        );

        // Lock is released here
    }

    info!(vm_id = %vm_id, vcpus, memory_mb, "Creating VM instance");

    // ── Panic guard ──
    // If the handler panics during the SDK calls below, the CatchPanicLayer
    // will catch it and return 500 — but the placeholder entry would remain
    // in the map forever. This guard removes it on drop if not defused.
    let mut guard = CreateGuard::new(state.clone(), vm_id);

    // ── Firecracker setup (lock NOT held) ──
    let instance_result = setup_firecracker_instance(
        &state,
        vm_id,
        vcpus,
        memory_mb,
        &kernel,
        &vm_rootfs,
        &subnet.tap_name,
        &subnet.guest_ip,
        &subnet.tap_ip,
        &vsock_placeholder_path,
    )
    .await;

    let (instance, desc) = match instance_result {
        Ok(result) => result,
        Err(e) => {
            // Explicit cleanup: remove placeholder and roll back resources.
            // We can use .await here (unlike Drop), so this is more reliable
            // than the guard for normal error paths.
            let mut vms = state.vms.lock().await;
            if let Some(entry) = vms.remove(&vm_id) {
                cleanup_vm_resources(&state, &vm_id, entry).await;
            }
            guard.defuse();
            return Err(ApiError::from(e));
        }
    };

    // ── Upgrade placeholder to Running ──
    {
        let mut vms = state.vms.lock().await;

        // Extract the resources from the Creating placeholder
        if let Some(VmEntry::Creating(resources)) = vms.remove(&vm_id) {
            vms.insert(
                vm_id,
                VmEntry::Running {
                    instance,
                    resources,
                },
            );
        }
        // If the entry was somehow removed (shouldn't happen — delete
        // rejects Creating VMs), the Instance is dropped here and the
        // FStack cleans up the Firecracker process.
    }

    guard.defuse();

    info!(vm_id = %vm_id, state = ?desc.state, guest_ip = %guest_ip, "VM instance started");

    // Report to gateway
    if let Some(ref client) = state.gateway_client
        && let Err(e) = client.register_vm(vm_id).await
    {
        warn!("Failed to register VM {} with gateway: {:#}", vm_id, e);
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateVmResponse {
            id: vm_id,
            vcpus,
            memory_mb,
            state: desc.state,
            vmm_version: desc.vmm_version,
            guest_ip,
        }),
    ))
}

/// Builds, configures, and starts a jailed Firecracker instance.
///
/// This is the slow part of VM creation — it spawns the jailer process,
/// configures the VMM via the API socket, and starts the guest. It runs
/// **without** holding the vms lock.
async fn setup_firecracker_instance(
    state: &Arc<AppState>,
    vm_id: Uuid,
    vcpus: u8,
    memory_mb: u32,
    kernel: &std::path::Path,
    vm_rootfs: &std::path::Path,
    tap_name: &str,
    guest_ip: &std::net::Ipv4Addr,
    tap_ip: &std::net::Ipv4Addr,
    vsock_placeholder_path: &std::path::Path,
) -> anyhow::Result<(Instance, InstanceInfo)> {
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
    // Boot args include the kernel ip= parameter for automatic network config.
    let network_boot_args = build_network_boot_args(guest_ip, tap_ip);
    let boot_args = format!(
        "console=ttyS0 reboot=k panic=1 pci=off {}",
        network_boot_args
    );

    instance
        .put_guest_boot_source(&BootSource {
            boot_args: Some(boot_args),
            initrd_path: None,
            kernel_image_path: kernel.to_path_buf(),
        })
        .await?;

    debug!(vm_id = %vm_id, "Boot source added");

    // Guest Network Interface — attach the tap device to the VM.
    instance
        .put_guest_network_interface_by_id(&NetworkInterface {
            iface_id: "eth0".into(),
            host_dev_name: PathBuf::from(tap_name),
            guest_mac: None,
            rx_rate_limiter: None,
            tx_rate_limiter: None,
        })
        .await?;

    debug!(vm_id = %vm_id, tap = tap_name, "Network interface added");

    // Guest Drives — each VM gets its own writable rootfs copy.
    instance
        .put_guest_drive_by_id(&Drive {
            drive_id: "rootfs".into(),
            partuuid: None,
            is_root_device: true,
            cache_type: None,
            is_read_only: false,
            path_on_host: vm_rootfs.to_path_buf(),
            rate_limiter: None,
            io_engine: None,
            socket: None,
        })
        .await?;

    debug!(vm_id = %vm_id, "Guest drive added");

    // Guest vsock device — enables host-to-guest communication for console access.
    //
    // The SDK's NaiveLinkStrategy computes a relative path from the UDS path we
    // provide and sends it to Firecracker. Firecracker bind()s a Unix socket at
    // that relative path inside its chroot. Unlike kernel/rootfs/drives, the vsock
    // UDS is created by Firecracker at runtime, not a pre-existing file.
    //
    // With NaiveLinkStrategy, the SDK extracts the filename from vsock_placeholder_path
    // and Firecracker creates the socket at: /srv/jailer/firecracker/{vm_id}/root/{vm_id}.sock
    //
    // Guest CID is derived from the VM ID, ensuring it's >= 3 (0-2 are reserved).
    let guest_cid = (vm_id.as_u128() as u32 & 0x7FFFFFFF) | 0x00000003;

    instance
        .put_guest_vsock(&Vsock {
            guest_cid,
            uds_path: vsock_placeholder_path.to_path_buf(),
            vsock_id: Some("vsock0".to_string()),
        })
        .await
        .context("Failed to configure vsock device")?;

    debug!(vm_id = %vm_id, guest_cid, "vsock device configured");

    // Start the instance. This returns as soon as Firecracker accepts the
    // action — the guest OS may still be booting, so we return 202 Accepted.
    instance.start().await?;

    let desc = instance.describe_instance().await?;

    Ok((instance, desc))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<InstanceInfo>, ApiError> {
    let mut vms = state.vms.lock().await;

    let entry = vms
        .get_mut(&id)
        .ok_or_else(|| ApiError::not_found("VM not found"))?;

    match entry {
        VmEntry::Creating(_) => Err(ApiError::conflict("VM is still being created")),
        VmEntry::Running { instance, .. } => {
            let desc = instance.describe_instance().await?;
            Ok(Json(desc))
        }
    }
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut vms = state.vms.lock().await;

    let entry = vms
        .remove(&id)
        .ok_or_else(|| ApiError::not_found("VM not found"))?;

    // Cannot delete a VM that is still being created — the create handler
    // is in the middle of setting up the Firecracker instance without
    // holding the lock. Put the entry back and tell the client to retry.
    if matches!(&entry, VmEntry::Creating(_)) {
        vms.insert(id, entry);
        return Err(ApiError::conflict(
            "VM is still being created, try again shortly",
        ));
    }

    // We know it's Running — destructure to get the instance
    let VmEntry::Running {
        mut instance,
        resources,
    } = entry
    else {
        unreachable!();
    };

    info!(vm_id = %id, "Stopping VM instance");

    // Attempt graceful shutdown via Ctrl+Alt+Del
    if let Err(e) = instance.stop().await {
        error!(vm_id = %id, error = ?e, "Graceful stop failed, instance will be force-terminated on drop");
    }

    // Instance is dropped here — the SDK's FStack sends SIGTERM to the
    // Firecracker process and cleans up the socket file and jailer workspace.
    drop(instance);

    // Clean up networking: tap device, NAT rules, subnet allocation
    if let Some(ref name) = resources.tap_name {
        delete_tap(name);
    }
    if let Some(ref handles) = resources.nat_handles {
        remove_nat_rules(handles);
    }
    if let Some(index) = resources.subnet_index {
        state.subnet_pool.lock().await.release(index);
    }

    // Clean up the per-VM rootfs copy (outside the jailer workspace)
    if let Some(ref path) = resources.rootfs_copy {
        cleanup_rootfs_copy(&id, path);
    }

    // Report to gateway
    if let Some(ref client) = state.gateway_client
        && let Err(e) = client.deregister_vm(id).await
    {
        warn!("Failed to deregister VM {} from gateway: {:#}", id, e);
    }

    info!(vm_id = %id, "VM instance terminated and cleaned up");

    Ok(Json(serde_json::json!({ "id": id, "deleted": true })))
}

// ── Helpers ──

/// Cleans up all resources associated with a VM entry (either Creating or
/// Running). Used by the create handler's error path and the shutdown handler.
async fn cleanup_vm_resources(state: &Arc<AppState>, vm_id: &Uuid, entry: VmEntry) {
    let resources = match entry {
        VmEntry::Running {
            mut instance,
            resources,
        } => {
            if let Err(e) = instance.stop().await {
                error!(vm_id = %vm_id, error = ?e, "Graceful stop failed");
            }
            drop(instance);
            resources
        }
        VmEntry::Creating(resources) => resources,
    };

    if let Some(ref name) = resources.tap_name {
        delete_tap(name);
    }
    if let Some(ref handles) = resources.nat_handles {
        remove_nat_rules(handles);
    }
    if let Some(index) = resources.subnet_index {
        state.subnet_pool.lock().await.release(index);
    }
    if let Some(ref path) = resources.rootfs_copy {
        cleanup_rootfs_copy(vm_id, path);
    }
}

/// Ensures the `Creating` placeholder is removed from the VM map if the
/// create handler panics between the two lock windows.
///
/// On normal error paths, the handler does explicit cleanup with `.await`
/// (more reliable). This guard is a safety net for panics only, where we
/// can't use async code (Drop is synchronous).
struct CreateGuard {
    state: Arc<AppState>,
    vm_id: Uuid,
    defused: bool,
}

impl CreateGuard {
    fn new(state: Arc<AppState>, vm_id: Uuid) -> Self {
        Self {
            state,
            vm_id,
            defused: false,
        }
    }

    /// Disarms the guard. Call this after successful upgrade to Running or
    /// after explicit error cleanup.
    fn defuse(&mut self) {
        self.defused = true;
    }
}

impl Drop for CreateGuard {
    fn drop(&mut self) {
        if self.defused {
            return;
        }

        warn!(
            vm_id = %self.vm_id,
            "Create guard triggered — cleaning up placeholder entry (probable panic)"
        );

        // try_lock because Drop is sync and we can't .await. If the lock is
        // contended (extremely unlikely during a panic), the placeholder
        // persists until restart — orphan cleanup will catch it.
        if let Ok(mut vms) = self.state.vms.try_lock() {
            if let Some(VmEntry::Creating(resources)) = vms.get(&self.vm_id) {
                // Best-effort cleanup of networking resources. Rootfs and
                // jailer workspace will be caught by orphan cleanup on restart.
                if let Some(ref name) = resources.tap_name {
                    delete_tap(name);
                }
                if let Some(ref handles) = resources.nat_handles {
                    remove_nat_rules(handles);
                }
                if let Some(index) = resources.subnet_index
                    && let Ok(mut pool) = self.state.subnet_pool.try_lock()
                {
                    pool.release(index);
                }
                if let Some(ref path) = resources.rootfs_copy {
                    let _ = std::fs::remove_file(path);
                }
            }
            vms.remove(&self.vm_id);
        }
    }
}
