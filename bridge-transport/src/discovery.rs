//! Service discovery — platform-neutral utilities and platform dispatch

use bridge_common::{BridgeResult, BridgeError};
use std::net::IpAddr;
use std::ptr;
use tracing::{debug, info};

// Platform-specific discovery implementations
#[cfg(target_os = "macos")]
pub use crate::macos::discovery::{ServiceAdvertiser, ServiceBrowser};
#[cfg(target_os = "linux")]
pub use crate::linux::discovery::{ServiceAdvertiser, ServiceBrowser};

/// Service discovery events
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    /// A new server was discovered
    ServerFound(bridge_common::ServerInfo),
    /// A server was removed
    ServerLost(String),
}

/// Resolve a hostname to an IP address
pub async fn resolve_hostname(hostname: &str) -> BridgeResult<IpAddr> {
    use tokio::net::lookup_host;

    let addr = format!("{}:0", hostname);
    let addrs: Vec<_> = lookup_host(&addr).await.map_err(|e| {
        BridgeError::Transport(format!("Failed to resolve hostname '{}': {}", hostname, e))
    })?.collect();

    if addrs.is_empty() {
        return Err(BridgeError::Transport(format!("No addresses found for '{}'", hostname)));
    }

    // Prefer addresses in this order:
    // 1. Thunderbolt/link-local IPv4 (169.254.x.x) — lowest latency
    // 2. Regular IPv4 addresses
    // 3. Global IPv6 addresses
    // 4. Any remaining address as fallback

    // First: prefer Thunderbolt link-local addresses
    for addr in &addrs {
        if let IpAddr::V4(v4) = addr.ip() {
            if is_thunderbolt_interface(&IpAddr::V4(v4)) {
                info!("Resolved {} to Thunderbolt address {}", hostname, v4);
                return Ok(IpAddr::V4(v4));
            }
        }
    }

    // Then: regular IPv4 addresses
    for addr in &addrs {
        if let IpAddr::V4(v4) = addr.ip() {
            if !v4.is_loopback() && !v4.is_unspecified() && !v4.is_link_local() {
                debug!("Resolved {} to IPv4 {}", hostname, v4);
                return Ok(IpAddr::V4(v4));
            }
        }
    }

    // Then: global IPv6 addresses
    for addr in &addrs {
        if let IpAddr::V6(v6) = addr.ip() {
            let segments = v6.segments();
            let is_link_local = segments[0] == 0xfe80;
            let is_loopback = v6.is_loopback();

            if !is_link_local && !is_loopback {
                debug!("Resolved {} to IPv6 {}", hostname, v6);
                return Ok(IpAddr::V6(v6));
            }
        }
    }

    // Fall back to first address if nothing better found
    let fallback = addrs[0].ip();
    debug!("Resolved {} to fallback address {}", hostname, fallback);
    Ok(fallback)
}

/// Get all local IP addresses using getifaddrs
pub fn get_local_addresses() -> Vec<IpAddr> {
    let mut addresses = Vec::new();

    unsafe {
        let mut ifaddrs: *mut libc::ifaddrs = ptr::null_mut();
        if libc::getifaddrs(&mut ifaddrs) != 0 {
            // Fallback: try the UDP trick
            if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
                if socket.connect("8.8.8.8:53").is_ok() {
                    if let Ok(addr) = socket.local_addr() {
                        addresses.push(addr.ip());
                    }
                }
            }
            addresses.push(IpAddr::from([127, 0, 0, 1]));
            return addresses;
        }

        let mut current = ifaddrs;
        while !current.is_null() {
            let ifa = &*current;

            // Check interface is UP and RUNNING
            let flags = ifa.ifa_flags as i32;
            let is_up = flags & libc::IFF_UP != 0;
            let is_running = flags & libc::IFF_RUNNING != 0;
            let is_loopback = flags & libc::IFF_LOOPBACK != 0;

            if is_up && is_running && !is_loopback && !ifa.ifa_addr.is_null() {
                let sa_family = (*ifa.ifa_addr).sa_family as i32;

                if sa_family == libc::AF_INET {
                    let sockaddr_in = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    let ip = std::net::Ipv4Addr::from(u32::from_be(sockaddr_in.sin_addr.s_addr));
                    if !ip.is_loopback() && !ip.is_unspecified() {
                        let addr = IpAddr::V4(ip);
                        if !addresses.contains(&addr) {
                            addresses.push(addr);
                        }
                    }
                }
            }

            current = ifa.ifa_next;
        }

        libc::freeifaddrs(ifaddrs);
    }

    // Add localhost as fallback
    if addresses.is_empty() {
        addresses.push(IpAddr::from([127, 0, 0, 1]));
    }

    addresses
}

/// Check if an address is likely a Thunderbolt interface
pub fn is_thunderbolt_interface(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(ipv4) => {
            let octets = ipv4.octets();
            // Link-local addresses often used for Thunderbolt
            octets[0] == 169 && octets[1] == 254
        }
        IpAddr::V6(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_local_addresses() {
        let addrs = get_local_addresses();
        assert!(!addrs.is_empty());
        // Should contain at least one non-loopback address (or localhost as fallback)
        for addr in &addrs {
            assert!(!addr.is_unspecified());
        }
    }

    #[test]
    fn test_is_thunderbolt() {
        assert!(is_thunderbolt_interface(&IpAddr::from([169, 254, 1, 1])));
        assert!(!is_thunderbolt_interface(&IpAddr::from([192, 168, 1, 1])));
    }
}
