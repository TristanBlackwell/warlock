const DEFAULT_VCPUS: u8 = 1;
const DEFAULT_MEMORY_MB: u32 = 128;
const MIN_MEMORY_MB: u32 = 128;
const MAX_VCPUS: u8 = 32;

/// Errors from validating a VM configuration.
#[derive(Debug)]
pub enum ConfigError {
    InvalidVcpus(String),
    InvalidMemory(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidVcpus(msg) | Self::InvalidMemory(msg) => f.write_str(msg),
        }
    }
}

/// Validates the requested VM configuration and returns the resolved (vcpus, memory_mb).
///
/// Applies defaults for any `None` values: 1 vCPU and 128 MB memory.
pub fn validate_vm_config(
    vcpus: Option<u8>,
    memory_mb: Option<u32>,
) -> Result<(u8, u32), ConfigError> {
    let vcpus = vcpus.unwrap_or(DEFAULT_VCPUS);
    let memory_mb = memory_mb.unwrap_or(DEFAULT_MEMORY_MB);

    if vcpus == 0 || vcpus > MAX_VCPUS || (vcpus > 1 && vcpus % 2 != 0) {
        return Err(ConfigError::InvalidVcpus(
            "vcpus must be 1 or an even number between 2 and 32".into(),
        ));
    }

    if memory_mb < MIN_MEMORY_MB {
        return Err(ConfigError::InvalidMemory(format!(
            "memory_mb must be at least {}",
            MIN_MEMORY_MB,
        )));
    }

    Ok((vcpus, memory_mb))
}

/// Builds the cgroup configuration for a jailed VM based on the detected
/// cgroup version and requested resources.
pub fn build_cgroup_config(
    cgroup_version: usize,
    vcpus: u8,
    memory_mb: u32,
) -> Vec<(String, String)> {
    // Memory limit: VM allocation + 50 MB overhead for the Firecracker process
    let memory_limit_bytes = ((memory_mb as u64) + 50) * 1024 * 1024;
    // CPU quota: 100% of one physical core per vCPU (100_000 us per 100_000 us period)
    let cpu_quota = (vcpus as u64) * 100_000;

    match cgroup_version {
        2 => vec![
            ("cpu.max".into(), format!("{} 100000", cpu_quota)),
            ("memory.max".into(), memory_limit_bytes.to_string()),
        ],
        _ => vec![
            ("cpu.cfs_quota_us".into(), cpu_quota.to_string()),
            ("cpu.cfs_period_us".into(), "100000".into()),
            (
                "memory.limit_in_bytes".into(),
                memory_limit_bytes.to_string(),
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Defaults ──

    #[test]
    fn defaults_when_no_values() {
        let (vcpus, memory_mb) = validate_vm_config(None, None).unwrap();
        assert_eq!(vcpus, 1);
        assert_eq!(memory_mb, 128);
    }

    // ── Valid vCPU values ──

    #[test]
    fn accepts_1_vcpu() {
        let (vcpus, _) = validate_vm_config(Some(1), None).unwrap();
        assert_eq!(vcpus, 1);
    }

    #[test]
    fn accepts_even_vcpus() {
        for n in [2, 4, 8, 16, 32] {
            let (vcpus, _) = validate_vm_config(Some(n), None).unwrap();
            assert_eq!(vcpus, n);
        }
    }

    // ── Invalid vCPU values ──

    #[test]
    fn rejects_0_vcpus() {
        let err = validate_vm_config(Some(0), None).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidVcpus(_)));
    }

    #[test]
    fn rejects_odd_vcpus_greater_than_1() {
        for n in [3, 5, 7, 15, 31] {
            let err = validate_vm_config(Some(n), None).unwrap_err();
            assert!(matches!(err, ConfigError::InvalidVcpus(_)));
        }
    }

    #[test]
    fn rejects_vcpus_over_32() {
        let err = validate_vm_config(Some(34), None).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidVcpus(_)));
    }

    // ── Valid memory values ──

    #[test]
    fn accepts_minimum_memory() {
        let (_, memory_mb) = validate_vm_config(None, Some(128)).unwrap();
        assert_eq!(memory_mb, 128);
    }

    #[test]
    fn accepts_large_memory() {
        let (_, memory_mb) = validate_vm_config(None, Some(4096)).unwrap();
        assert_eq!(memory_mb, 4096);
    }

    // ── Invalid memory values ──

    #[test]
    fn rejects_memory_below_minimum() {
        let err = validate_vm_config(None, Some(64)).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidMemory(_)));
    }

    // ── Cgroup configuration ──

    #[test]
    fn cgroup_v2_config() {
        let cgroups = build_cgroup_config(2, 1, 128);
        assert_eq!(cgroups.len(), 2);
        // 1 vCPU = 100_000 us quota per 100_000 us period
        assert_eq!(cgroups[0], ("cpu.max".into(), "100000 100000".into()));
        // 128 MB + 50 MB overhead = 178 MB in bytes
        let expected_mem = ((128u64 + 50) * 1024 * 1024).to_string();
        assert_eq!(cgroups[1], ("memory.max".into(), expected_mem));
    }

    #[test]
    fn cgroup_v2_config_multi_vcpu() {
        let cgroups = build_cgroup_config(2, 4, 256);
        // 4 vCPUs = 400_000 us quota
        assert_eq!(cgroups[0], ("cpu.max".into(), "400000 100000".into()));
        let expected_mem = ((256u64 + 50) * 1024 * 1024).to_string();
        assert_eq!(cgroups[1], ("memory.max".into(), expected_mem));
    }

    #[test]
    fn cgroup_v1_config() {
        let cgroups = build_cgroup_config(1, 2, 256);
        assert_eq!(cgroups.len(), 3);
        assert_eq!(cgroups[0], ("cpu.cfs_quota_us".into(), "200000".into()));
        assert_eq!(cgroups[1], ("cpu.cfs_period_us".into(), "100000".into()));
        let expected_mem = ((256u64 + 50) * 1024 * 1024).to_string();
        assert_eq!(cgroups[2], ("memory.limit_in_bytes".into(), expected_mem));
    }

    #[test]
    fn cgroup_v1_single_vcpu() {
        let cgroups = build_cgroup_config(1, 1, 128);
        assert_eq!(cgroups.len(), 3);
        // 1 vCPU = 100_000 us quota
        assert_eq!(cgroups[0], ("cpu.cfs_quota_us".into(), "100000".into()));
        assert_eq!(cgroups[1], ("cpu.cfs_period_us".into(), "100000".into()));
        let expected_mem = ((128u64 + 50) * 1024 * 1024).to_string();
        assert_eq!(cgroups[2], ("memory.limit_in_bytes".into(), expected_mem));
    }

    #[test]
    fn cgroup_v2_max_vcpus() {
        let cgroups = build_cgroup_config(2, 32, 4096);
        // 32 vCPUs = 3_200_000 us quota
        assert_eq!(cgroups[0], ("cpu.max".into(), "3200000 100000".into()));
        let expected_mem = ((4096u64 + 50) * 1024 * 1024).to_string();
        assert_eq!(cgroups[1], ("memory.max".into(), expected_mem));
    }
}
