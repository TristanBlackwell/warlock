use anyhow::{bail, Context, Result};
use semver::Version;
use std::path::PathBuf;
use std::process::Command;
use tracing::{info, warn};

const MIN_FIRECRACKER_VERSION: &str = "1.14.1";

/// UID/GID for the jailed Firecracker process.
pub const JAILER_UID: usize = 1100;
pub const JAILER_GID: usize = 1100;

/// Strategy for creating per-VM rootfs copies, detected at startup.
#[derive(Debug, Clone)]
pub enum CopyStrategy {
    /// Filesystem supports reflinks (btrfs, XFS). Instant copy-on-write.
    Reflink,
    /// Fallback: sparse copy. Reads source but skips zero blocks.
    Sparse,
}

/// Configuration determined at startup for the jailer.
#[derive(Debug, Clone)]
pub struct JailerConfig {
    /// Detected cgroup version (1 or 2).
    pub cgroup_version: usize,
    /// Absolute path to the Firecracker binary.
    pub firecracker_path: PathBuf,
    /// Absolute path to the jailer binary.
    pub jailer_path: PathBuf,
    /// Detected strategy for copying rootfs images per VM.
    pub copy_strategy: CopyStrategy,
}

/// Runs all pre-flight checks for Firecracker and jailer availability.
///
/// Returns a `JailerConfig` containing runtime-detected settings (e.g. cgroup
/// version). In dev mode, returns a default config and skips all checks.
///
/// # Errors
///
/// Returns an error if any required component is missing or misconfigured.
pub fn preflight_check() -> Result<JailerConfig> {
    if is_dev_mode() {
        warn!("Running in development mode - skipping Firecracker checks");
        return Ok(JailerConfig {
            cgroup_version: 2,
            firecracker_path: PathBuf::from("firecracker"),
            jailer_path: PathBuf::from("jailer"),
            copy_strategy: CopyStrategy::Sparse,
        });
    }

    info!("Running Firecracker pre-flight checks...");

    // Check Firecracker binary and version, resolve absolute path
    let (version_output, firecracker_path) = check_firecracker_binary()?;
    let version = parse_and_validate_version(&version_output)?;
    info!(
        "Found Firecracker: v{} at {}",
        version,
        firecracker_path.display()
    );

    // Check jailer binary, resolve absolute path
    let jailer_path = check_jailer_binary()?;

    // Check KVM availability (Linux only)
    check_kvm_available()?;

    // Verify the firecracker system user exists
    check_jailer_user()?;

    // Verify assets and jailer chroot are on the same filesystem
    check_jailer_filesystem()?;

    // Check vm-images directory exists
    check_vm_images_dir()?;

    // Detect cgroup version
    let cgroup_version = detect_cgroup_version();
    info!("Detected cgroup version: v{}", cgroup_version);

    // Detect best rootfs copy strategy for the filesystem
    let copy_strategy = detect_copy_strategy();
    match copy_strategy {
        CopyStrategy::Reflink => info!("Rootfs copy strategy: reflink (instant CoW)"),
        CopyStrategy::Sparse => info!("Rootfs copy strategy: sparse copy (no reflink support)"),
    }

    info!("Firecracker pre-flight checks passed");
    Ok(JailerConfig {
        cgroup_version,
        firecracker_path,
        jailer_path,
        copy_strategy,
    })
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

/// Resolves a binary name to its absolute path.
///
/// Checks the given environment variable for an explicit path first, then
/// falls back to searching PATH via `which`. Returns the canonical absolute
/// path to the binary.
fn resolve_binary_path(env_var: &str, default_name: &str) -> Result<PathBuf> {
    let name = std::env::var(env_var).unwrap_or_else(|_| default_name.to_string());
    let path = PathBuf::from(&name);

    // If the user gave us an absolute path, trust it
    if path.is_absolute() {
        return Ok(path);
    }

    // Otherwise resolve via PATH
    which::which(&name).with_context(|| {
        format!(
            "Could not find '{}' in PATH. \
             Set `{}` environment variable to the absolute path.",
            name, env_var
        )
    })
}

/// Checks for Firecracker binary and returns its version output and resolved
/// absolute path.
///
/// First checks `FIRECRACKER_BIN` environment variable for a custom path,
/// otherwise falls back to "firecracker" in PATH.
///
/// # Errors
///
/// Returns an error if:
/// - The binary cannot be found or executed
/// - The version command fails
fn check_firecracker_binary() -> Result<(String, PathBuf)> {
    let firecracker_path = resolve_binary_path("FIRECRACKER_BIN", "firecracker")?;

    let output = Command::new(&firecracker_path)
        .arg("--version")
        .output()
        .with_context(|| {
            format!(
                "Failed to execute Firecracker binary at '{}'. \
                 Install Firecracker from https://github.com/firecracker-microvm/firecracker \
                 or set `FIRECRACKER_BIN` environment variable to the correct path.",
                firecracker_path.display()
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

    Ok((version_output, firecracker_path))
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

/// Checks that the jailer binary exists and is executable. Returns the
/// resolved absolute path.
fn check_jailer_binary() -> Result<PathBuf> {
    let jailer_path = resolve_binary_path("JAILER_BIN", "jailer")?;

    let output = Command::new(&jailer_path)
        .arg("--version")
        .output()
        .with_context(|| {
            format!(
                "Failed to execute jailer binary at '{}'. \
                 The jailer is included in Firecracker releases. \
                 Set `JAILER_BIN` environment variable if it is installed elsewhere.",
                jailer_path.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Jailer execution failed: {}", stderr);
    }

    info!("Jailer binary available at {}", jailer_path.display());
    Ok(jailer_path)
}

/// Verifies that the `firecracker` system user (uid 1100) exists.
#[cfg(target_os = "linux")]
fn check_jailer_user() -> Result<()> {
    use std::io::BufRead;

    let file = std::fs::File::open("/etc/passwd").context("Failed to read /etc/passwd")?;
    let reader = std::io::BufReader::new(file);

    let uid_str = JAILER_UID.to_string();
    for line in reader.lines() {
        let line = line?;
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 3 && fields[2] == uid_str {
            info!("Jailer user found: {} (uid {})", fields[0], JAILER_UID);
            return Ok(());
        }
    }

    bail!(
        "No user with uid {} found. Run the install-firecracker.sh script to create \
         the 'firecracker' system user.",
        JAILER_UID,
    );
}

#[cfg(not(target_os = "linux"))]
fn check_jailer_user() -> Result<()> {
    Ok(())
}

/// Verifies that /opt/firecracker and /srv/jailer are on the same filesystem
/// (required for hard-linking assets into the jailer chroot).
#[cfg(target_os = "linux")]
fn check_jailer_filesystem() -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    let assets_dir = Path::new("/opt/firecracker");
    let jailer_dir = Path::new("/srv/jailer");

    if !assets_dir.exists() {
        bail!(
            "/opt/firecracker does not exist. Run install-firecracker.sh to install \
             Firecracker assets."
        );
    }

    if !jailer_dir.exists() {
        bail!(
            "/srv/jailer does not exist. Run install-firecracker.sh to create the \
             jailer chroot base directory."
        );
    }

    let assets_dev = std::fs::metadata(assets_dir)
        .context("Failed to stat /opt/firecracker")?
        .dev();
    let jailer_dev = std::fs::metadata(jailer_dir)
        .context("Failed to stat /srv/jailer")?
        .dev();

    if assets_dev != jailer_dev {
        bail!(
            "/opt/firecracker and /srv/jailer are on different filesystems. \
             The jailer uses hard links, which cannot cross filesystem boundaries. \
             Move assets to the same filesystem as /srv/jailer."
        );
    }

    info!("Filesystem check passed (assets and jailer chroot on same device)");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn check_jailer_filesystem() -> Result<()> {
    Ok(())
}

/// Directory for per-VM rootfs copies.
pub const VM_IMAGES_DIR: &str = "/srv/jailer/vm-images";

/// Verifies that the vm-images directory exists.
#[cfg(target_os = "linux")]
fn check_vm_images_dir() -> Result<()> {
    use std::path::Path;

    let dir = Path::new(VM_IMAGES_DIR);
    if !dir.exists() {
        bail!(
            "{} does not exist. Run install-firecracker.sh to create it.",
            VM_IMAGES_DIR
        );
    }

    info!("VM images directory: {}", VM_IMAGES_DIR);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn check_vm_images_dir() -> Result<()> {
    Ok(())
}

/// Detects whether the filesystem at `/srv/jailer` supports reflink copies.
///
/// Creates a small probe file and attempts `cp --reflink=always`. If it
/// succeeds, the filesystem supports copy-on-write (btrfs, XFS with reflinks).
/// Falls back to sparse copies otherwise.
#[cfg(target_os = "linux")]
fn detect_copy_strategy() -> CopyStrategy {
    use std::path::Path;

    let probe_src = Path::new(VM_IMAGES_DIR).join(".warlock-probe");
    let probe_dst = Path::new(VM_IMAGES_DIR).join(".warlock-probe.reflink");

    // Clean up any stale probe files
    let _ = std::fs::remove_file(&probe_src);
    let _ = std::fs::remove_file(&probe_dst);

    // Create a small probe file
    if std::fs::write(&probe_src, b"probe").is_err() {
        return CopyStrategy::Sparse;
    }

    let result = Command::new("cp")
        .arg("--reflink=always")
        .arg(&probe_src)
        .arg(&probe_dst)
        .output();

    // Clean up
    let _ = std::fs::remove_file(&probe_src);
    let _ = std::fs::remove_file(&probe_dst);

    match result {
        Ok(output) if output.status.success() => CopyStrategy::Reflink,
        _ => CopyStrategy::Sparse,
    }
}

#[cfg(not(target_os = "linux"))]
fn detect_copy_strategy() -> CopyStrategy {
    CopyStrategy::Sparse
}

/// Detects whether the host uses cgroup v1 or v2.
///
/// Checks for the cgroup2 unified hierarchy at `/sys/fs/cgroup/cgroup.controllers`.
/// Falls back to v1 if not found.
#[cfg(target_os = "linux")]
fn detect_cgroup_version() -> usize {
    if std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        2
    } else {
        1
    }
}

#[cfg(not(target_os = "linux"))]
fn detect_cgroup_version() -> usize {
    2 // Default for non-Linux (dev mode)
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
