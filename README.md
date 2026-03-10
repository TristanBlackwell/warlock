# Warlock

Warlock is an experimental control plane layered over [Firecracker](https://github.com/firecracker-microvm/firecracker). 

The project focuses on abstracting the intricacies of Firecracker VMM management. The application can prepare a bare KVM-supported machine and exposes a simple, opinionated HTTP layer for interaction. It supports capacity planning, [jailer](https://github.com/firecracker-microvm/firecracker/blob/main/docs/jailer.md) isolation and machine configuration.

Warlock is implemented in [Rust](https://www.rust-lang.org/), and uses [Axum](https://github.com/tokio-rs/axum). A more detailed description of architecture is detailed in the [architecture overview](docs/architecture.md).

## Development

### Prerequisites

- A KVM enabled machine
- Firecracker >= v1.14.1
- Rust v1.91+

> [!TIP]
> I recommend following the [getting started](https://github.com/firecracker-microvm/firecracker/blob/main/docs/getting-started.md) guide from Firecracker on your intended development machine. This will ensure your machine meets the necessary prerequisites and that you can run Firecracker VMs.

#### Host Setup

The [`install-firecracker.sh`](scripts/install-firecracker.sh) script handles all host prerequisites:

- Creates the `firecracker` system user (uid/gid 1100)
- Downloads Firecracker and jailer binaries
- Downloads the getting-started kernel and rootfs images to `/opt/firecracker/`
- Creates `/srv/jailer/` and `/srv/jailer/vm-images/` directories

```bash
sudo ./scripts/install-firecracker.sh
```

> [!IMPORTANT]
> `/opt/firecracker` and `/srv/jailer` must be on the same filesystem. The jailer uses hard links, which cannot cross filesystem boundaries.

### Getting started

Run the app:

```bash
cargo run
```

Or with Make:

```bash
make start
```

Warlock runs preflight checks on startup and will configure the machine environment in preparation for machine creation. It is possible to disable these checks with the `WARLOCK_DEV` environment variable:

```bash
WARLOCK_DEV=true cargo run
```

### Tests

```bash
cargo test
```

Or:

```bash
make test
```

Tests always run in development mode (`WARLOCK_DEV=true` is set automatically). These are static tests that can function without the existence of Firecracker.

Separately there are tests that will interact with Firecracker for the VM lifecycle. These can be run with `make test-live`

### Configuration

| Variable | Description | Default |
|---|---|---|
| `FIRECRACKER_BIN` | Path to the Firecracker binary | Resolved from `PATH` |
| `JAILER_BIN` | Path to the jailer binary | Resolved from `PATH` |
| `WARLOCK_DEV` | Set to `true` to skip all Firecracker/KVM/jailer checks | `false` |
| `RUST_LOG` | Tracing filter directive (e.g. `debug`, `warlock=debug`) | `info` |
| `GATEWAY_URL` | URL to the Warlock gateway for worker registration and VM lifecycle reporting | Not set (gateway disabled) |
| `WORKER_ID` | Unique identifier for this worker node (used by gateway) | Hostname |
| `WORKER_IP` | IP address where this worker is reachable (required if `GATEWAY_URL` is set) | None (must be set explicitly) |

For details on the gateway and related variables, see the [gateway docs](./docs/gateway.md).

## Capabilities

The actions available via Warlock can be found in the [API docs](./docs/api.md)

## Releases

New versions can be found on the [GitHub Releases](https://github.com/TristanBlackwell/warlock/releases) page.

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
