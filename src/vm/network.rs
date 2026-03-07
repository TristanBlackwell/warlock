use std::net::Ipv4Addr;

/// Maximum number of `/30` subnets in the `172.16.0.0/16` range.
///
/// Each subnet consumes 4 IPs (network, tap, guest, broadcast), giving
/// 65536 / 4 = 16384 available subnets.
const MAX_SUBNETS: usize = 16_384;

/// A pool of `/30` subnets in `172.16.0.0/16` for per-VM networking.
///
/// Each VM gets a dedicated `/30` subnet containing a tap IP (host side)
/// and a guest IP (VM side). Subnets are tracked by index and recycled
/// when VMs are deleted.
pub struct SubnetPool {
    /// Bitmap: `true` = in use, `false` = free.
    slots: Vec<bool>,
}

/// A subnet allocation for a single VM.
#[derive(Debug, Clone)]
pub struct SubnetAllocation {
    /// Index into the pool (0-based).
    pub index: u16,
    /// IP address for the host-side tap device.
    pub tap_ip: Ipv4Addr,
    /// IP address for the guest.
    pub guest_ip: Ipv4Addr,
    /// Name of the tap device (e.g. `fc0`, `fc42`).
    pub tap_name: String,
}

impl Default for SubnetPool {
    fn default() -> Self {
        SubnetPool::new()
    }
}

impl SubnetPool {
    pub fn new() -> Self {
        Self {
            slots: vec![false; MAX_SUBNETS],
        }
    }

    /// Allocates the next free `/30` subnet.
    ///
    /// Returns `None` if all 16384 subnets are in use.
    pub fn allocate(&mut self) -> Option<SubnetAllocation> {
        let index = self.slots.iter().position(|&used| !used)?;
        self.slots[index] = true;

        let index = index as u16;
        Some(SubnetAllocation {
            tap_ip: subnet_tap_ip(index),
            guest_ip: subnet_guest_ip(index),
            tap_name: tap_name(index),
            index,
        })
    }

    /// Releases a previously allocated subnet back to the pool.
    ///
    /// # Panics
    ///
    /// Panics if the index is out of range.
    pub fn release(&mut self, index: u16) {
        self.slots[index as usize] = false;
    }
}

/// Computes the tap (host-side) IP for subnet `index`.
///
/// Formula from Firecracker docs: `172.16.[(4*N+1)/256].[(4*N+1)%256]`
fn subnet_tap_ip(index: u16) -> Ipv4Addr {
    let offset = 4 * (index as u32) + 1;
    Ipv4Addr::new(172, 16, (offset / 256) as u8, (offset % 256) as u8)
}

/// Computes the guest IP for subnet `index`.
///
/// Formula from Firecracker docs: `172.16.[(4*N+2)/256].[(4*N+2)%256]`
fn subnet_guest_ip(index: u16) -> Ipv4Addr {
    let offset = 4 * (index as u32) + 2;
    Ipv4Addr::new(172, 16, (offset / 256) as u8, (offset % 256) as u8)
}

/// Returns the tap device name for a subnet index.
///
/// Linux interface names are limited to 15 characters. Using `fc{N}`
/// keeps names short and unique (e.g. `fc0`, `fc16383`).
fn tap_name(index: u16) -> String {
    format!("fc{}", index)
}

/// Builds the kernel `ip=` boot argument for automatic guest network
/// configuration.
///
/// Format: `ip=<guest_ip>::<gateway>:<netmask>::<iface>:off`
///
/// This configures the guest's network at kernel level, avoiding any
/// dependency on `iproute2` or DHCP in the guest.
pub fn build_network_boot_args(guest_ip: &Ipv4Addr, tap_ip: &Ipv4Addr) -> String {
    format!("ip={}::{}:255.255.255.252::eth0:off", guest_ip, tap_ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── IP calculation ──

    #[test]
    fn first_subnet_ips() {
        assert_eq!(subnet_tap_ip(0), Ipv4Addr::new(172, 16, 0, 1));
        assert_eq!(subnet_guest_ip(0), Ipv4Addr::new(172, 16, 0, 2));
    }

    #[test]
    fn second_subnet_ips() {
        assert_eq!(subnet_tap_ip(1), Ipv4Addr::new(172, 16, 0, 5));
        assert_eq!(subnet_guest_ip(1), Ipv4Addr::new(172, 16, 0, 6));
    }

    #[test]
    fn subnet_wraps_third_octet() {
        // Index 64: offset = 4*64 = 256, tap = 256+1 = 257
        assert_eq!(subnet_tap_ip(64), Ipv4Addr::new(172, 16, 1, 1));
        assert_eq!(subnet_guest_ip(64), Ipv4Addr::new(172, 16, 1, 2));
    }

    #[test]
    fn thousandth_subnet_matches_firecracker_docs() {
        // From Firecracker docs: VM 999
        // tap = 172.16.[(4*999+1)/256].[(4*999+1)%256] = 172.16.15.157
        // guest = 172.16.[(4*999+2)/256].[(4*999+2)%256] = 172.16.15.158
        assert_eq!(subnet_tap_ip(999), Ipv4Addr::new(172, 16, 15, 157));
        assert_eq!(subnet_guest_ip(999), Ipv4Addr::new(172, 16, 15, 158));
    }

    #[test]
    fn last_subnet_ips() {
        // Index 16383: offset = 4*16383 = 65532
        // tap = 65533 -> 172.16.255.253
        // guest = 65534 -> 172.16.255.254
        assert_eq!(subnet_tap_ip(16383), Ipv4Addr::new(172, 16, 255, 253));
        assert_eq!(subnet_guest_ip(16383), Ipv4Addr::new(172, 16, 255, 254));
    }

    // ── Tap naming ──

    #[test]
    fn tap_name_format() {
        assert_eq!(tap_name(0), "fc0");
        assert_eq!(tap_name(42), "fc42");
        assert_eq!(tap_name(16383), "fc16383");
    }

    #[test]
    fn tap_name_within_linux_limit() {
        // Linux interface names max out at 15 characters
        let longest = tap_name(16383); // "fc16383" = 7 chars
        assert!(longest.len() <= 15);
    }

    // ── Boot args ──

    #[test]
    fn boot_args_format() {
        let args =
            build_network_boot_args(&Ipv4Addr::new(172, 16, 0, 2), &Ipv4Addr::new(172, 16, 0, 1));
        assert_eq!(args, "ip=172.16.0.2::172.16.0.1:255.255.255.252::eth0:off");
    }

    // ── Pool allocation ──

    #[test]
    fn allocate_first_subnet() {
        let mut pool = SubnetPool::new();
        let alloc = pool.allocate().unwrap();

        assert_eq!(alloc.index, 0);
        assert_eq!(alloc.tap_ip, Ipv4Addr::new(172, 16, 0, 1));
        assert_eq!(alloc.guest_ip, Ipv4Addr::new(172, 16, 0, 2));
        assert_eq!(alloc.tap_name, "fc0");
    }

    #[test]
    fn sequential_allocation() {
        let mut pool = SubnetPool::new();

        let a = pool.allocate().unwrap();
        let b = pool.allocate().unwrap();
        let c = pool.allocate().unwrap();

        assert_eq!(a.index, 0);
        assert_eq!(b.index, 1);
        assert_eq!(c.index, 2);
    }

    #[test]
    fn release_and_reuse() {
        let mut pool = SubnetPool::new();

        let a = pool.allocate().unwrap();
        let b = pool.allocate().unwrap();
        assert_eq!(a.index, 0);
        assert_eq!(b.index, 1);

        // Release the first, next allocation should reuse index 0
        pool.release(0);
        let c = pool.allocate().unwrap();
        assert_eq!(c.index, 0);
    }

    #[test]
    fn pool_exhaustion() {
        let mut pool = SubnetPool::new();

        // Allocate all subnets
        for _ in 0..MAX_SUBNETS {
            assert!(pool.allocate().is_some());
        }

        // Pool is exhausted
        assert!(pool.allocate().is_none());

        // Release one and it's available again
        pool.release(100);
        let alloc = pool.allocate().unwrap();
        assert_eq!(alloc.index, 100);
    }
}
