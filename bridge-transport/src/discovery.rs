//! Service discovery using Bonjour/mDNS
//!
//! Uses macOS native DNS-SD APIs for zero-configuration networking.

use bridge_common::{BridgeResult, BridgeError, ServerInfo, Resolution};
use std::ffi::{c_void, CStr, CString};
use std::net::{IpAddr, SocketAddr};
use std::collections::HashMap;
use std::sync::Arc;
use std::ptr;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::{SERVICE_TYPE, SERVICE_DOMAIN};

// DNS-SD FFI types
type DNSServiceRef = *mut c_void;
type DNSServiceFlags = u32;
type DNSServiceErrorType = i32;

const K_DNS_SERVICE_ERR_NO_ERROR: DNSServiceErrorType = 0;
const K_DNS_SERVICE_FLAGS_ADD: DNSServiceFlags = 0x2;

#[link(name = "System")]
extern "C" {
    fn DNSServiceRegister(
        sd_ref: *mut DNSServiceRef,
        flags: DNSServiceFlags,
        interface_index: u32,
        name: *const i8,
        reg_type: *const i8,
        domain: *const i8,
        host: *const i8,
        port: u16,
        txt_len: u16,
        txt_record: *const c_void,
        callback: Option<extern "C" fn(
            DNSServiceRef,
            DNSServiceFlags,
            DNSServiceErrorType,
            *const i8,
            *const i8,
            *const i8,
            *mut c_void,
        )>,
        context: *mut c_void,
    ) -> DNSServiceErrorType;

    fn DNSServiceBrowse(
        sd_ref: *mut DNSServiceRef,
        flags: DNSServiceFlags,
        interface_index: u32,
        reg_type: *const i8,
        domain: *const i8,
        callback: Option<extern "C" fn(
            DNSServiceRef,
            DNSServiceFlags,
            u32,
            DNSServiceErrorType,
            *const i8,
            *const i8,
            *const i8,
            *mut c_void,
        )>,
        context: *mut c_void,
    ) -> DNSServiceErrorType;

    fn DNSServiceResolve(
        sd_ref: *mut DNSServiceRef,
        flags: DNSServiceFlags,
        interface_index: u32,
        name: *const i8,
        reg_type: *const i8,
        domain: *const i8,
        callback: Option<extern "C" fn(
            DNSServiceRef,
            DNSServiceFlags,
            u32,
            DNSServiceErrorType,
            *const i8,
            *const i8,
            u16,
            u16,
            *const u8,
            *mut c_void,
        )>,
        context: *mut c_void,
    ) -> DNSServiceErrorType;

    fn DNSServiceGetAddrInfo(
        sd_ref: *mut DNSServiceRef,
        flags: DNSServiceFlags,
        interface_index: u32,
        protocol: u32,
        hostname: *const i8,
        callback: Option<extern "C" fn(
            DNSServiceRef,
            DNSServiceFlags,
            u32,
            DNSServiceErrorType,
            *const i8,
            *const libc::sockaddr,
            u32,
            *mut c_void,
        )>,
        context: *mut c_void,
    ) -> DNSServiceErrorType;

    fn DNSServiceRefDeallocate(sd_ref: DNSServiceRef);
    fn DNSServiceRefSockFD(sd_ref: DNSServiceRef) -> i32;
    fn DNSServiceProcessResult(sd_ref: DNSServiceRef) -> DNSServiceErrorType;
}

/// Service discovery events
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    /// A new server was discovered
    ServerFound(ServerInfo),
    /// A server was removed
    ServerLost(String),
}

/// Context for DNS-SD registration callback
struct AdvertiserContext {
    name: String,
    registered: bool,
}

/// Service advertiser for servers
pub struct ServiceAdvertiser {
    name: String,
    port: u16,
    service_ref: Option<DNSServiceRef>,
    _context: Option<Box<AdvertiserContext>>,
}

unsafe impl Send for ServiceAdvertiser {}

extern "C" fn register_callback(
    _sd_ref: DNSServiceRef,
    _flags: DNSServiceFlags,
    error: DNSServiceErrorType,
    name: *const i8,
    reg_type: *const i8,
    _domain: *const i8,
    context: *mut c_void,
) {
    if error != K_DNS_SERVICE_ERR_NO_ERROR {
        error!("DNS-SD registration failed with error: {}", error);
        return;
    }

    let ctx = unsafe { &mut *(context as *mut AdvertiserContext) };
    ctx.registered = true;

    let name_str = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    let type_str = unsafe { CStr::from_ptr(reg_type) }.to_string_lossy();
    info!("Service registered: {} ({})", name_str, type_str);
}

impl ServiceAdvertiser {
    /// Start advertising a Bridge server
    pub async fn new(name: &str, port: u16) -> BridgeResult<Self> {
        info!("Advertising Bridge service '{}' on port {}", name, port);

        let name_cstr = CString::new(name).map_err(|_| BridgeError::Config("Invalid service name".into()))?;
        let type_cstr = CString::new(SERVICE_TYPE).unwrap();
        let domain_cstr = CString::new(SERVICE_DOMAIN).unwrap();

        let mut context = Box::new(AdvertiserContext {
            name: name.to_string(),
            registered: false,
        });

        let context_ptr = &mut *context as *mut AdvertiserContext as *mut c_void;
        let mut service_ref: DNSServiceRef = ptr::null_mut();

        // Port must be in network byte order
        let port_be = port.to_be();

        let error = unsafe {
            DNSServiceRegister(
                &mut service_ref,
                0, // No flags
                0, // All interfaces
                name_cstr.as_ptr(),
                type_cstr.as_ptr(),
                domain_cstr.as_ptr(),
                ptr::null(), // Default host
                port_be,
                0, // No TXT record
                ptr::null(),
                Some(register_callback),
                context_ptr,
            )
        };

        if error != K_DNS_SERVICE_ERR_NO_ERROR {
            return Err(BridgeError::Transport(format!(
                "Failed to register DNS-SD service: error {}",
                error
            )));
        }

        // Process the registration in a background thread
        // We use a thread instead of async because DNS-SD uses its own socket that we
        // shouldn't interfere with using async I/O wrappers
        let service_ref_copy = service_ref as usize;
        std::thread::spawn(move || {
            loop {
                let result = unsafe {
                    DNSServiceProcessResult(service_ref_copy as DNSServiceRef)
                };
                if result != K_DNS_SERVICE_ERR_NO_ERROR {
                    break;
                }
            }
        });

        Ok(Self {
            name: name.to_string(),
            port,
            service_ref: Some(service_ref),
            _context: Some(context),
        })
    }

    /// Stop advertising
    pub fn stop(&mut self) {
        if let Some(service_ref) = self.service_ref.take() {
            info!("Stopping service advertisement for '{}'", self.name);
            unsafe {
                DNSServiceRefDeallocate(service_ref);
            }
        }
    }
}

impl Drop for ServiceAdvertiser {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Context for browser callbacks
struct BrowserContext {
    servers: Arc<RwLock<HashMap<String, ServerInfo>>>,
    event_tx: mpsc::Sender<DiscoveryEvent>,
    runtime_handle: tokio::runtime::Handle,
}

/// Context for resolve callback
struct ResolveContext {
    name: String,
    servers: Arc<RwLock<HashMap<String, ServerInfo>>>,
    event_tx: mpsc::Sender<DiscoveryEvent>,
    runtime_handle: tokio::runtime::Handle,
}

/// Service browser for clients
pub struct ServiceBrowser {
    servers: Arc<RwLock<HashMap<String, ServerInfo>>>,
    event_tx: mpsc::Sender<DiscoveryEvent>,
    event_rx: mpsc::Receiver<DiscoveryEvent>,
    service_ref: Option<DNSServiceRef>,
    _context: Option<Box<BrowserContext>>,
}

unsafe impl Send for ServiceBrowser {}

extern "C" fn browse_callback(
    _sd_ref: DNSServiceRef,
    flags: DNSServiceFlags,
    interface_index: u32,
    error: DNSServiceErrorType,
    name: *const i8,
    reg_type: *const i8,
    domain: *const i8,
    context: *mut c_void,
) {
    if error != K_DNS_SERVICE_ERR_NO_ERROR {
        error!("DNS-SD browse callback error: {}", error);
        return;
    }

    let ctx = unsafe { &*(context as *const BrowserContext) };
    let name_str = unsafe { CStr::from_ptr(name) }.to_string_lossy().to_string();
    let type_str = unsafe { CStr::from_ptr(reg_type) }.to_string_lossy().to_string();
    let domain_str = unsafe { CStr::from_ptr(domain) }.to_string_lossy().to_string();

    if flags & K_DNS_SERVICE_FLAGS_ADD != 0 {
        debug!("Found service: {} ({}) in {}", name_str, type_str, domain_str);

        // Resolve the service to get host and port
        let name_cstr = CString::new(name_str.clone()).unwrap();
        let type_cstr = CString::new(type_str).unwrap();
        let domain_cstr = CString::new(domain_str).unwrap();

        let resolve_ctx = Box::new(ResolveContext {
            name: name_str,
            servers: ctx.servers.clone(),
            event_tx: ctx.event_tx.clone(),
            runtime_handle: ctx.runtime_handle.clone(),
        });

        let resolve_ctx_ptr = Box::into_raw(resolve_ctx) as *mut c_void;
        let mut resolve_ref: DNSServiceRef = ptr::null_mut();

        let resolve_err = unsafe {
            DNSServiceResolve(
                &mut resolve_ref,
                0,
                interface_index,
                name_cstr.as_ptr(),
                type_cstr.as_ptr(),
                domain_cstr.as_ptr(),
                Some(resolve_callback),
                resolve_ctx_ptr,
            )
        };

        if resolve_err == K_DNS_SERVICE_ERR_NO_ERROR && !resolve_ref.is_null() {
            // Process resolve result synchronously
            unsafe {
                DNSServiceProcessResult(resolve_ref);
                DNSServiceRefDeallocate(resolve_ref);
            }
        } else {
            // Clean up context on error
            unsafe { drop(Box::from_raw(resolve_ctx_ptr as *mut ResolveContext)); }
        }
    } else {
        // Service removed
        debug!("Service removed: {}", name_str);
        let servers = ctx.servers.clone();
        let event_tx = ctx.event_tx.clone();
        let handle = ctx.runtime_handle.clone();

        handle.spawn(async move {
            servers.write().await.remove(&name_str);
            let _ = event_tx.send(DiscoveryEvent::ServerLost(name_str)).await;
        });
    }
}

extern "C" fn resolve_callback(
    _sd_ref: DNSServiceRef,
    _flags: DNSServiceFlags,
    _interface_index: u32,
    error: DNSServiceErrorType,
    _fullname: *const i8,
    host: *const i8,
    port: u16,
    _txt_len: u16,
    _txt_record: *const u8,
    context: *mut c_void,
) {
    let ctx = unsafe { Box::from_raw(context as *mut ResolveContext) };

    if error != K_DNS_SERVICE_ERR_NO_ERROR {
        error!("DNS-SD resolve callback error: {}", error);
        return;
    }

    let host_str = unsafe { CStr::from_ptr(host) }.to_string_lossy().to_string();
    let port_host = u16::from_be(port);

    info!("Resolved service '{}' to {}:{}", ctx.name, host_str, port_host);

    // Try to resolve the hostname to an IP address
    let servers = ctx.servers.clone();
    let event_tx = ctx.event_tx.clone();
    let name = ctx.name.clone();
    let handle = ctx.runtime_handle.clone();

    handle.spawn(async move {
        match resolve_hostname(&host_str).await {
            Ok(ip) => {
                let addr = SocketAddr::new(ip, port_host);
                let info = ServerInfo {
                    name: name.clone(),
                    address: addr,
                    hostname: host_str,
                    resolution: Resolution::UHD_4K,
                    available: true,
                };

                servers.write().await.insert(name, info.clone());
                let _ = event_tx.send(DiscoveryEvent::ServerFound(info)).await;
            }
            Err(e) => {
                warn!("Failed to resolve hostname {}: {}", host_str, e);
            }
        }
    });
}

impl ServiceBrowser {
    /// Start browsing for Bridge servers
    pub async fn new() -> BridgeResult<Self> {
        let (event_tx, event_rx) = mpsc::channel(32);
        let servers = Arc::new(RwLock::new(HashMap::new()));

        info!("Starting Bridge service browser for {}", SERVICE_TYPE);

        let context = Box::new(BrowserContext {
            servers: servers.clone(),
            event_tx: event_tx.clone(),
            runtime_handle: tokio::runtime::Handle::current(),
        });

        let context_ptr = Box::into_raw(context);
        let type_cstr = CString::new(SERVICE_TYPE).unwrap();

        let mut service_ref: DNSServiceRef = ptr::null_mut();

        let error = unsafe {
            DNSServiceBrowse(
                &mut service_ref,
                0, // No flags
                0, // All interfaces
                type_cstr.as_ptr(),
                ptr::null(), // Default domain
                Some(browse_callback),
                context_ptr as *mut c_void,
            )
        };

        if error != K_DNS_SERVICE_ERR_NO_ERROR {
            unsafe { drop(Box::from_raw(context_ptr)); }
            return Err(BridgeError::Transport(format!(
                "Failed to start DNS-SD browse: error {}",
                error
            )));
        }

        // Process DNS-SD events in a background thread
        let service_ref_copy = service_ref as usize;
        std::thread::spawn(move || {
            loop {
                let result = unsafe {
                    DNSServiceProcessResult(service_ref_copy as DNSServiceRef)
                };
                if result != K_DNS_SERVICE_ERR_NO_ERROR {
                    break;
                }
            }
        });

        Ok(Self {
            servers,
            event_tx,
            event_rx,
            service_ref: Some(service_ref),
            _context: Some(unsafe { Box::from_raw(context_ptr) }),
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
        if let Some(service_ref) = self.service_ref.take() {
            info!("Stopping service browser");
            unsafe {
                DNSServiceRefDeallocate(service_ref);
            }
        }
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
