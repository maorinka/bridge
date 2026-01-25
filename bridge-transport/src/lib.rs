//! Bridge Transport Layer
//!
//! Provides unified networking abstraction supporting:
//! - Thunderbolt IP Bridge (lowest latency)
//! - UDP for video/audio data
//! - QUIC/TCP for control channel

pub mod connection;
pub mod udp;
pub mod quic;
pub mod discovery;

pub use connection::*;
pub use udp::*;
pub use quic::*;
pub use discovery::*;

use bridge_common::{BridgeError, BridgeResult};
use std::net::SocketAddr;

/// Default ports for Bridge services
pub const DEFAULT_CONTROL_PORT: u16 = 9420;
pub const DEFAULT_VIDEO_PORT: u16 = 9421;
pub const DEFAULT_AUDIO_PORT: u16 = 9422;
pub const DEFAULT_INPUT_PORT: u16 = 9423;

/// Service name for Bonjour discovery
pub const SERVICE_TYPE: &str = "_bridge._tcp";
pub const SERVICE_DOMAIN: &str = "local.";

/// Transport configuration
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Control channel port (QUIC)
    pub control_port: u16,
    /// Video stream port (UDP)
    pub video_port: u16,
    /// Audio stream port (UDP)
    pub audio_port: u16,
    /// Input events port (UDP)
    pub input_port: u16,
    /// Maximum packet size for UDP
    pub max_packet_size: usize,
    /// Send buffer size
    pub send_buffer_size: usize,
    /// Receive buffer size
    pub recv_buffer_size: usize,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            control_port: DEFAULT_CONTROL_PORT,
            video_port: DEFAULT_VIDEO_PORT,
            audio_port: DEFAULT_AUDIO_PORT,
            input_port: DEFAULT_INPUT_PORT,
            max_packet_size: bridge_common::MAX_UDP_PACKET_SIZE,
            send_buffer_size: 16 * 1024 * 1024, // 16MB
            recv_buffer_size: 16 * 1024 * 1024, // 16MB
        }
    }
}

/// Get the local Thunderbolt IP address if available
pub fn get_thunderbolt_address() -> Option<SocketAddr> {
    // Thunderbolt networking creates a bridge interface
    // On macOS, this is typically bridge0 or en5/en6
    // For now, return None - we'll implement proper detection later
    None
}

/// Check if running over Thunderbolt connection
pub fn is_thunderbolt_connection(addr: &SocketAddr) -> bool {
    // Thunderbolt IP typically uses 169.254.x.x link-local addresses
    // or custom configured addresses
    if let std::net::IpAddr::V4(ipv4) = addr.ip() {
        let octets = ipv4.octets();
        // Link-local range often used for Thunderbolt
        octets[0] == 169 && octets[1] == 254
    } else {
        false
    }
}
