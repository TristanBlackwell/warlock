# Warlock

Warlock is an experimental application providing a control plane over [Firecracker](https://github.com/firecracker-microvm/firecracker) on a Linux machine.


## Development

### Prerequisites

- Rust v1.91
- Firecracker and a compatible machine - See Firecracker's [getting started](https://github.com/firecracker-microvm/firecracker/blob/main/docs/getting-started.md)

Warlock will attempt to find firecracker on the PATH of the host machine. If this is not found, or you wish to override it's location you can use the `FIRECRACKER_BIN` environment variable to define the location of the firecracker binary. Warlock must find a minimum compatible version of Firecracker (`>1.14.0`) for preflight checks to pass.

The necessary KVM requirements will also be checked at start-up to ensure Firecracker will be able to create microvms.

> [!TIP]
> It is possible to bypass Firecracker and KVM checks with the `WARLOCK_DEV` environment variable. Naturally the use cases for this are limited since this is designed to work with Firecracker.

### Getting started

1. Run the app:

```bash
`cargo run`
```

or if you prefer Make:

```bash
make start
```

define the location of the Firecracker binary:

```bash
FIRECRACKER_BIN="/apps/firecracker" cargo run
```

## Tests

You can run the apps unit tests:

```bash
cargo test
```

or:

```bash
make test
```

## Deployment

The [release](./.github/workflows/release.yml) workflow will run the application tests and build Warlock for Linux machines on the push of a new tag:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The workflow can also be dispatched manually.
 
## Installation

The [install](./scripts/install.sh) can be used to download and install Warlock on a Linux machine:

```bash
curl -fsSL https://raw.githubusercontent.com/TristanBlackwell/warlock/master/install.sh | bash
```

This will place Warlock on the device path.

```bash
warlock
```

You can also define variables as when running locally:

```bash
FIRECRACKER_BIN="./firecracker" warlock
```

## Scripts

There are a few utility scripts to help with development, testing, and deployment.

### Install

The [install.sh](./scripts/install.sh) will download the latest Warlock build from GitHub and install it onto the path of the machine.

### Firecracker

The [install-firecracker.sh](./scripts/install-firecracker.sh) installs the latest Firecracker binary as well as the getting started kernel image and rootfs.

### Droplet

The [setup-droplet.sh](./scripts/setup-droplet.sh) will spin up a $6 / month Droplet on DigitalOcean, then install Firecracker and Warlock, essentially scaffolding a VM in preparation for testing.

There is a few prerequisites to this:

- `doctl` - Install the DigitalOcean CLI with `brew install doctl` (or the respective command for your system).
- authenticate - Authenticate with DigitalOcean `doctl auth init`, you'll need a token with scopes `account:read`, `droplet:create`, `droplet:delete`, `ssh:create`.
- SSH key - The script will create a local warlock SSH key (if one doesn't already exist) and upload this to DigitalOcean so the script (and you) can access the VM.

This can be ran via Make: `make droplet`.

You can destroy the machine after creation with `Make droplet-destroy`.
