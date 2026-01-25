//! Service discovery using Bonjour/mDNS

use bridge_common::{BridgeResult, BridgeError, ServerInfo, Resolution};
use std::net::{IpAddr, SocketAddr};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use crate::{SERVICE_TYPE, SERVICE_DOMAIN, DEFAULT_CONTROL_PORT};

/// Service discovery events
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    /// A new server was discovered
    ServerFound(ServerInfo),
    /// A server was removed
    ServerLost(String),
}

/// Service advertiser for servers
pub struct ServiceAdvertiser {
    name: String,
    port: u16,
    // In a full implementation, this would hold the Bonjour registration
}

impl ServiceAdvertiser {
    /// Start advertising a Bridge server
    pub async fn new(name: &str, port: u16) -> BridgeResult<Self> {
        info!("Advertising Bridge service '{}' on port {}", name, port);

        // TODO: Implement actual Bonjour registration using dns-sd or similar
        // For now, this is a placeholder
        //
        // In a full implementation:
        // 1. Use the dns-sd crate or direct FFI to DNSServiceRegister
        // 2. Register the service with type "_bridge._tcp" on the local domain
        // 3. Include TXT records with resolution and other metadata

        Ok(Self {
            name: name.to_string(),
            port,
        })
    }

    /// Stop advertising
    pub fn stop(&mut self) {
        info!("Stopping service advertisement for '{}'", self.name);
        // Deregister from Bonjour
    }
}

impl Drop for ServiceAdvertiser {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Service browser for clients
pub struct ServiceBrowser {
    servers: Arc<RwLock<HashMap<String, ServerInfo>>>,
    event_tx: mpsc::Sender<DiscoveryEvent>,
    event_rx: mpsc::Receiver<DiscoveryEvent>,
}

impl ServiceBrowser {
    /// Start browsing for Bridge servers
    pub async fn new() -> BridgeResult<Self> {
        let (event_tx, event_rx) = mpsc::channel(32);
        let servers = Arc::new(RwLock::new(HashMap::new()));

        info!("Starting Bridge service browser");

        // TODO: Implement actual Bonjour browsing using dns-sd or similar
        // For now, this is a placeholder
        //
        // In a full implementation:
        // 1. Use DNSServiceBrowse to discover _bridge._tcp services
        // 2. Use DNSServiceResolve to get the host and port
        // 3. Use DNSServiceGetAddrInfo to get the IP address
        // 4. Send DiscoveryEvent::ServerFound when a service is found
        // 5. Send DiscoveryEvent::ServerLost when a service is removed

        Ok(Self {
            servers,
            event_tx,
            event_rx,
        })
    }

    /// Get the next discovery event
    pub async fn next_event(&mut self) -> Option<DiscoveryEvent> {
        self.event_rx.recv().await
    }

    /// Get all currently known servers
    pub async fn servers(&self) -> Vec<ServerInfo> {
        self.servers.read().await.values().cloned().collect()
    }

    /// Manually add a server (for direct IP connections)
    pub async fn add_server(&self, address: SocketAddr, name: &str) {
        let info = ServerInfo {
            name: name.to_string(),
            address,
            hostname: address.ip().to_string(),
            resolution: Resolution::UHD_4K,
            available: true,
        };

        self.servers.write().await.insert(name.to_string(), info.clone());
        let _ = self.event_tx.send(DiscoveryEvent::ServerFound(info)).await;
    }

    /// Stop browsing
    pub fn stop(&mut self) {
        info!("Stopping service browser");
        // Stop the Bonjour browse operation
    }
}

impl Drop for ServiceBrowser {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Resolve a hostname to an IP address
pub async fn resolve_hostname(hostname: &str) -> BridgeResult<IpAddr> {
    use tokio::net::lookup_host;

    let addr = format!("{}:0", hostname);
    let mut addrs = lookup_host(&addr).await.map_err(|e| {
        BridgeError::Transport(format!("Failed to resolve hostname '{}': {}", hostname, e))
    })?;

    addrs
        .next()
        .map(|a| a.ip())
        .ok_or_else(|| BridgeError::Transport(format!("No addresses found for '{}'", hostname)))
}

/// Get all local IP addresses
pub fn get_local_addresses() -> Vec<IpAddr> {
    let mut addresses = Vec::new();

    // This is a simple implementation using getifaddrs would be more complete
    // For now, try to get addresses by connecting to a known address
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        // Try connecting to Google's DNS to find our default route address
        if socket.connect("8.8.8.8:53").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                addresses.push(addr.ip());
            }
        }
    }

    // Add localhost
    addresses.push(IpAddr::from([127, 0, 0, 1]));

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
        assert!(addrs.contains(&IpAddr::from([127, 0, 0, 1])));
    }

    #[test]
    fn test_is_thunderbolt() {
        assert!(is_thunderbolt_interface(&IpAddr::from([169, 254, 1, 1])));
        assert!(!is_thunderbolt_interface(&IpAddr::from([192, 168, 1, 1])));
    }
}
