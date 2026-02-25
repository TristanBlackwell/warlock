#!/usr/bin/env bash
set -euo pipefail

# Provisions a DigitalOcean Droplet pre-configured with Firecracker and Warlock.
#
# Prerequisites:
#   - doctl CLI installed and authenticated (brew install doctl && doctl auth init)
#   - At least one SSH key registered in your DigitalOcean account
#
# Usage:
#   ./scripts/setup-droplet.sh [--name <droplet-name>]
#
# Defaults:
#   Name:   warlock-dev
#   Region: lon1 (London)
#   Image:  ubuntu-24-04-x64
#   Size:   s-1vcpu-1gb ($6/mo)

DROPLET_NAME="warlock-dev"
DROPLET_REGION="lon1"
DROPLET_IMAGE="ubuntu-24-04-x64"
DROPLET_SIZE="s-1vcpu-1gb"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SSH_BASE_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

info() {
  echo "  $1"
}

error() {
  echo "ERROR: $1" >&2
  exit 1
}

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
  case "$1" in
    --name)
      DROPLET_NAME="$2"
      shift 2
      ;;
    *)
      error "Unknown argument: $1"
      ;;
  esac
done

# ---------------------------------------------------------------------------
# 1. Check prerequisites
# ---------------------------------------------------------------------------

echo "Checking prerequisites..."

# doctl
if ! command -v doctl >/dev/null 2>&1; then
  echo ""
  echo "doctl is not installed. Install it with:"
  echo ""
  echo "  brew install doctl     # macOS"
  echo "  snap install doctl     # Linux"
  echo ""
  echo "Then authenticate:"
  echo ""
  echo "  doctl auth init"
  echo ""
  error "doctl is required."
fi

# doctl auth
if ! doctl account get >/dev/null 2>&1; then
  echo ""
  echo "doctl is not authenticated. Run:"
  echo ""
  echo "  doctl auth init"
  echo ""
  error "doctl authentication required."
fi

info "doctl is installed and authenticated."

# Check install scripts exist
[ -f "${SCRIPT_DIR}/install-firecracker.sh" ] || error "scripts/install-firecracker.sh not found."
[ -f "${SCRIPT_DIR}/install.sh" ] || error "scripts/install.sh not found."

# ---------------------------------------------------------------------------
# 2. Detect or create SSH key
# ---------------------------------------------------------------------------

echo "Detecting SSH key..."

WARLOCK_KEY="$HOME/.ssh/warlock_ed25519"
SSH_KEY_ID=""
SSH_KEY_NAME=""
LOCAL_KEY_FILE=""

# Match a local SSH public key against keys registered in DigitalOcean.
# This ensures the Droplet is provisioned with a key we can actually use.
DO_KEYS=$(doctl compute ssh-key list --format ID,Name,FingerPrint --no-header 2>/dev/null || true)

if [ -n "$DO_KEYS" ]; then
  for pubkey in ~/.ssh/*.pub; do
    [ -f "$pubkey" ] || continue

    LOCAL_FP=$(ssh-keygen -l -E md5 -f "$pubkey" 2>/dev/null | awk '{print $2}' | sed 's/^MD5://')
    [ -n "$LOCAL_FP" ] || continue

    MATCH=$(echo "$DO_KEYS" | awk -v fp="$LOCAL_FP" '$NF == fp {print $1, $NF}' | head -1)
    if [ -n "$MATCH" ]; then
      SSH_KEY_ID=$(echo "$MATCH" | awk '{print $1}')
      SSH_KEY_NAME=$(echo "$DO_KEYS" | awk -v id="$SSH_KEY_ID" '$1 == id {$1=""; $NF=""; print}' | xargs)
      LOCAL_KEY_FILE="${pubkey%.pub}"
      break
    fi
  done
fi

# If no match was found, generate a dedicated warlock key and upload it to DO
if [ -z "$SSH_KEY_ID" ]; then
  info "No local SSH key matches your DigitalOcean account."

  # Generate key if it doesn't exist locally
  if [ ! -f "$WARLOCK_KEY" ]; then
    info "Generating new SSH key at ${WARLOCK_KEY}..."
    ssh-keygen -t ed25519 -f "$WARLOCK_KEY" -N "" -C "warlock" -q
  else
    info "Found existing key at ${WARLOCK_KEY}"
  fi

  # Check if this key is already in DO (e.g. from a previous run that failed after upload)
  WARLOCK_FP=$(ssh-keygen -l -E md5 -f "${WARLOCK_KEY}.pub" 2>/dev/null | awk '{print $2}' | sed 's/^MD5://')

  if [ -n "$DO_KEYS" ]; then
    EXISTING=$(echo "$DO_KEYS" | awk -v fp="$WARLOCK_FP" '$NF == fp {print $1}' | head -1)
  else
    EXISTING=""
  fi

  if [ -n "$EXISTING" ]; then
    SSH_KEY_ID="$EXISTING"
    info "Key already registered in DigitalOcean (${SSH_KEY_ID})"
  else
    info "Uploading public key to DigitalOcean..."
    SSH_KEY_ID=$(doctl compute ssh-key create warlock \
      --public-key "$(cat "${WARLOCK_KEY}.pub")" \
      --format ID --no-header) \
      || error "Failed to upload SSH key to DigitalOcean."
    info "Key uploaded to DigitalOcean (${SSH_KEY_ID})"
  fi

  SSH_KEY_NAME="warlock"
  LOCAL_KEY_FILE="$WARLOCK_KEY"
fi

info "Using SSH key: ${SSH_KEY_NAME} (${SSH_KEY_ID})"
info "Local key:     ${LOCAL_KEY_FILE}"

SSH_OPTS="${SSH_BASE_OPTS} -i ${LOCAL_KEY_FILE}"

# ---------------------------------------------------------------------------
# 3. Check for existing Droplet
# ---------------------------------------------------------------------------

EXISTING_ID=$(doctl compute droplet list --format ID,Name --no-header 2>/dev/null \
  | grep " ${DROPLET_NAME}$" | awk '{print $1}' || true)

if [ -n "$EXISTING_ID" ]; then
  echo ""
  echo "A Droplet named '${DROPLET_NAME}' already exists (ID: ${EXISTING_ID})."
  echo "Destroy it first with: make droplet-destroy"
  echo "Or use a different name: ./scripts/setup-droplet.sh --name my-other-name"
  echo ""
  error "Droplet already exists."
fi

# ---------------------------------------------------------------------------
# 4. Create Droplet
# ---------------------------------------------------------------------------

echo ""
echo "Creating Droplet '${DROPLET_NAME}'..."
info "Region: ${DROPLET_REGION}"
info "Image:  ${DROPLET_IMAGE}"
info "Size:   ${DROPLET_SIZE}"

doctl compute droplet create "$DROPLET_NAME" \
  --region "$DROPLET_REGION" \
  --image "$DROPLET_IMAGE" \
  --size "$DROPLET_SIZE" \
  --ssh-keys "$SSH_KEY_ID" \
  --wait \
  --no-header \
  --format ID >/dev/null

info "Droplet created."

# ---------------------------------------------------------------------------
# 5. Get Droplet IP
# ---------------------------------------------------------------------------

echo "Retrieving Droplet IP..."

DROPLET_IP=""
for i in $(seq 1 10); do
  DROPLET_IP=$(doctl compute droplet get "$DROPLET_NAME" --format PublicIPv4 --no-header 2>/dev/null || true)
  if [ -n "$DROPLET_IP" ] && [ "$DROPLET_IP" != "" ]; then
    break
  fi
  sleep 3
done

[ -n "$DROPLET_IP" ] || error "Failed to retrieve Droplet IP address."

info "Droplet IP: ${DROPLET_IP}"

# ---------------------------------------------------------------------------
# 6. Wait for SSH
# ---------------------------------------------------------------------------

echo -n "Waiting for SSH to become available"

MAX_ATTEMPTS=60
for i in $(seq 1 $MAX_ATTEMPTS); do
  if ssh $SSH_OPTS -o ConnectTimeout=5 "root@${DROPLET_IP}" true 2>/dev/null; then
    echo ""
    info "SSH is ready."
    break
  fi

  if [ "$i" -eq "$MAX_ATTEMPTS" ]; then
    echo ""
    error "SSH did not become available after $((MAX_ATTEMPTS * 5)) seconds."
  fi

  echo -n "."
  sleep 5
done

# ---------------------------------------------------------------------------
# 7. Copy and run install scripts
# ---------------------------------------------------------------------------

echo ""
echo "Copying install scripts to Droplet..."

scp $SSH_OPTS \
  "${SCRIPT_DIR}/install-firecracker.sh" \
  "${SCRIPT_DIR}/install.sh" \
  "root@${DROPLET_IP}:/tmp/"

echo ""
echo "Installing Firecracker..."
ssh $SSH_OPTS "root@${DROPLET_IP}" "bash /tmp/install-firecracker.sh"

echo ""
echo "Installing Warlock..."
ssh $SSH_OPTS "root@${DROPLET_IP}" "bash /tmp/install.sh"

# Clean up remote temp files
ssh $SSH_OPTS "root@${DROPLET_IP}" "rm -f /tmp/install-firecracker.sh /tmp/install.sh"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "================================================"
echo "  Droplet ready!"
echo "================================================"
echo ""
echo "  Name:    ${DROPLET_NAME}"
echo "  IP:      ${DROPLET_IP}"
echo "  Region:  ${DROPLET_REGION}"
echo ""
echo "  SSH:     ssh -i ${LOCAL_KEY_FILE} root@${DROPLET_IP}"
echo "  Start:   ssh -i ${LOCAL_KEY_FILE} root@${DROPLET_IP} warlock"
echo ""
echo "  Destroy: make droplet-destroy"
echo "================================================"
