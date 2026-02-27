# Architecture

This document describes how Warlock is structured and how it interacts with Firecracker to manage microVMs.

## Overview

Warlock is an HTTP API server (built with Axum) that creates, queries, and deletes Firecracker microVMs. It communicates with Firecracker through a forked Rust SDK (`firecracker-rs-sdk`) and runs each VM inside a jailer for security isolation.

```
Client  -->  Warlock (HTTP API)  -->  Jailer  -->  Firecracker VMM  -->  Guest OS
                   |
                   +-- AppState (in-memory VM registry)
```

## VM Lifecycle

### Creation (`POST /vm`)

1. **Validate** the requested configuration (vCPUs, memory) against constraints
2. **Copy rootfs** -- create a per-VM copy of the base rootfs image so each VM has its own writable disk
3. **Check capacity** -- verify the host has enough vCPUs and memory (no overcommit)
4. **Build jailer config** -- configure chroot, cgroups, PID namespace, uid/gid
5. **Start VMM** -- the jailer spawns Firecracker inside the chroot
6. **Configure VM** -- apply machine configuration, boot source, and guest drives via Firecracker's API socket
7. **Start instance** -- Firecracker begins executing the guest vCPU
8. **Register** -- store the `VmEntry` in the in-memory HashMap and return 202 Accepted

The 202 status reflects that Firecracker has accepted the start action but the guest OS may still be booting.

### Query (`GET /vm/{id}` and `GET /vm`)

Queries the Firecracker VMM via its API socket to get current instance state (`NotStarted`, `Running`, `Paused`). The list endpoint iterates all VMs and tolerates individual query failures.

### Deletion (`DELETE /vm/{id}`)

1. **Graceful stop** -- send Ctrl+Alt+Del to the guest via the Firecracker API
2. **Drop entry** -- the SDK's `FStack` destructor fires in LIFO order:
   - SIGTERM the Firecracker process
   - Remove the API socket file
   - Remove the jailer workspace directory
3. **Clean up rootfs copy** -- remove the per-VM rootfs file from `/srv/jailer/vm-images/`

### Shutdown (SIGTERM / Ctrl+C)

Warlock catches shutdown signals and iterates all registered VMs, performing the same stop + drop + rootfs cleanup sequence.

## Jailer Integration

Every VM runs inside Firecracker's [jailer](https://github.com/firecracker-microvm/firecracker/blob/main/docs/jailer.md), which provides:

- **Chroot** -- each VM gets its own filesystem root under `/srv/jailer/firecracker/{vm_id}/root/`
- **Non-root execution** -- Firecracker runs as uid/gid 1100 (`firecracker` system user)
- **PID namespace** -- each VM is isolated in its own PID namespace
- **Cgroups** -- CPU and memory limits enforced via cgroup v1 or v2 (auto-detected)
- **Hard links** -- the jailer hard-links the kernel binary into the chroot (symlinks are resolved first via `canonicalize()` since symlink targets would be outside the chroot)

### Assets Layout

```
/opt/firecracker/
  vmlinux      -> vmlinux-6.1.155     (symlink to kernel)
  rootfs.ext4  -> ubuntu-24.04.ext4   (symlink to base image)

/srv/jailer/
  firecracker/{vm_id}/root/           (jailer chroot per VM)
  vm-images/{vm_id}.ext4              (per-VM rootfs copy)
```

### Filesystem Requirements

Hard links cannot cross filesystem boundaries. Warlock validates at startup that `/opt/firecracker` and `/srv/jailer` are on the same device.

## Rootfs Strategy

Each VM needs its own writable root filesystem. Warlock creates per-VM copies with the best available strategy, detected at startup:

| Strategy | Filesystem | Mechanism |
|---|---|---|
| Reflink | btrfs, XFS | Instant copy-on-write clone (`cp --reflink=always`) |
| Sparse | ext4, others | Sparse copy that skips zero blocks (`cp --sparse=always`) |

Detection works by probing: Warlock creates a temporary file in `/srv/jailer/vm-images/` and attempts a reflink copy. If it succeeds, reflinks are used for all VMs; otherwise sparse copies.

Copies are stored at `/srv/jailer/vm-images/{vm_id}.ext4`, chowned to uid 1100, and cleaned up on VM deletion or Warlock shutdown.

See [ADR 001](decisions/001-writable-rootfs-strategy.md) for the full decision record.

## State Model

All VM state lives in an in-memory `HashMap<Uuid, VmEntry>` behind a `tokio::sync::Mutex`:

```rust
pub struct VmEntry {
    pub instance: Instance,  // SDK handle (Send but not Sync)
    pub vcpus: u8,
    pub memory_mb: u32,
    pub rootfs_copy: Option<PathBuf>,
}
```

The `Mutex` is required because the SDK's `Instance` struct (which holds `Child`, `Command`, `SocketAgent`, `FStack`) is `Send` but not `Sync`. The entire create operation holds the lock to prevent race conditions on capacity checks.

**Limitations**: If Warlock crashes, the in-memory state is lost but Firecracker processes and jailer workspaces remain on disk. Orphan detection on startup is planned but not yet implemented.

## Resource Management

Warlock prevents overcommitting host resources:

- **vCPUs** -- tracked and compared against available cores (no overcommit)
- **Memory** -- tracked with a 256 MB reservation for the host OS and Warlock itself
- **Cgroups** -- each VM gets CPU quota (100,000 us per vCPU) and a memory limit (requested + 50 MB overhead for the Firecracker process)

Requests that exceed available capacity are rejected with 409 Conflict.

## SDK Fork

Warlock uses a fork of the Firecracker Rust SDK at [`TristanBlackwell/firecracker-rs-sdk`](https://github.com/TristanBlackwell/firecracker-rs-sdk). The fork includes fixes for:

- **Process group isolation** -- `process_group(0)` so signals don't propagate to the child
- **Stdio defaults** -- `Stdio::null()` for stdin/stdout/stderr to prevent the child from inheriting Warlock's stdio
- **HTTP error handling** -- `ResponseTrait::decode` now checks HTTP status codes and extracts `fault_message` from error responses
- **Buffer fix** -- `recv_response` correctly handles response data across all three runtime implementations
- **Jailer workspace cleanup** -- `FStack` drop removes the entire jail directory, not just the `root/` subdirectory

## Preflight Checks

On startup (unless in dev mode), Warlock validates:

1. Firecracker binary exists and is >= v1.14.1
2. Jailer binary exists
3. KVM device is available (`/dev/kvm`)
4. `firecracker` system user (uid 1100) exists
5. `/opt/firecracker` and `/srv/jailer` are on the same filesystem
6. `/srv/jailer/vm-images/` directory exists
7. Cgroup version (v1 or v2)
8. Rootfs copy strategy (reflink or sparse)

## Project Structure

```
src/
  main.rs              -- entry point, shutdown handler
  lib.rs               -- module exports
  app.rs               -- Router, AppState, VmEntry
  error.rs             -- ApiError with IntoResponse
  capacity.rs          -- host capacity detection (sysinfo)
  logging.rs           -- tracing init
  handlers/
    mod.rs             -- handler module exports
    healthcheck.rs     -- enriched healthcheck (JSON)
    vm.rs              -- create/get/list/delete handlers
  firecracker/
    mod.rs             -- JailerConfig, CopyStrategy, constants
    preflight.rs       -- startup checks and detection
    version.rs         -- Firecracker version parsing
  vm/
    mod.rs             -- re-exports
    config.rs          -- validation, cgroup config
    rootfs.rs          -- per-VM rootfs copy/cleanup
tests/
  common/mod.rs        -- shared dev-mode test server + HTTP client
  healthcheck.rs       -- healthcheck integration tests
  vm.rs                -- VM API integration tests (dev mode)
  vm_lifecycle.rs      -- live integration tests (requires Firecracker)
```

## Testing

Tests are split into two tiers based on what infrastructure they require. See [ADR 002](decisions/002-test-strategy.md) for the full decision record.

### Tier 1: Fast Tests

Run everywhere (macOS, CI, provisioned hosts) via `cargo test`. The server starts in development mode (`WARLOCK_DEV=true`), skipping Firecracker preflight checks. Covers unit tests for domain logic and integration tests for HTTP error paths, validation, and response structure.

### Tier 2: Live Tests

Run only on hosts with Firecracker, KVM, and the full jailer layout via `make test-live`. Gated by the `WARLOCK_LIVE=true` environment variable. Covers the full VM lifecycle (create, get, list, delete), custom configurations, and healthcheck with running VMs.

```sh
# Fast tests (everywhere)
make test

# Live tests (provisioned host only)
make test-live
```
