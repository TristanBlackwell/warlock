use sysinfo::System;

/// Memory in MB reserved for the host OS and Warlock itself.
const HOST_RESERVED_MEMORY_MB: u64 = 256;

#[derive(Debug, Clone, Copy)]
pub struct Capacity {
    /// Total system memory in megabytes.
    pub memory_mb: u64,
    /// Total number of CPU cores.
    pub vcpus: u8,
}

impl Capacity {
    /// Memory available for VM allocation after the host reservation.
    pub fn allocatable_memory_mb(&self) -> u64 {
        self.memory_mb.saturating_sub(HOST_RESERVED_MEMORY_MB)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocatable_memory_subtracts_reservation() {
        let cap = Capacity {
            memory_mb: 1024,
            vcpus: 1,
        };
        assert_eq!(cap.allocatable_memory_mb(), 1024 - 256);
    }

    #[test]
    fn allocatable_memory_saturates_at_zero() {
        let cap = Capacity {
            memory_mb: 128,
            vcpus: 1,
        };
        // 128 MB < 256 MB reservation, should saturate to 0
        assert_eq!(cap.allocatable_memory_mb(), 0);
    }

    #[test]
    fn allocatable_memory_exact_reservation() {
        let cap = Capacity {
            memory_mb: 256,
            vcpus: 1,
        };
        assert_eq!(cap.allocatable_memory_mb(), 0);
    }

    #[test]
    fn allocatable_memory_just_above_reservation() {
        let cap = Capacity {
            memory_mb: 257,
            vcpus: 1,
        };
        assert_eq!(cap.allocatable_memory_mb(), 1);
    }
}
