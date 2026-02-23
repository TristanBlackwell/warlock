use sysinfo::System;

/// Represents a virtual machine instance with its resource allocation and runtime information.
pub struct Vm {
    pub id: String,
    pub ram_mb: u64,
    pub capacity: Capacity,
    pub socket_path: String,
    pub process: std::process::Child,
}

#[derive(Debug, Clone, Copy)]
pub struct Capacity {
    /// Total system memory in megabytes.
    pub memory_mb: u64,
    /// Total number of CPU cores.
    pub vcpus: u8,
}

/// Returns the total system memory in megabytes.
///
/// This function works cross-platform (Linux, macOS, Windows, FreeBSD).
pub fn total_memory_mb() -> anyhow::Result<u64> {
    let mut sys = System::new_all();
    sys.refresh_memory();

    let total_memory_bytes = sys.total_memory();
    let total_memory_mb = total_memory_bytes / 1024 / 1024;

    Ok(total_memory_mb)
}

/// Returns the number of available CPU cores.
///
/// This function works cross-platform and returns the number of logical CPU cores
/// available to the current process. If the number cannot be determined, it defaults to 1.
pub fn total_cpus() -> u8 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u8)
        .unwrap_or(1)
}

/// Returns the total available system capacity.
///
/// # Errors
///
/// Returns an error if the memory information cannot be retrieved.
pub fn available_capacity() -> anyhow::Result<Capacity> {
    let total_memory = total_memory_mb()?;
    let total_cpus = total_cpus();

    Ok(Capacity {
        memory_mb: total_memory,
        vcpus: total_cpus,
    })
}
