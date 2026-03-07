use std::net::Ipv4Addr;

use anyhow::{Context, bail};
use tracing::{error, info};

/// Handles returned by nftables when adding rules, used for cleanup.
#[derive(Debug, Clone, Copy)]
pub struct NatHandles {
    pub postrouting: u64,
    pub filter: u64,
}

/// Creates a tap device with the given name and assigns it an IP address.
///
/// Runs:
/// - `ip tuntap add <name> mode tap`
/// - `ip addr add <tap_ip>/30 dev <name>`
/// - `ip link set <name> up`
#[cfg(target_os = "linux")]
pub fn create_tap(name: &str, tap_ip: &Ipv4Addr) -> anyhow::Result<()> {
    run_cmd("ip", &["tuntap", "add", name, "mode", "tap"])
        .with_context(|| format!("Failed to create tap device '{}'", name))?;

    let addr = format!("{}/30", tap_ip);
    if let Err(e) = run_cmd("ip", &["addr", "add", &addr, "dev", name]) {
        // Roll back: delete the tap device we just created
        let _ = run_cmd("ip", &["link", "del", name]);
        return Err(e).with_context(|| format!("Failed to assign IP to tap '{}'", name));
    }

    if let Err(e) = run_cmd("ip", &["link", "set", name, "up"]) {
        let _ = run_cmd("ip", &["link", "del", name]);
        return Err(e).with_context(|| format!("Failed to bring up tap '{}'", name));
    }

    info!(tap = name, ip = %tap_ip, "Tap device created");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn create_tap(_name: &str, _tap_ip: &Ipv4Addr) -> anyhow::Result<()> {
    Ok(())
}

/// Deletes a tap device. Logs errors but does not fail — this is cleanup.
#[cfg(target_os = "linux")]
pub fn delete_tap(name: &str) {
    if let Err(e) = run_cmd("ip", &["link", "del", name]) {
        error!(tap = name, error = ?e, "Failed to delete tap device");
    } else {
        info!(tap = name, "Tap device deleted");
    }
}

#[cfg(not(target_os = "linux"))]
pub fn delete_tap(_name: &str) {}

/// Adds NAT and forwarding rules for a guest IP via nftables.
///
/// Adds two rules:
/// 1. `firecracker postrouting`: masquerade traffic from guest IP
/// 2. `firecracker filter`: forward traffic from tap to host interface
///
/// Returns the nftables handles for both rules (used for cleanup).
#[cfg(target_os = "linux")]
pub fn add_nat_rules(
    guest_ip: &Ipv4Addr,
    tap_name: &str,
    host_iface: &str,
) -> anyhow::Result<NatHandles> {
    // nft -ae echoes the rule with its handle, e.g.:
    //   add rule firecracker postrouting ... # handle 3
    let post_rule = format!(
        "add rule firecracker postrouting ip saddr {} oifname {} counter masquerade",
        guest_ip, host_iface
    );
    let post_output = run_cmd_output("nft", &["-ae", &post_rule])
        .context("Failed to add postrouting NAT rule")?;
    let postrouting =
        parse_nft_handle(&post_output).context("Failed to parse postrouting rule handle")?;

    let filter_rule = format!(
        "add rule firecracker filter iifname {} oifname {} accept",
        tap_name, host_iface
    );
    let filter_output = match run_cmd_output("nft", &["-ae", &filter_rule]) {
        Ok(output) => output,
        Err(e) => {
            // Roll back the postrouting rule
            let _ = run_cmd(
                "nft",
                &[
                    "delete",
                    "rule",
                    "firecracker",
                    "postrouting",
                    "handle",
                    &postrouting.to_string(),
                ],
            );
            return Err(e).context("Failed to add filter forwarding rule");
        }
    };
    let filter = parse_nft_handle(&filter_output).context("Failed to parse filter rule handle")?;

    info!(
        guest_ip = %guest_ip,
        tap = tap_name,
        postrouting_handle = postrouting,
        filter_handle = filter,
        "NAT rules added"
    );

    Ok(NatHandles {
        postrouting,
        filter,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn add_nat_rules(
    _guest_ip: &Ipv4Addr,
    _tap_name: &str,
    _host_iface: &str,
) -> anyhow::Result<NatHandles> {
    Ok(NatHandles {
        postrouting: 0,
        filter: 0,
    })
}

/// Removes NAT and forwarding rules by their nftables handles.
/// Logs errors but does not fail — this is cleanup.
#[cfg(target_os = "linux")]
pub fn remove_nat_rules(handles: &NatHandles) {
    if let Err(e) = run_cmd(
        "nft",
        &[
            "delete",
            "rule",
            "firecracker",
            "postrouting",
            "handle",
            &handles.postrouting.to_string(),
        ],
    ) {
        error!(handle = handles.postrouting, error = ?e, "Failed to remove postrouting rule");
    }

    if let Err(e) = run_cmd(
        "nft",
        &[
            "delete",
            "rule",
            "firecracker",
            "filter",
            "handle",
            &handles.filter.to_string(),
        ],
    ) {
        error!(handle = handles.filter, error = ?e, "Failed to remove filter rule");
    }

    info!(
        postrouting = handles.postrouting,
        filter = handles.filter,
        "NAT rules removed"
    );
}

#[cfg(not(target_os = "linux"))]
pub fn remove_nat_rules(_handles: &NatHandles) {}

/// Detects the host's outward-facing network interface from the default route.
///
/// Parses `ip route show default` for the `dev` field.
#[cfg(target_os = "linux")]
pub fn detect_host_interface() -> anyhow::Result<String> {
    let output = run_cmd_output("ip", &["route", "show", "default"])
        .context("Failed to query default route")?;

    // Output format: "default via 10.0.0.1 dev eth0 proto ..."
    let iface = output
        .split_whitespace()
        .skip_while(|&w| w != "dev")
        .nth(1)
        .map(String::from);

    match iface {
        Some(name) => {
            info!(interface = %name, "Detected host network interface");
            Ok(name)
        }
        None => bail!("Could not determine host network interface from default route"),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn detect_host_interface() -> anyhow::Result<String> {
    Ok("eth0".into())
}

// ── Helpers ──

/// Runs a command and returns an error if it fails.
fn run_cmd(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("Failed to execute '{}'", program))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{} {} failed: {}", program, args.join(" "), stderr.trim());
    }

    Ok(())
}

/// Runs a command and returns its stdout as a string.
fn run_cmd_output(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("Failed to execute '{}'", program))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{} {} failed: {}", program, args.join(" "), stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Parses an nftables handle from the output of `nft -ae add rule ...`.
///
/// The output contains a line like: `... # handle 42`
fn parse_nft_handle(output: &str) -> anyhow::Result<u64> {
    // Look for "# handle <N>" anywhere in the output
    for line in output.lines() {
        if let Some(pos) = line.find("# handle ") {
            let handle_str = line[pos + 9..].trim();
            return handle_str
                .parse::<u64>()
                .with_context(|| format!("Failed to parse nft handle from: {}", handle_str));
        }
    }

    bail!("No handle found in nft output: {}", output.trim());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_nft_handle_from_output() {
        let output = "add rule firecracker postrouting ip saddr 172.16.0.2 oifname eth0 counter masquerade # handle 42\n";
        assert_eq!(parse_nft_handle(output).unwrap(), 42);
    }

    #[test]
    fn parse_nft_handle_multiline() {
        let output = "table firecracker {\n  chain postrouting {\n    # handle 7\n  }\n}\nadd rule ... # handle 99\n";
        // Should find the last "# handle" — but our impl finds the first
        assert_eq!(parse_nft_handle(output).unwrap(), 7);
    }

    #[test]
    fn parse_nft_handle_missing() {
        let output = "some random output without a handle\n";
        assert!(parse_nft_handle(output).is_err());
    }
}
