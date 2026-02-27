# ADR-001: Writable Rootfs Strategy

**Status:** Accepted  
**Date:** 2026-02-27  
**Context:** VM rootfs lifecycle management in a Firecracker jailer environment

## Context

Warlock manages Firecracker microVMs through the jailer, which isolates each VM
inside a chroot. The jailer uses hard links to place host files (kernel, rootfs)
into the chroot. This creates a problem for writable root filesystems:

- The base rootfs image (`/opt/firecracker/ubuntu-24.04.ext4`) is shared across
  all VMs
- Hard links share the same inode — writes from one VM would corrupt the image
  for all others
- The Firecracker process runs as uid 1100 (unprivileged) and cannot write to
  root-owned files

VMs need a writable root filesystem to function properly (logging, temp files,
networking configuration, package installation, etc.).

## Decision

Use **per-VM rootfs copies** with automatic detection of the best copy strategy
based on the host filesystem's capabilities.

### Copy Strategy Detection

```
                  Preflight Check
                       |
                       v
            Create temp file in /srv/jailer/
                       |
                       v
           Attempt cp --reflink=always
                    /       \
                   /         \
              Success       Failure
                 |             |
                 v             v
            Reflink        Sparse
         (btrfs/XFS)    (ext4/other)
```

At startup, Warlock probes the filesystem by attempting a reflink copy of a
small test file. The result is stored in `JailerConfig` and used for all
subsequent VM creations.

### VM Lifecycle

```
  POST /vm
     |
     v
  Resolve base rootfs (canonicalize symlinks)
     |
     v
  Copy rootfs to /srv/jailer/vm-images/{vm_id}.ext4
     |  (reflink: instant | sparse: seconds)
     |
     v
  chown {uid}:{gid} the copy
     |
     v
  Start jailer (creates chroot)
     |
     v
  SDK hard-links per-VM copy into chroot
     |
     v
  Firecracker opens rootfs read-write
     |
     v
  VM is running with its own writable rootfs
     |
     ...
     |
  DELETE /vm/{id}  or  Ctrl+C shutdown
     |
     v
  Stop Firecracker (graceful shutdown)
     |
     v
  SDK FStack cleanup:
     1. SIGTERM Firecracker process
     2. Remove API socket
     3. Remove jailer workspace (/srv/jailer/firecracker/{id}/)
     |
     v
  Warlock cleanup:
     4. Remove rootfs copy (/srv/jailer/vm-images/{vm_id}.ext4)
```

### Directory Layout

```
/opt/firecracker/
  vmlinux -> vmlinux-6.1.155          # symlink to kernel
  vmlinux-6.1.155                     # actual kernel binary
  rootfs.ext4 -> ubuntu-24.04.ext4    # symlink to base image
  ubuntu-24.04.ext4                   # base rootfs (shared, read-only)

/srv/jailer/
  vm-images/                          # per-VM rootfs copies
    {vm_id_1}.ext4                    # writable copy for VM 1
    {vm_id_2}.ext4                    # writable copy for VM 2
  firecracker/                        # jailer chroot workspaces
    {vm_id_1}/
      root/                           # chroot root
        vmlinux-6.1.155               # hard-link to kernel
        {vm_id_1}.ext4               # hard-link to per-VM copy
        run/firecracker.socket        # API socket
        firecracker.pid
    {vm_id_2}/
      root/
        ...
```

## Alternatives Considered

### 1. Read-only rootfs (current state)

Keep the rootfs read-only and accept the limitation.

- **Pro:** Zero copy overhead, instant startup
- **Con:** VMs cannot write to disk at all — no logging, no temp files, no
  networking configuration, no package installation. Effectively unusable for
  real workloads.
- **Verdict:** Not viable for production use.

### 2. Read-only rootfs + writable scratch disk

Keep the base rootfs read-only, attach a second small writable drive as a
scratch disk. Guest configures mount points (e.g., `/var`, `/tmp`) on the
scratch disk.

- **Pro:** Fast (scratch disk is a small sparse file), no base image copying
- **Con:** Pushes complexity into the guest image — requires custom init
  scripts, applications must be aware of writable paths, some programs expect
  `/` to be writable. Non-standard guest configuration.
- **Verdict:** Workable but non-idiomatic. Creates coupling between Warlock
  and guest image configuration.

### 3. Overlayfs

Mount an overlayfs with the base image as the read-only lower layer and a
per-VM directory as the writable upper layer.

- **Pro:** Instant, efficient (only delta is stored), standard Linux mechanism
- **Con:** Overlayfs operates on directory trees, not block device images.
  Firecracker requires raw disk images. Would need to loop-mount the base
  image, overlay it, then re-export as a block device (FUSE/NBD) — adding
  latency and significant complexity.
- **Verdict:** Not compatible with Firecracker's raw image requirement without
  significant indirection.

### 4. Device-mapper thin provisioning

Create a dm-thin pool, import the base image, create thin snapshots per VM.

- **Pro:** Block-level CoW, instant snapshots, used by AWS Lambda internally
- **Con:** Complex setup (thin pool creation, snapshot management),
  operational overhead, requires specific tooling, harder to debug
- **Verdict:** Viable at scale but over-engineered for Warlock's current scope.

### 5. Mandate btrfs/XFS

Require the host to use a reflink-capable filesystem.

- **Pro:** Simplest code (just `cp --reflink=always`), instant copies
- **Con:** Dictates infrastructure choices to operators, many Linux systems
  default to ext4, migration is disruptive
- **Verdict:** Too prescriptive. Instead, detect and prefer reflinks when
  available, fall back gracefully.

## Consequences

### Positive

- VMs get a fully writable root filesystem — standard guest images work
  without modification
- Instant startup on reflink-capable filesystems (btrfs, XFS)
- Graceful degradation on ext4 (sparse copy — slower but functional)
- Automatic detection — no operator configuration needed
- Clean separation: base image is never modified, per-VM state is isolated
- Cleanup is deterministic (delete handler + graceful shutdown)

### Negative

- On ext4, rootfs copy adds startup latency proportional to image size
  (~1GB image ≈ a few seconds on SSD)
- Requires Warlock to run as root (for `chown` of the copy to uid 1100) —
  but this is already required for the jailer
- Additional disk usage: one copy per running VM (mitigated by reflinks
  on capable filesystems)

### Operational Recommendations

For production deployments prioritising fast startup:

- Use **btrfs** or **XFS with reflinks** on the partition containing
  `/srv/jailer/`. This gives instant CoW copies.
- On Ubuntu: `mkfs.btrfs /dev/sdX && mount /dev/sdX /srv/jailer`
- On systems where filesystem changes are not possible, ext4 with sparse
  copies is functional but slower.
