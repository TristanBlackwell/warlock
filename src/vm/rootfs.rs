use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context};
use tracing::error;
use uuid::Uuid;

/// Strategy for creating per-VM rootfs copies.
///
/// Mirrors `firecracker::CopyStrategy` but kept here to avoid coupling the
/// domain layer to the infrastructure module. The handler maps between them.
#[derive(Debug, Clone)]
pub enum CopyStrategy {
    /// Filesystem supports reflinks (btrfs, XFS). Instant copy-on-write.
    Reflink,
    /// Fallback: sparse copy. Reads source but skips zero blocks.
    Sparse,
}

/// Creates a per-VM copy of the rootfs image using the best available strategy.
///
/// On reflink-capable filesystems (btrfs, XFS) this is an instant CoW clone.
/// Otherwise falls back to a sparse copy. The copy is chowned to the given
/// uid:gid so Firecracker can read/write it.
pub fn copy_rootfs(
    strategy: &CopyStrategy,
    source: &Path,
    dest: &Path,
    uid: usize,
    gid: usize,
) -> anyhow::Result<()> {
    let args: &[&str] = match strategy {
        CopyStrategy::Reflink => &["--reflink=always"],
        CopyStrategy::Sparse => &["--sparse=always"],
    };

    let output = Command::new("cp")
        .args(args)
        .arg(source)
        .arg(dest)
        .output()
        .context("Failed to execute cp")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to copy rootfs: {}", stderr.trim());
    }

    let output = Command::new("chown")
        .arg(format!("{}:{}", uid, gid))
        .arg(dest)
        .output()
        .context("Failed to execute chown")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to chown rootfs copy: {}", stderr.trim());
    }

    Ok(())
}

/// Removes a per-VM rootfs copy, logging any errors.
pub fn cleanup_rootfs_copy(vm_id: &Uuid, path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        error!(vm_id = %vm_id, path = %path.display(), error = ?e, "Failed to remove rootfs copy");
    }
}
