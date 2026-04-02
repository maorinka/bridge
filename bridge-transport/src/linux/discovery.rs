//! Service discovery for Linux (direct IP mode)
//!
//! Full Avahi/mDNS integration requires libavahi-client.
//! For now, provides stub ServiceAdvertiser/ServiceBrowser.
//! Clients connect via --address flag.

use bridge_common::{BridgeResult, ServerInfo};
use tracing::info;

pub struct ServiceAdvertiser {
    _name: String,
    _port: u16,
}

impl ServiceAdvertiser {
    pub async fn new(name: &str, port: u16) -> BridgeResult<Self> {
        info!("Linux service advertiser: {} on port {} (direct IP mode)", name, port);
        Ok(Self { _name: name.to_string(), _port: port })
    }
}

pub struct ServiceBrowser;

impl ServiceBrowser {
    pub async fn new() -> BridgeResult<Self> {
        Ok(Self)
    }

    pub async fn discover(&self) -> BridgeResult<Vec<ServerInfo>> {
        Ok(vec![])
    }
}
