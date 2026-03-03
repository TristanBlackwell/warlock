pub mod network;
pub mod orphan;
mod preflight;
mod version;

use std::path::PathBuf;

pub use preflight::preflight_check;

// Re-export the domain CopyStrategy so callers that go through the firecracker
// module (e.g. JailerConfig) don't need to know about the vm layer.
pub use crate::vm::rootfs::CopyStrategy;

/// UID/GID for the jailed Firecracker process.
pub const JAILER_UID: usize = 1100;
pub const JAILER_GID: usize = 1100;

/// Directory for per-VM rootfs copies.
pub const VM_IMAGES_DIR: &str = "/srv/jailer/vm-images";

/// Configuration determined at startup for the jailer.
#[derive(Debug, Clone)]
pub struct JailerConfig {
    /// Detected cgroup version (1 or 2).
    pub cgroup_version: usize,
    /// Absolute path to the Firecracker binary.
    pub firecracker_path: PathBuf,
    /// Absolute path to the jailer binary.
    pub jailer_path: PathBuf,
    /// Absolute path to the kernel image.
    pub kernel_path: PathBuf,
    /// Absolute path to the base rootfs image.
    pub rootfs_path: PathBuf,
    /// Directory for per-VM rootfs copies.
    pub vm_images_dir: PathBuf,
    /// Detected strategy for copying rootfs images per VM.
    pub copy_strategy: CopyStrategy,
    /// Host's outward-facing network interface (e.g. `eth0`).
    pub host_interface: String,
}
