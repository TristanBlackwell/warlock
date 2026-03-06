# ADR-003: SSH Console Access via virtio-vsock

**Status:** Ready  
**Date:** 2026-03-06  
**Context:** Interactive terminal access to Firecracker microVMs without guest network exposure

## Context

Operators need interactive terminal access to VMs for debugging, configuration, and monitoring. Firecracker VMs communicate via vsock (virtio-vsock), not exposed network interfaces. Access must work from anywhere on the internet, not just the Warlock host, and support multiple client types: CLI tools, desktop applications, and automation scripts.

**Requirements:**
- Standard protocol (no custom clients)
- Works globally (through NAT, firewalls, etc.)
- Supports full TTY (vim, htop, terminal resize)
- Minimal setup for users
- Secure by default

## Decision

Use SSH as the console access protocol, proxying connections through virtio-vsock to guest VMs.

### Architecture

```
User's SSH Client (anywhere on internet)
       ↓
   TCP port 2222
       ↓
Warlock SSH Server (russh library)
       ↓
Username Parser: vm-{uuid} → VM lookup
       ↓
Unix Domain Socket (/srv/jailer/firecracker/{vm_id}/root/{vm_id}.sock)
       ↓
Firecracker vsock device (CONNECT handshake)
       ↓
Guest socat listener (VSOCK-LISTEN:1024)
       ↓
PTY + /bin/login
```

### Core Decisions

1. **Protocol:** SSH (standard, encrypted, widely supported)
2. **Port:** 2222 (non-privileged, no conflict with host SSH)
3. **Authentication:** Public key, per-VM authorized keys
4. **Username Format:** `vm-{uuid}@host` (VM ID encoded in username)
5. **Host Key:** Ephemeral Ed25519 key (generated on startup, not persisted)
6. **Guest Listener:** socat with vsock + PTY + /bin/login

### API Design

Users provide SSH public keys when creating a VM:

```json
POST /vm
Content-Type: application/json

{
  "vcpus": 2,
  "memory_mb": 512,
  "ssh_keys": [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... user@laptop",
    "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAB... automation-bot"
  ]
}
```

### Connection

```bash
ssh vm-abc-123-def-456@warlock.example.com -p 2222
```

## Implementation

### Components

**SSH Server** (`src/ssh/server.rs`):
- Listens on `0.0.0.0:2222` (all interfaces)
- Uses russh library (pure Rust, async/await)
- Generates ephemeral Ed25519 host key on startup
- **Note:** Key not persisted - causes SSH "host key changed" warnings on restart

**Session Handler** (`src/ssh/session.rs`):
- Parses username to extract VM UUID
- Validates VM exists and is in Running state
- **Validates offered SSH key against VM's authorized keys** (per-VM authorization)
- Connects to vsock UDS at `/srv/jailer/firecracker/{vm_id}/root/{vm_id}.sock`
- Sends `CONNECT 1024\n` handshake to Firecracker vsock proxy
- Bidirectionally proxies: SSH channel ↔ UDS via tokio channels
- Handles PTY requests and terminal resize (window_change_request)

**Authorized Keys Module** (`src/ssh/authorized_keys.rs`):
- Parses OpenSSH public key format (e.g., "ssh-ed25519 AAAA...")
- Compares keys using russh's native key equality
- Supports Ed25519, RSA, and EC keys
- Skips comments and empty lines
- Returns true only if key matches exactly

**Guest Listener** (systemd service `vsock-console.service`):
```ini
[Unit]
Description=vsock console listener
After=network.target

[Service]
Type=simple
Environment="TERM=xterm-256color"
ExecStart=/usr/bin/socat VSOCK-LISTEN:1024,fork,reuseaddr EXEC:"/bin/login -p",pty,stderr,setsid,sigint,sane
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

### Data Model

```rust
pub struct VmResources {
    // ... existing fields
    pub ssh_keys: Vec<String>,  // OpenSSH public key format
}
```

### Directory Structure

**Current:** No persistent files (ephemeral host key)

**Future:** Persistent host key storage
```
/etc/warlock/ssh/
  ssh_host_ed25519_key      # Persistent Ed25519 host key
  ssh_host_ed25519_key.pub  # Public component
```

## External Access

### Network Configuration

The SSH server binds to all interfaces (`0.0.0.0:2222`), accepting connections from any network. For external access:

**Firewall Setup:**
```bash
# Ubuntu/Debian with UFW
sudo ufw allow 2222/tcp

# Or cloud provider firewall:
# AWS Security Group: Allow TCP 2222 from 0.0.0.0/0
# GCP Firewall Rule: Allow tcp:2222
# DigitalOcean: Add firewall rule for TCP 2222
```

**DNS:**
- Users connect via IP address: `ssh vm-{uuid}@1.2.3.4 -p 2222`
- Optional: Set up DNS for convenience (e.g., warlock.example.com)

The standard SSH protocol works through NAT, proxies, and corporate firewalls without special configuration.

## User Workflow

### Creating VM with SSH Access

```bash
# 1. Generate SSH key (one-time)
ssh-keygen -t ed25519 -f ~/.ssh/warlock_key

# 2. Create VM with public key
curl -X POST http://warlock.example.com:3000/vm \
  -H "Content-Type: application/json" \
  -d "{
    \"vcpus\": 2,
    \"memory_mb\": 512,
    \"ssh_keys\": [\"$(cat ~/.ssh/warlock_key.pub)\"]
  }"

# Response: {"id": "abc-123-def-456", ...}

# 3. Wait for boot (~15 seconds)
sleep 15

# 4. Connect via SSH
ssh -i ~/.ssh/warlock_key vm-abc-123-def-456@warlock.example.com -p 2222
```

### Multiple Keys

Users can provide multiple SSH public keys for a single VM (shared access, key rotation, etc.):

```json
{
  "ssh_keys": [
    "ssh-ed25519 AAAAC3... primary@laptop",
    "ssh-rsa AAAAB3... backup@desktop",
    "ssh-ed25519 AAAAC3... automation@ci"
  ]
}
```

## Alternatives Considered

### 1. Guest Networking + Standard SSH

Run sshd inside each guest VM with dedicated IP addresses.

- **Pro:** Most familiar pattern for operators
- **Con:** Requires NAT port mapping, exposes guest network, complex firewall rules, potential port exhaustion
- **Verdict:** vsock is simpler and more secure

### 2. Serial Console

Use Firecracker's serial console feature.

- **Pro:** Simple, always available
- **Con:** Single session only, no multiplexing, poor UX for interactive use
- **Verdict:** Not suitable for production use

### 3. Custom Protocol over vsock

Build proprietary console protocol.

- **Pro:** Complete control, tailored to needs
- **Con:** Reinventing SSH, no ecosystem support, requires custom clients
- **Verdict:** SSH is proven and universal

### 4. HTTP/WebSocket Terminal

Serve terminal emulator in browser via WebSocket.

- **Pro:** No client installation, accessible from any browser
- **Con:** Requires custom client for CLI use, complex terminal handling, browser-only
- **Verdict:** SSH is more universal (works with standard tools, desktop apps, automation)

## Consequences

### Positive

- ✅ **Zero client installation:** Everyone has `ssh` command
- ✅ **Standard protocol:** Encryption, authentication built-in
- ✅ **Global access:** Works from anywhere on internet
- ✅ **Library support:** Mature ssh2 implementations in all major languages
- ✅ **Firewall friendly:** SSH typically allowed through corporate firewalls
- ✅ **PTY support:** vim, htop, interactive apps work correctly
- ✅ **Terminal resize:** Automatic via SSH protocol (SIGWINCH)
- ✅ **Multiple sessions:** socat fork mode allows concurrent connections
- ✅ **No guest networking:** VMs remain isolated, only vsock communication

### Negative

- ⚠️ **Per-VM key management:** Each VM has its own authorized keys (no centralized account keys)
- ⚠️ **Non-standard port:** 2222 instead of 22 (avoids conflict with host SSH)
- ⚠️ **Prototype limitations:** Current implementation has security gaps (see below)

### Current State (Prototype)

**Implemented:**
- ✅ **SSH key validation** - Validates against per-VM authorized keys
- ✅ **Per-VM authorized keys** - Stored in `VmResources.ssh_keys`
- ✅ SSH server binds to all interfaces (external access ready)
- ✅ PTY allocation and terminal resize
- ✅ Bidirectional I/O proxy
- ✅ Multiple concurrent sessions
- ⚠️ Ephemeral host key (generates on startup, not persisted)

**Not Implemented (Security Gaps):**
- ❌ HTTP API authentication - Anyone can create VMs
- ❌ Session recording
- ❌ Rate limiting
- ❌ Audit logging
- ⚠️ Persistent host key - Causes "host key changed" warnings on restart

### Production Requirements

Before exposing to public internet:

1. **Add HTTP API authentication** (OAuth, JWT, API keys) - High priority
2. **Persistent host key** - Prevents SSH fingerprint warnings on restart
3. **Session recording** for compliance and debugging
4. **Rate limiting** (connections per IP, per VM)
5. **Audit logging** (who connected, when, to which VM)
6. **Fail2ban integration** (optional - auto-block brute force attempts)

## Security Considerations

### Threat Model

| Threat | Mitigation (Current) | Mitigation (Required) |
|--------|---------------------|----------------------|
| Unauthorized VM access | ✅ Per-VM SSH key validation | ✅ Already implemented |
| MITM attack | ⚠️ Ephemeral host key (changes on restart) | ✅ Implement persistent host key |
| Brute force | ✅ Public key auth only | ⚠️ Optional: rate limiting |
| VM creation abuse | ❌ No API auth | ✅ Add HTTP authentication |
| Session replay | ❌ No recording | ⚠️ Optional (compliance-dependent) |
| DoS via connections | ❌ No limits | ✅ Rate limit per IP/VM |

### Risk Assessment

**Acceptable for Development:**
- Current implementation is suitable for trusted environments
- SSH encryption provides transport security
- Per-VM SSH key validation provides access control
- No sensitive data in prototype deployments
- Ephemeral host key is inconvenient but not a security risk

**Current Status (SSH Key Validation Implemented):**
- SSH key validation is functional and tested
- Per-VM authorized keys provide access control
- Suitable for trusted networks and development
- Still requires HTTP API authentication for production
- Ephemeral host key causes UX friction (fingerprint warnings) but does not impact security

**Not Acceptable for Production:**
- Must add HTTP API authentication before public internet exposure
- Should implement persistent host key for better UX
- Recommend session recording for compliance
- Recommend rate limiting for DoS protection

## Operational Recommendations

### Development/Testing

Current implementation is sufficient for:
- Local development on trusted networks
- Internal testing environments
- Proof-of-concept deployments

### Production

Before production deployment:

1. **Add HTTP API authentication** - Prevent unauthorized VM creation (top priority)
2. **Implement persistent host key** - Better UX, prevents fingerprint warnings
3. **Configure firewall rules** - Only allow necessary ports (3000, 2222)
4. **Monitor auth failures** - Set up alerts for suspicious activity
5. **Consider rate limiting** - Protect against abuse
6. **Optional: Session recording** - For compliance and auditing

### Filesystem Requirements

**Current:** No filesystem requirements (ephemeral key)

**Future (Persistent Host Key):**
- Directory: `/etc/warlock/ssh/` (will be created automatically)
- Permissions: 600 on private key, 644 on public key
- Backup recommended: Store private key in secure location to prevent fingerprint changes on host rebuild
