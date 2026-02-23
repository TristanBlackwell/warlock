use anyhow::{bail, Context, Result};
use semver::Version;
use std::process::Command;
use tracing::{info, warn};

const MIN_FIRECRACKER_VERSION: &str = "1.14.1";

/// Runs all pre-flight checks for Firecracker availability.
///
/// This function checks:
/// - If running in development mode (skips checks if so)
/// - Firecracker binary availability and version
/// - KVM device availability (Linux only)
///
/// # Errors
///
/// Returns an error if:
/// - Firecracker binary is not found
/// - Firecracker version is below minimum required
/// - KVM device is not available (Linux only)
pub fn preflight_check() -> Result<()> {
    if is_dev_mode() {
        warn!("Running in development mode - skipping Firecracker checks");
        return Ok(());
    }

    info!("Running Firecracker pre-flight checks...");

    // Check Firecracker binary and version
    let version_output = check_firecracker_binary()?;
    let version = parse_and_validate_version(&version_output)?;
    info!("Found Firecracker: v{}", version);

    // Check KVM availability (Linux only)
    check_kvm_available()?;

    info!("Firecracker pre-flight checks passed");
    Ok(())
}

/// Checks if running in development mode.
///
/// Development mode is enabled if:
/// - WARLOCK_DEV environment variable is set to "true"
/// - RUST_ENV environment variable is set to "development"
fn is_dev_mode() -> bool {
    std::env::var("WARLOCK_DEV")
        .map(|v| v == "true")
        .unwrap_or(false)
        || std::env::var("RUST_ENV")
            .map(|v| v == "development")
            .unwrap_or(false)
}

/// Checks for Firecracker binary and returns its version output.
///
/// First checks `FIRECRACKER_BIN` environment variable for a custom path,
/// otherwise falls back to "firecracker" in PATH.
///
/// # Errors
///
/// Returns an error if:
/// - The binary cannot be found or executed
/// - The version command fails
fn check_firecracker_binary() -> Result<String> {
    let firecracker_bin =
        std::env::var("FIRECRACKER_BIN").unwrap_or_else(|_| "firecracker".to_string());

    let output = Command::new(&firecracker_bin)
        .arg("--version")
        .output()
        .with_context(|| {
            format!(
                "Failed to execute Firecracker binary at '{}'. \
                 Install Firecracker from https://github.com/firecracker-microvm/firecracker \
                 or set `FIRECRACKER_BIN` environment variable to the correct path.",
                firecracker_bin
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Firecracker execution failed: {}", stderr);
    }

    let version_output = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if version_output.is_empty() {
        bail!("Firecracker version output was empty");
    }

    Ok(version_output)
}

/// Parses Firecracker version output and validates it meets minimum requirements.
///
/// # Errors
///
/// Returns an error if:
/// - Version string cannot be parsed
/// - Version is below minimum required version
fn parse_and_validate_version(version_output: &str) -> Result<Version> {
    // Firecracker outputs format like "Firecracker v1.14.1" or "v1.14.1" on the first line
    // Additional lines may contain exit messages and timestamps - ignore them
    let first_line = version_output
        .lines()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Version output was empty"))?;

    // Extract version number from the line (might be "Firecracker v1.14.1" or just "v1.14.1")
    // Split by whitespace and find the part that looks like a version
    let version_str = first_line
        .split_whitespace()
        .find(|s| s.starts_with('v') || s.chars().next().map_or(false, |c| c.is_ascii_digit()))
        .ok_or_else(|| anyhow::anyhow!("No version number found in output: {}", first_line))?
        .trim_start_matches('v');

    let version =
        Version::parse(version_str).context("Failed to parse Firecracker version string")?;

    let min_version =
        Version::parse(MIN_FIRECRACKER_VERSION).expect("MIN_FIRECRACKER_VERSION is invalid");

    if version < min_version {
        bail!(
            "Firecracker version {} is too old. Minimum required: {}",
            version,
            MIN_FIRECRACKER_VERSION
        );
    }

    Ok(version)
}

/// Checks if KVM device is available (Linux only).
///
/// # Errors
///
/// Returns an error if:
/// - /dev/kvm does not exist
/// - /dev/kvm cannot be accessed
#[cfg(target_os = "linux")]
fn check_kvm_available() -> Result<()> {
    use std::path::Path;

    let kvm_path = Path::new("/dev/kvm");

    if !kvm_path.exists() {
        bail!(
            "KVM device not found at /dev/kvm. \
             Ensure KVM is enabled in your kernel and BIOS. \
             On Ubuntu/Debian, try: sudo modprobe kvm kvm_intel (or kvm_amd for AMD CPUs)"
        );
    }

    // Check if we can access the device
    if let Err(e) = std::fs::metadata(kvm_path) {
        bail!(
            "Cannot access /dev/kvm: {}. \
             You may need to add your user to the kvm group: \
             sudo usermod -aG kvm $USER && newgrp kvm",
            e
        );
    }

    info!("KVM device available");
    Ok(())
}

/// Stub for non-Linux platforms - KVM check is skipped.
#[cfg(not(target_os = "linux"))]
fn check_kvm_available() -> Result<()> {
    // KVM is Linux-specific, so we skip this check on other platforms
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_with_v_prefix() {
        let result = parse_and_validate_version("v1.14.1");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 14, 1));
    }

    #[test]
    fn test_parse_version_without_v_prefix() {
        let result = parse_and_validate_version("1.14.1");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 14, 1));
    }

    #[test]
    fn test_parse_version_with_whitespace() {
        let result = parse_and_validate_version("  v1.14.1\n");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 14, 1));
    }

    #[test]
    fn test_version_meets_minimum() {
        let result = parse_and_validate_version("v1.14.1");
        assert!(result.is_ok());
    }

    #[test]
    fn test_version_exceeds_minimum() {
        let result = parse_and_validate_version("v1.15.0");
        assert!(result.is_ok());
    }

    #[test]
    fn test_version_below_minimum() {
        let result = parse_and_validate_version("v1.10.0");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too old"));
    }

    #[test]
    fn test_version_exact_minimum() {
        let result = parse_and_validate_version("v1.14.1");
        assert!(result.is_ok());
    }

    #[test]
    fn test_invalid_version_format() {
        let result = parse_and_validate_version("invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_version_with_multiline_output() {
        // Real Firecracker output includes exit messages after version
        let output = "Firecracker v1.14.1\n\n2026-02-23T21:05:34.876998360 [anonymous-instance:main] Firecracker exiting successfully. exit_code=0\n";
        let result = parse_and_validate_version(output);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 14, 1));
    }

    #[test]
    fn test_parse_version_with_extra_lines() {
        let output = "v1.15.0\nSome other output\nMore lines here";
        let result = parse_and_validate_version(output);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 15, 0));
    }

    #[test]
    fn test_parse_version_single_line_still_works() {
        let result = parse_and_validate_version("v1.14.1");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Version::new(1, 14, 1));
    }

    // NOTE: All env var assertions live in a single test to avoid race conditions.
    // Rust runs tests in parallel and env vars are global process state, so
    // separate tests that mutate env vars will interfere with each other.
    #[test]
    fn test_dev_mode_detection() {
        // Save original values
        let original_warlock_dev = std::env::var("WARLOCK_DEV").ok();
        let original_rust_env = std::env::var("RUST_ENV").ok();

        // Clear both to ensure clean test state
        unsafe {
            std::env::remove_var("WARLOCK_DEV");
            std::env::remove_var("RUST_ENV");
        }

        // Neither set - not dev mode
        assert!(!is_dev_mode());

        // WARLOCK_DEV=true - dev mode
        unsafe { std::env::set_var("WARLOCK_DEV", "true") }
        assert!(is_dev_mode());

        // WARLOCK_DEV=false - not dev mode
        unsafe { std::env::set_var("WARLOCK_DEV", "false") }
        assert!(!is_dev_mode());

        // RUST_ENV=development (with WARLOCK_DEV cleared) - dev mode
        unsafe {
            std::env::remove_var("WARLOCK_DEV");
            std::env::set_var("RUST_ENV", "development");
        }
        assert!(is_dev_mode());

        // RUST_ENV=production - not dev mode
        unsafe { std::env::set_var("RUST_ENV", "production") }
        assert!(!is_dev_mode());

        // Restore original values
        match original_warlock_dev {
            Some(val) => unsafe { std::env::set_var("WARLOCK_DEV", val) },
            None => unsafe { std::env::remove_var("WARLOCK_DEV") },
        }
        match original_rust_env {
            Some(val) => unsafe { std::env::set_var("RUST_ENV", val) },
            None => unsafe { std::env::remove_var("RUST_ENV") },
        }
    }
}
