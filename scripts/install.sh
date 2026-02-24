#!/usr/bin/env bash
set -euo pipefail

REPO="TristanBlackwell/warlock"
BINARY_NAME="warlock-linux-x86_64"
INSTALL_DIR="/usr/local/bin"
INSTALL_NAME="warlock"

# Clean up temp directory on exit
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

command -v curl >/dev/null 2>&1 || error "curl is required but not installed."
command -v sha256sum >/dev/null 2>&1 || error "sha256sum is required but not installed."

echo "Installing warlock..."

# Determine if we need sudo to write to the install directory
SUDO=""
if [ ! -w "$INSTALL_DIR" ]; then
  if command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
  else
    error "Cannot write to $INSTALL_DIR and sudo is not available. Run as root or install to a writable directory."
  fi
fi

info "Fetching latest release..."
LATEST_TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')

if [ -z "$LATEST_TAG" ]; then
  error "Failed to determine latest release. Check https://github.com/${REPO}/releases"
fi

TEMP_DIR=$(mktemp -d)

DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${LATEST_TAG}"

info "Downloading warlock ${LATEST_TAG}..."
curl -fsSL "${DOWNLOAD_URL}/${BINARY_NAME}" -o "${TEMP_DIR}/${BINARY_NAME}" ||
  error "Failed to download binary. Does release ${LATEST_TAG} exist at ${DOWNLOAD_URL}?"

curl -fsSL "${DOWNLOAD_URL}/${BINARY_NAME}.sha256" -o "${TEMP_DIR}/${BINARY_NAME}.sha256" ||
  error "Failed to download checksum."

info "Verifying checksum..."
(cd "$TEMP_DIR" && sha256sum -c "${BINARY_NAME}.sha256" --quiet) ||
  error "Checksum verification failed. The download may be corrupted."

info "Installing to ${INSTALL_DIR}/${INSTALL_NAME}..."
chmod +x "${TEMP_DIR}/${BINARY_NAME}"
$SUDO mv -f "${TEMP_DIR}/${BINARY_NAME}" "${INSTALL_DIR}/${INSTALL_NAME}"

echo ""
echo "Successfully installed warlock ${LATEST_TAG}"
