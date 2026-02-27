#!/usr/bin/env bash
set -euo pipefail

# Installs the latest Firecracker release binary plus a kernel image and
# Ubuntu rootfs from Firecracker's CI assets. Designed to run on any
# Linux x86_64 machine (Ubuntu recommended).
#
# Assets are stored under /opt/firecracker/ with convenience symlinks:
#   /opt/firecracker/vmlinux      -> latest kernel
#   /opt/firecracker/rootfs.ext4  -> latest rootfs
#   /opt/firecracker/*.id_rsa     -> SSH key for guest access
#
# Usage:
#   sudo bash install-firecracker.sh

INSTALL_DIR="/usr/local/bin"
ASSETS_DIR="/opt/firecracker"
REPO="firecracker-microvm/firecracker"
ARCH="$(uname -m)"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

TEMP_DIR=""
cleanup() {
  if [ -n "$TEMP_DIR" ] && [ -d "$TEMP_DIR" ]; then
    rm -rf "$TEMP_DIR"
  fi
}
trap cleanup EXIT

info() {
  echo "  $1"
}

error() {
  echo "ERROR: $1" >&2
  exit 1
}

# ---------------------------------------------------------------------------
# Pre-flight
# ---------------------------------------------------------------------------

[ "$(uname -s)" = "Linux" ] || error "This script only supports Linux."
[ "$ARCH" = "x86_64" ] || error "Only x86_64 is supported. Detected: $ARCH"

command -v curl >/dev/null 2>&1 || error "curl is required but not installed."

# KVM check (warn only — the binary installs fine without it)
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
  info "KVM is available."
else
  echo "WARNING: /dev/kvm is not accessible. Firecracker will install but VMs cannot start without KVM."
fi

# We need sudo for writing to /usr/local/bin and /opt
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  if command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
  else
    error "This script requires root privileges. Run with sudo or as root."
  fi
fi

TEMP_DIR=$(mktemp -d)

# ---------------------------------------------------------------------------
# 1. Create firecracker system user/group and jailer directories
# ---------------------------------------------------------------------------

echo ""
echo "Setting up jailer prerequisites..."

if ! id -u firecracker &>/dev/null; then
  info "Creating 'firecracker' system user (uid/gid 1100)..."
  $SUDO groupadd --system --gid 1100 firecracker
  $SUDO useradd --system --uid 1100 --gid 1100 --no-create-home --shell /usr/sbin/nologin firecracker
  info "Created user 'firecracker' (1100:1100)"
else
  info "User 'firecracker' already exists."
fi

$SUDO mkdir -p /srv/jailer
$SUDO mkdir -p /srv/jailer/vm-images
info "Jailer chroot base: /srv/jailer"
info "VM images directory: /srv/jailer/vm-images"

# ---------------------------------------------------------------------------
# 2. Configure networking for VM connectivity
# ---------------------------------------------------------------------------

echo ""
echo "Configuring networking..."

# Ensure nftables is available
if ! command -v nft >/dev/null 2>&1; then
  info "Installing nftables..."
  $SUDO apt-get update -qq && $SUDO apt-get install -y -qq nftables >/dev/null
fi

# Enable IPv4 forwarding (persistent)
if [ "$(cat /proc/sys/net/ipv4/ip_forward)" != "1" ]; then
  info "Enabling IPv4 forwarding..."
  $SUDO sysctl -w net.ipv4.ip_forward=1 >/dev/null
fi

# Persist across reboots
if [ ! -f /etc/sysctl.d/99-firecracker.conf ] || ! grep -q "net.ipv4.ip_forward" /etc/sysctl.d/99-firecracker.conf 2>/dev/null; then
  echo "net.ipv4.ip_forward = 1" | $SUDO tee /etc/sysctl.d/99-firecracker.conf >/dev/null
  info "IPv4 forwarding persisted to /etc/sysctl.d/99-firecracker.conf"
fi

# Create nftables table and chains for VM NAT (idempotent)
if ! $SUDO nft list table firecracker >/dev/null 2>&1; then
  info "Creating nftables firecracker table and chains..."
  $SUDO nft add table firecracker
  $SUDO nft 'add chain firecracker postrouting { type nat hook postrouting priority srcnat; policy accept; }'
  $SUDO nft 'add chain firecracker filter { type filter hook forward priority filter; policy accept; }'
  info "nftables firecracker table created with postrouting and filter chains"
else
  info "nftables firecracker table already exists"
fi

# ---------------------------------------------------------------------------
# 3. Install Firecracker binary
# ---------------------------------------------------------------------------

echo ""
echo "Installing Firecracker..."

info "Fetching latest release tag..."
LATEST_TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
  | grep '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')

[ -n "$LATEST_TAG" ] || error "Failed to determine latest Firecracker release."

info "Downloading Firecracker ${LATEST_TAG}..."
TARBALL="firecracker-${LATEST_TAG}-${ARCH}.tgz"
curl -fsSL "https://github.com/${REPO}/releases/download/${LATEST_TAG}/${TARBALL}" \
  -o "${TEMP_DIR}/${TARBALL}" \
  || error "Failed to download ${TARBALL}."

info "Extracting..."
tar -xzf "${TEMP_DIR}/${TARBALL}" -C "$TEMP_DIR"

RELEASE_DIR="${TEMP_DIR}/release-${LATEST_TAG}-${ARCH}"
[ -d "$RELEASE_DIR" ] || error "Unexpected archive layout — expected ${RELEASE_DIR}."

# Install firecracker and jailer binaries
for bin in firecracker jailer; do
  SRC="${RELEASE_DIR}/${bin}-${LATEST_TAG}-${ARCH}"
  if [ -f "$SRC" ]; then
    chmod +x "$SRC"
    $SUDO mv -f "$SRC" "${INSTALL_DIR}/${bin}"
    info "Installed ${bin} to ${INSTALL_DIR}/${bin}"
  else
    echo "WARNING: ${bin} binary not found in release archive, skipping."
  fi
done

info "Firecracker ${LATEST_TAG} installed."

# ---------------------------------------------------------------------------
# 4. Download kernel and rootfs from Firecracker CI
# ---------------------------------------------------------------------------

echo ""
echo "Downloading kernel and rootfs..."

# The CI assets use the version prefix with the 'v' (e.g. firecracker-ci/v1.14/)
CI_VERSION="${LATEST_TAG%.*}"

# --- Kernel ---
info "Fetching latest kernel image key..."
LATEST_KERNEL_KEY=$(curl -fsSL \
  "http://spec.ccfc.min.s3.amazonaws.com/?prefix=firecracker-ci/${CI_VERSION}/${ARCH}/vmlinux-&list-type=2" \
  | sed -n 's/.*<Key>\(firecracker-ci\/[^<]*vmlinux-[0-9]\+\.[0-9]\+\.[0-9]\+\)<\/Key>.*/\1/p' \
  | sort -V | tail -1)

[ -n "$LATEST_KERNEL_KEY" ] || error "Failed to find kernel image in Firecracker CI."

KERNEL_FILE=$(basename "$LATEST_KERNEL_KEY")
info "Downloading kernel: ${KERNEL_FILE}..."
curl -fsSL "https://s3.amazonaws.com/spec.ccfc.min/${LATEST_KERNEL_KEY}" \
  -o "${TEMP_DIR}/${KERNEL_FILE}" \
  || error "Failed to download kernel image."

# --- Rootfs ---
info "Fetching latest Ubuntu rootfs key..."
LATEST_UBUNTU_KEY=$(curl -fsSL \
  "http://spec.ccfc.min.s3.amazonaws.com/?prefix=firecracker-ci/${CI_VERSION}/${ARCH}/ubuntu-&list-type=2" \
  | sed -n 's/.*<Key>\(firecracker-ci\/[^<]*ubuntu-[0-9]\+\.[0-9]\+\.squashfs\)<\/Key>.*/\1/p' \
  | sort -V | tail -1)

[ -n "$LATEST_UBUNTU_KEY" ] || error "Failed to find Ubuntu rootfs in Firecracker CI."

UBUNTU_SQUASHFS=$(basename "$LATEST_UBUNTU_KEY")
UBUNTU_VERSION=$(echo "$UBUNTU_SQUASHFS" | grep -oE '[0-9]+\.[0-9]+')

info "Downloading rootfs: ${UBUNTU_SQUASHFS}..."
curl -fsSL "https://s3.amazonaws.com/spec.ccfc.min/${LATEST_UBUNTU_KEY}" \
  -o "${TEMP_DIR}/${UBUNTU_SQUASHFS}" \
  || error "Failed to download Ubuntu rootfs."

# ---------------------------------------------------------------------------
# 5. Prepare rootfs (unsquash, patch SSH key, create ext4)
# ---------------------------------------------------------------------------

echo ""
echo "Preparing rootfs..."

# Ensure squashfs-tools is available
if ! command -v unsquashfs >/dev/null 2>&1; then
  info "Installing squashfs-tools..."
  $SUDO apt-get update -qq && $SUDO apt-get install -y -qq squashfs-tools >/dev/null
fi

WORK_DIR="${TEMP_DIR}/rootfs-work"
mkdir -p "$WORK_DIR"

info "Extracting squashfs..."
$SUDO unsquashfs -d "${WORK_DIR}/squashfs-root" "${TEMP_DIR}/${UBUNTU_SQUASHFS}" >/dev/null

# Generate an SSH keypair for guest access
info "Generating SSH key for guest access..."
ssh-keygen -t rsa -f "${WORK_DIR}/id_rsa" -N "" -q

# Ensure .ssh directory exists and patch in the public key
$SUDO mkdir -p "${WORK_DIR}/squashfs-root/root/.ssh"
$SUDO cp "${WORK_DIR}/id_rsa.pub" "${WORK_DIR}/squashfs-root/root/.ssh/authorized_keys"

# Create ext4 filesystem image
ROOTFS_NAME="ubuntu-${UBUNTU_VERSION}.ext4"
info "Creating ext4 image: ${ROOTFS_NAME}..."
$SUDO chown -R root:root "${WORK_DIR}/squashfs-root"
truncate -s 1G "${WORK_DIR}/${ROOTFS_NAME}"
$SUDO mkfs.ext4 -d "${WORK_DIR}/squashfs-root" -F "${WORK_DIR}/${ROOTFS_NAME}" >/dev/null 2>&1

# ---------------------------------------------------------------------------
# 6. Install assets to /opt/firecracker/
# ---------------------------------------------------------------------------

echo ""
echo "Installing assets to ${ASSETS_DIR}/..."

$SUDO mkdir -p "$ASSETS_DIR"

# Kernel
$SUDO mv -f "${TEMP_DIR}/${KERNEL_FILE}" "${ASSETS_DIR}/${KERNEL_FILE}"
$SUDO ln -sf "${ASSETS_DIR}/${KERNEL_FILE}" "${ASSETS_DIR}/vmlinux"
info "Kernel:  ${ASSETS_DIR}/${KERNEL_FILE}"

# Rootfs
$SUDO mv -f "${WORK_DIR}/${ROOTFS_NAME}" "${ASSETS_DIR}/${ROOTFS_NAME}"
$SUDO ln -sf "${ASSETS_DIR}/${ROOTFS_NAME}" "${ASSETS_DIR}/rootfs.ext4"
info "Rootfs:  ${ASSETS_DIR}/${ROOTFS_NAME}"

# SSH key
KEY_NAME="ubuntu-${UBUNTU_VERSION}.id_rsa"
$SUDO mv -f "${WORK_DIR}/id_rsa" "${ASSETS_DIR}/${KEY_NAME}"
$SUDO chmod 600 "${ASSETS_DIR}/${KEY_NAME}"
info "SSH key: ${ASSETS_DIR}/${KEY_NAME}"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "Firecracker installation complete!"
echo ""
echo "  Binaries:"
echo "    firecracker  -> ${INSTALL_DIR}/firecracker"
echo "    jailer       -> ${INSTALL_DIR}/jailer"
echo ""
echo "  VM assets (${ASSETS_DIR}/):"
echo "    vmlinux      -> ${KERNEL_FILE}"
echo "    rootfs.ext4  -> ${ROOTFS_NAME}"
echo "    SSH key      -> ${KEY_NAME}"
echo ""
echo "  Versions:"
echo "    Firecracker: ${LATEST_TAG}"
echo "    Kernel:      ${KERNEL_FILE}"
echo "    Rootfs:      ubuntu-${UBUNTU_VERSION}"
