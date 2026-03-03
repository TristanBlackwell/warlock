//! Orphan detection and cleanup for stale resources from a previous instance.
//!
//! When Warlock crashes (SIGKILL, OOM, host reboot), the graceful shutdown
//! handler never runs. This leaves behind orphaned Firecracker processes,
//! stale jailer workspaces, rootfs copies, tap devices, and nftables rules.
//!
//! This module performs a **clean sweep** on startup: all orphaned resources
//! are destroyed. Re-adoption of running VMs is not supported — this is a
//! deliberate simplification for a single-node control plane with no persistent
//! state. Persistence can be layered on later.
//!
//! The cleanup runs after preflight checks pass (so tools like `ip`, `nft`,
//! `pkill` are known to be available) and before the server accepts requests.

use std::path::Path;

use tracing::info;
#[cfg(target_os = "linux")]
use tracing::warn;

/// Scans for and removes orphaned resources from a previous Warlock instance.
///
/// This is a best-effort operation: individual cleanup steps log warnings on
/// failure but do not prevent the server from starting. The only hard failure
/// is if we cannot enumerate resources at all (e.g. `/srv/jailer` is missing),
/// which would have already been caught by preflight checks.
///
/// # Cleanup order
///
/// 1. Kill orphaned Firecracker processes (SIGTERM, wait, SIGKILL)
/// 2. Remove stale jailer workspaces (`/srv/jailer/firecracker/*/`)
/// 3. Remove stale per-VM rootfs copies (`{vm_images_dir}/*.ext4`)
/// 4. Remove orphaned `fc*` tap devices
/// 5. Flush nftables rules from the `firecracker` table
/// 6. Remove stale jailer stderr logs (`/tmp/warlock-jailer-*.log`)
pub fn cleanup_orphans(vm_images_dir: &Path) {
    let mut found_anything = false;

    if kill_orphaned_processes() {
        found_anything = true;
    }
    if remove_stale_jailer_workspaces() {
        found_anything = true;
    }
    if remove_stale_rootfs_copies(vm_images_dir) {
        found_anything = true;
    }
    if remove_stale_tap_devices() {
        found_anything = true;
    }
    if flush_nftables_rules() {
        found_anything = true;
    }
    if remove_stale_jailer_logs() {
        found_anything = true;
    }

    if found_anything {
        info!("Orphan cleanup complete");
    } else {
        info!("No orphaned resources found");
    }
}

// ── Process cleanup ──

/// Kills orphaned Firecracker processes owned by the jailer user (uid 1100).
///
/// Sends SIGTERM first, waits briefly, then sends SIGKILL for stragglers.
/// Returns `true` if any processes were found and killed.
#[cfg(target_os = "linux")]
fn kill_orphaned_processes() -> bool {
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    let uid = super::JAILER_UID.to_string();

    // SIGTERM — graceful shutdown
    let term_result = Command::new("pkill")
        .args(["-15", "-u", &uid, "firecracker"])
        .output();

    let had_processes = match term_result {
        Ok(output) => output.status.success(), // exit 0 = matched processes
        Err(e) => {
            warn!(error = ?e, "Failed to run pkill for orphaned Firecracker processes");
            return false;
        }
    };

    if !had_processes {
        return false;
    }

    info!("Sent SIGTERM to orphaned Firecracker processes, waiting for exit...");
    thread::sleep(Duration::from_secs(2));

    // SIGKILL — force kill any stragglers
    let kill_result = Command::new("pkill")
        .args(["-9", "-u", &uid, "firecracker"])
        .output();

    match kill_result {
        Ok(output) if output.status.success() => {
            warn!("Had to SIGKILL orphaned Firecracker processes that didn't exit on SIGTERM");
        }
        _ => {
            info!("All orphaned Firecracker processes exited cleanly after SIGTERM");
        }
    }

    true
}

#[cfg(not(target_os = "linux"))]
fn kill_orphaned_processes() -> bool {
    false
}

// ── Jailer workspace cleanup ──

/// Removes stale jailer workspace directories under `/srv/jailer/firecracker/`.
///
/// Each jailed VM creates a directory at `/srv/jailer/firecracker/{vm_id}/`.
/// After an unclean shutdown these persist with hard-linked kernel, rootfs,
/// binary, socket, PID file, and log files inside.
///
/// Returns `true` if any workspaces were found and removed.
#[cfg(target_os = "linux")]
fn remove_stale_jailer_workspaces() -> bool {
    let jailer_dir = Path::new("/srv/jailer/firecracker");

    let entries = match std::fs::read_dir(jailer_dir) {
        Ok(entries) => entries,
        Err(e) => {
            // Not an error if the directory doesn't exist — could be a fresh install
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(error = ?e, "Failed to read jailer workspace directory");
            }
            return false;
        }
    };

    let mut count = 0;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = ?e, "Failed to read jailer workspace entry");
                continue;
            }
        };

        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        if let Err(e) = std::fs::remove_dir_all(&path) {
            warn!(path = %path.display(), error = ?e, "Failed to remove stale jailer workspace");
        } else {
            count += 1;
        }
    }

    if count > 0 {
        info!(count, "Removed stale jailer workspaces");
    }
    count > 0
}

#[cfg(not(target_os = "linux"))]
fn remove_stale_jailer_workspaces() -> bool {
    false
}

// ── Rootfs copy cleanup ──

/// Removes stale per-VM rootfs copies from the vm-images directory.
///
/// Each VM gets a copy at `{vm_images_dir}/{vm_id}.ext4`. These can be large
/// (hundreds of MB to several GB) and accumulate after unclean shutdowns.
///
/// Returns `true` if any copies were found and removed.
#[cfg(target_os = "linux")]
fn remove_stale_rootfs_copies(vm_images_dir: &Path) -> bool {
    let entries = match std::fs::read_dir(vm_images_dir) {
        Ok(entries) => entries,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(path = %vm_images_dir.display(), error = ?e, "Failed to read VM images directory");
            }
            return false;
        }
    };

    let mut count = 0;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = ?e, "Failed to read VM images entry");
                continue;
            }
        };

        let path = entry.path();
        let is_ext4 = path.extension().map(|ext| ext == "ext4").unwrap_or(false);

        if !is_ext4 {
            continue;
        }

        if let Err(e) = std::fs::remove_file(&path) {
            warn!(path = %path.display(), error = ?e, "Failed to remove stale rootfs copy");
        } else {
            count += 1;
        }
    }

    if count > 0 {
        info!(count, "Removed stale rootfs copies");
    }
    count > 0
}

#[cfg(not(target_os = "linux"))]
fn remove_stale_rootfs_copies(_vm_images_dir: &Path) -> bool {
    false
}

// ── Tap device cleanup ──

/// Removes orphaned `fc*` tap devices.
///
/// Parses `ip -o link show` output to find interfaces named `fc0`, `fc1`, etc.
/// and deletes them. Returns `true` if any were found and removed.
#[cfg(target_os = "linux")]
fn remove_stale_tap_devices() -> bool {
    use std::process::Command;

    let output = match Command::new("ip").args(["-o", "link", "show"]).output() {
        Ok(o) => o,
        Err(e) => {
            warn!(error = ?e, "Failed to list network interfaces");
            return false;
        }
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let tap_names = parse_fc_tap_devices(&text);

    if tap_names.is_empty() {
        return false;
    }

    let mut count = 0;
    for name in &tap_names {
        match Command::new("ip").args(["link", "del", name]).output() {
            Ok(o) if o.status.success() => count += 1,
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                warn!(tap = name, error = %stderr.trim(), "Failed to delete stale tap device");
            }
            Err(e) => {
                warn!(tap = name, error = ?e, "Failed to run ip link del");
            }
        }
    }

    if count > 0 {
        info!(count, "Removed stale tap devices");
    }
    count > 0
}

#[cfg(not(target_os = "linux"))]
fn remove_stale_tap_devices() -> bool {
    false
}

/// Parses `ip -o link show` output and returns names of `fc*` tap devices.
///
/// Output format: `N: fc0: <BROADCAST,MULTICAST,UP> ...`
/// Each line starts with an index, then the interface name with a trailing colon.
fn parse_fc_tap_devices(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let name = line.split_whitespace().nth(1)?;
            let name = name.trim_end_matches(':');
            if name.starts_with("fc") && name[2..].chars().all(|c| c.is_ascii_digit()) {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect()
}

// ── nftables cleanup ──

/// Flushes all rules from the nftables `firecracker` table.
///
/// This preserves the table and chain structure (postrouting, filter) but
/// removes all per-VM masquerade and forwarding rules. Returns `true` if
/// rules were present and flushed.
#[cfg(target_os = "linux")]
fn flush_nftables_rules() -> bool {
    use std::process::Command;

    // First check if the table has any rules worth flushing by listing it
    let list_output = match Command::new("nft")
        .args(["list", "table", "firecracker"])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return false, // Table doesn't exist or can't be listed
    };

    // Check if there are any actual rules (not just chain declarations).
    // A chain with rules will have lines that don't start with "table", "chain",
    // "type", or "}" — e.g. "ip saddr 172.16.0.2 ..."
    let has_rules = has_nftables_rules(&list_output);

    if !has_rules {
        return false;
    }

    match Command::new("nft")
        .args(["flush", "table", "firecracker"])
        .output()
    {
        Ok(o) if o.status.success() => {
            info!("Flushed stale nftables rules from firecracker table");
            true
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!(error = %stderr.trim(), "Failed to flush nftables firecracker table");
            false
        }
        Err(e) => {
            warn!(error = ?e, "Failed to run nft flush");
            false
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn flush_nftables_rules() -> bool {
    false
}

/// Checks whether `nft list table` output contains actual rules (not just
/// table/chain structure).
///
/// A table with only empty chains looks like:
/// ```text
/// table ip firecracker {
///     chain postrouting {
///         type nat hook postrouting priority srcnat; policy accept;
///     }
///     chain filter {
///         type filter hook forward priority filter; policy accept;
///     }
/// }
/// ```
///
/// Rules appear as additional indented lines within chains, e.g.:
/// ```text
///     ip saddr 172.16.0.2 oifname "eth0" counter masquerade
/// ```
fn has_nftables_rules(output: &str) -> bool {
    for line in output.lines() {
        let trimmed = line.trim();
        // Skip structural lines
        if trimmed.is_empty()
            || trimmed.starts_with("table ")
            || trimmed.starts_with("chain ")
            || trimmed.starts_with("type ")
            || trimmed == "}"
            || trimmed == "{"
        {
            continue;
        }
        // Skip policy lines (part of chain declaration)
        // These appear as: "type nat hook postrouting priority srcnat; policy accept;"
        // But we already skip "type " lines above. The only other structural
        // content is "}" and chain/table headers.
        // Anything else is an actual rule.
        return true;
    }
    false
}

// ── Jailer log cleanup ──

/// Removes stale jailer stderr logs from `/tmp/`.
///
/// Each VM creates a log at `/tmp/warlock-jailer-{vm_id}.log`. These are small
/// but accumulate. Returns `true` if any were found and removed.
#[cfg(target_os = "linux")]
fn remove_stale_jailer_logs() -> bool {
    let tmp = Path::new("/tmp");

    let entries = match std::fs::read_dir(tmp) {
        Ok(entries) => entries,
        Err(e) => {
            warn!(error = ?e, "Failed to read /tmp for jailer log cleanup");
            return false;
        }
    };

    let mut count = 0;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let name = entry.file_name();
        let name = name.to_string_lossy();

        if name.starts_with("warlock-jailer-") && name.ends_with(".log") {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                warn!(path = %entry.path().display(), error = ?e, "Failed to remove stale jailer log");
            } else {
                count += 1;
            }
        }
    }

    if count > 0 {
        info!(count, "Removed stale jailer logs");
    }
    count > 0
}

#[cfg(not(target_os = "linux"))]
fn remove_stale_jailer_logs() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_fc_tap_devices ──

    #[test]
    fn parse_tap_devices_from_ip_output() {
        let output = "\
1: lo: <LOOPBACK,UP,LOWER_UP> mtu 65536 qdisc noqueue state UNKNOWN mode DEFAULT group default qlen 1000\\    link/loopback 00:00:00:00:00:00 brd 00:00:00:00:00:00
2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc fq_codel state UP mode DEFAULT group default qlen 1000\\    link/ether 12:34:56:78:9a:bc brd ff:ff:ff:ff:ff:ff
3: fc0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc fq_codel state UP mode DEFAULT group default qlen 1000\\    link/ether aa:bb:cc:dd:ee:ff brd ff:ff:ff:ff:ff:ff
4: fc1: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc fq_codel state UP mode DEFAULT group default qlen 1000\\    link/ether 11:22:33:44:55:66 brd ff:ff:ff:ff:ff:ff
";
        let devices = parse_fc_tap_devices(output);
        assert_eq!(devices, vec!["fc0", "fc1"]);
    }

    #[test]
    fn parse_tap_devices_no_fc_interfaces() {
        let output = "\
1: lo: <LOOPBACK,UP,LOWER_UP> mtu 65536 qdisc noqueue state UNKNOWN
2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc fq_codel state UP
";
        let devices = parse_fc_tap_devices(output);
        assert!(devices.is_empty());
    }

    #[test]
    fn parse_tap_devices_ignores_non_fc_prefixed() {
        // "fcc0" doesn't match because the suffix after "fc" must be all digits
        // "fc_0" doesn't match because '_' is not a digit
        let output = "\
1: lo: <LOOPBACK> mtu 65536
2: fcc0: <BROADCAST> mtu 1500
3: fc_0: <BROADCAST> mtu 1500
4: fc42: <BROADCAST> mtu 1500
";
        let devices = parse_fc_tap_devices(output);
        assert_eq!(devices, vec!["fc42"]);
    }

    #[test]
    fn parse_tap_devices_empty_output() {
        let devices = parse_fc_tap_devices("");
        assert!(devices.is_empty());
    }

    #[test]
    fn parse_tap_devices_high_index() {
        let output = "5: fc16383: <BROADCAST,MULTICAST> mtu 1500 qdisc noop state DOWN\n";
        let devices = parse_fc_tap_devices(output);
        assert_eq!(devices, vec!["fc16383"]);
    }

    // ── has_nftables_rules ──

    #[test]
    fn nftables_empty_table_has_no_rules() {
        let output = "\
table ip firecracker {
    chain postrouting {
        type nat hook postrouting priority srcnat; policy accept;
    }
    chain filter {
        type filter hook forward priority filter; policy accept;
    }
}
";
        assert!(!has_nftables_rules(output));
    }

    #[test]
    fn nftables_table_with_masquerade_rule() {
        let output = "\
table ip firecracker {
    chain postrouting {
        type nat hook postrouting priority srcnat; policy accept;
        ip saddr 172.16.0.2 oifname \"eth0\" counter packets 0 bytes 0 masquerade
    }
    chain filter {
        type filter hook forward priority filter; policy accept;
    }
}
";
        assert!(has_nftables_rules(output));
    }

    #[test]
    fn nftables_table_with_filter_rule() {
        let output = "\
table ip firecracker {
    chain postrouting {
        type nat hook postrouting priority srcnat; policy accept;
    }
    chain filter {
        type filter hook forward priority filter; policy accept;
        iifname \"fc0\" oifname \"eth0\" accept
    }
}
";
        assert!(has_nftables_rules(output));
    }

    #[test]
    fn nftables_empty_output() {
        assert!(!has_nftables_rules(""));
    }
}
