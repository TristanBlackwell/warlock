# Warlock

Warlock is an experimental control plane for [Firecracker](https://github.com/firecracker-microvm/firecracker) microVMs. It exposes an HTTP API for creating, querying, listing, and deleting virtual machines on a Linux host, with automatic jailer integration for security isolation, per-VM rootfs copies, and resource management.

For a deeper look at how Warlock works internally, see the [architecture overview](docs/architecture.md).

## API

All endpoints return JSON.

| Method | Path | Description |
|---|---|---|
| `GET` | `/internal/hc` | Healthcheck -- returns status, capacity, VM count, copy strategy |
| `POST` | `/vm` | Create a VM (202 Accepted) |
| `GET` | `/vm` | List all VMs with state and resource allocation |
| `GET` | `/vm/{id}` | Get a specific VM's state |
| `DELETE` | `/vm/{id}` | Stop and delete a VM |

### Create VM

```bash
curl -X POST http://localhost:3000/vm \
  -H "Content-Type: application/json" \
  -d '{"vcpus": 2, "memory_mb": 256}'
```

Both fields are optional. Defaults: 1 vCPU, 128 MB memory. Constraints: vCPUs must be 1 or an even number up to 32; memory must be at least 128 MB.

### List VMs

```bash
curl http://localhost:3000/vm
```

### Healthcheck

```bash
curl http://localhost:3000/internal/hc
```

Returns capacity, running VM count, allocated resources, and the detected rootfs copy strategy.

## Host Setup

Warlock requires a Linux host with KVM support. The [`install-firecracker.sh`](scripts/install-firecracker.sh) script handles all host prerequisites:

- Creates the `firecracker` system user (uid/gid 1100)
- Downloads Firecracker and jailer binaries
- Downloads the getting-started kernel and rootfs images to `/opt/firecracker/`
- Creates `/srv/jailer/` and `/srv/jailer/vm-images/` directories

```bash
sudo ./scripts/install-firecracker.sh
```

> [!IMPORTANT]
> `/opt/firecracker` and `/srv/jailer` must be on the same filesystem. The jailer uses hard links, which cannot cross filesystem boundaries.

## Configuration

| Variable | Description | Default |
|---|---|---|
| `FIRECRACKER_BIN` | Path to the Firecracker binary | Resolved from `PATH` |
| `JAILER_BIN` | Path to the jailer binary | Resolved from `PATH` |
| `WARLOCK_DEV` | Set to `true` to skip all Firecracker/KVM/jailer checks | `false` |
| `RUST_LOG` | Tracing filter directive (e.g. `debug`, `warlock=debug`) | `info` |

## Development

### Prerequisites

- Rust v1.91+
- Firecracker and a compatible machine (or `WARLOCK_DEV=true` for local development without Firecracker)

### Getting started

Run the app:

```bash
cargo run
```

Or with Make:

```bash
make start
```

For local development on macOS (where Firecracker is not available):

```bash
WARLOCK_DEV=true cargo run
```

## Tests

```bash
cargo test
```

Or:

```bash
make test
```

Tests always run in development mode (`WARLOCK_DEV=true` is set automatically).

## Deployment

The [release](./.github/workflows/release.yml) workflow builds Warlock for Linux x86_64 on tag push:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The workflow can also be dispatched manually.

## Installation

The [install script](scripts/install.sh) downloads the latest release and installs it to the system path:

```bash
curl -fsSL https://raw.githubusercontent.com/TristanBlackwell/warlock/master/scripts/install.sh | bash
```

Then run:

```bash
warlock
```

## Scripts

| Script | Description |
|---|---|
| [`install.sh`](scripts/install.sh) | Downloads and installs the latest Warlock binary |
| [`install-firecracker.sh`](scripts/install-firecracker.sh) | Installs Firecracker, jailer, kernel, and rootfs; creates system user and directories |
| [`setup-droplet.sh`](scripts/setup-droplet.sh) | Provisions a DigitalOcean Droplet with Firecracker and Warlock for testing |

### Droplet

The setup script spins up a $6/month Droplet on DigitalOcean, then installs Firecracker and Warlock.

Prerequisites:

- `doctl` -- Install with `brew install doctl` (or equivalent)
- Authenticate with `doctl auth init` (scopes: `account:read`, `droplet:create`, `droplet:delete`, `ssh:create`)
- An SSH key (the script creates one if needed)

```bash
make droplet          # create
make droplet-destroy  # tear down
```
