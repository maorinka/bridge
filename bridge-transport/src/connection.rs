//! Connection management for Bridge transport

use bridge_common::{
    BridgeError, BridgeResult, ConnectionState, ControlMessage, HelloMessage,
    WelcomeMessage, AudioConfig, VideoConfig,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

use crate::{QuicConnection, UdpChannel, TransportConfig};

/// Manages a complete Bridge connection including control and data channels
pub struct BridgeConnection {
    /// Current connection state
    state: Arc<RwLock<ConnectionState>>,
    /// Remote address
    remote_addr: SocketAddr,
    /// Transport configuration
    config: TransportConfig,
    /// Control channel (QUIC)
    control: Option<QuicConnection>,
    /// Video channel (UDP)
    video_channel: Option<UdpChannel>,
    /// Audio channel (UDP)
    audio_channel: Option<UdpChannel>,
    /// Input channel (UDP)
    input_channel: Option<UdpChannel>,
    /// Negotiated video configuration
    video_config: Option<VideoConfig>,
    /// Negotiated audio configuration
    audio_config: Option<AudioConfig>,
}

impl BridgeConnection {
    /// Create a new connection to a remote server
    pub fn new(remote_addr: SocketAddr, config: TransportConfig) -> Self {
        Self {
            state: Arc::new(RwLock::new(ConnectionState::Disconnected)),
            remote_addr,
            config,
            control: None,
            video_channel: None,
            audio_channel: None,
            input_channel: None,
            video_config: None,
            audio_config: None,
        }
    }

    /// Get current connection state
    pub async fn state(&self) -> ConnectionState {
        *self.state.read().await
    }

    /// Connect to the remote server (client mode)
    pub async fn connect(&mut self, client_name: &str) -> BridgeResult<WelcomeMessage> {
        self.connect_with_config(client_name, VideoConfig::default(), AudioConfig::default()).await
    }

    /// Connect to the remote server with specific video/audio configuration (client mode)
    pub async fn connect_with_config(
        &mut self,
        client_name: &str,
        video_config: VideoConfig,
        audio_config: AudioConfig,
    ) -> BridgeResult<WelcomeMessage> {
        info!("Connecting to {}", self.remote_addr);
        *self.state.write().await = ConnectionState::Connecting;

        // Establish QUIC control connection
        let control_addr = SocketAddr::new(self.remote_addr.ip(), self.config.control_port);
        let mut control = QuicConnection::connect(control_addr).await?;

        *self.state.write().await = ConnectionState::Handshaking;

        // Send Hello message
        let hello = HelloMessage {
            client_name: client_name.to_string(),
            protocol_version: bridge_common::PROTOCOL_VERSION,
            video_config,
            audio_config,
        };

        control.send_control(ControlMessage::Hello(hello)).await?;

        // Wait for Welcome response
        let welcome = match control.recv_control().await? {
            ControlMessage::Welcome(w) => w,
            other => {
                error!("Unexpected response: {:?}", other);
                return Err(BridgeError::Protocol("Expected Welcome message".into()));
            }
        };

        info!("Connected to server: {}", welcome.server_name);

        // Set up UDP channels for data streams
        let video_addr = SocketAddr::new(self.remote_addr.ip(), welcome.video_port);
        let audio_addr = SocketAddr::new(self.remote_addr.ip(), welcome.audio_port);
        let input_addr = SocketAddr::new(self.remote_addr.ip(), self.config.input_port);

        self.video_channel = Some(UdpChannel::connect(video_addr, self.config.max_packet_size).await?);
        self.audio_channel = Some(UdpChannel::connect(audio_addr, self.config.max_packet_size).await?);
        self.input_channel = Some(UdpChannel::connect(input_addr, self.config.max_packet_size).await?);

        self.video_config = Some(welcome.video_config.clone());
        self.audio_config = Some(welcome.audio_config.clone());
        self.control = Some(control);

        *self.state.write().await = ConnectionState::Connected;

        Ok(welcome)
    }

    /// Accept a connection from a client (server mode)
    pub async fn accept(
        control: QuicConnection,
        config: TransportConfig,
        server_name: &str,
    ) -> BridgeResult<(Self, HelloMessage)> {
        let mut control = control;
        let remote_addr = control.remote_addr();

        // Wait for Hello message
        let hello = match control.recv_control().await? {
            ControlMessage::Hello(h) => h,
            other => {
                error!("Unexpected message: {:?}", other);
                return Err(BridgeError::Protocol("Expected Hello message".into()));
            }
        };

        info!("Client connecting: {}", hello.client_name);

        // Negotiate configuration
        let video_config = hello.video_config.clone(); // Accept client's request for now
        let audio_config = hello.audio_config.clone();

        // Set up UDP channels
        let video_channel = UdpChannel::bind(config.video_port, config.max_packet_size).await?;
        let audio_channel = UdpChannel::bind(config.audio_port, config.max_packet_size).await?;
        let input_channel = UdpChannel::bind(config.input_port, config.max_packet_size).await?;

        // Send Welcome response
        let welcome = WelcomeMessage {
            server_name: server_name.to_string(),
            video_config: video_config.clone(),
            audio_config: audio_config.clone(),
            video_port: config.video_port,
            audio_port: config.audio_port,
        };

        control.send_control(ControlMessage::Welcome(welcome)).await?;

        let conn = Self {
            state: Arc::new(RwLock::new(ConnectionState::Connected)),
            remote_addr,
            config,
            control: Some(control),
            video_channel: Some(video_channel),
            audio_channel: Some(audio_channel),
            input_channel: Some(input_channel),
            video_config: Some(video_config),
            audio_config: Some(audio_config),
        };

        Ok((conn, hello))
    }

    /// Get the video channel for sending/receiving frames
    pub fn video_channel(&mut self) -> Option<&mut UdpChannel> {
        self.video_channel.as_mut()
    }

    /// Get the audio channel for sending/receiving samples
    pub fn audio_channel(&mut self) -> Option<&mut UdpChannel> {
        self.audio_channel.as_mut()
    }

    /// Get the input channel for sending/receiving events
    pub fn input_channel(&mut self) -> Option<&mut UdpChannel> {
        self.input_channel.as_mut()
    }

    /// Get the control connection
    pub fn control(&mut self) -> Option<&mut QuicConnection> {
        self.control.as_mut()
    }

    /// Get negotiated video configuration
    pub fn video_config(&self) -> Option<&VideoConfig> {
        self.video_config.as_ref()
    }

    /// Get negotiated audio configuration
    pub fn audio_config(&self) -> Option<&AudioConfig> {
        self.audio_config.as_ref()
    }

    /// Send a control message
    pub async fn send_control(&mut self, msg: ControlMessage) -> BridgeResult<()> {
        if let Some(control) = &mut self.control {
            control.send_control(msg).await
        } else {
            Err(BridgeError::NotConnected)
        }
    }

    /// Receive a control message
    pub async fn recv_control(&mut self) -> BridgeResult<ControlMessage> {
        if let Some(control) = &mut self.control {
            control.recv_control().await
        } else {
            Err(BridgeError::NotConnected)
        }
    }

    /// Disconnect from the remote peer
    pub async fn disconnect(&mut self) -> BridgeResult<()> {
        info!("Disconnecting from {}", self.remote_addr);

        // Send disconnect message if control channel is available
        if let Some(control) = &mut self.control {
            let _ = control.send_control(ControlMessage::Disconnect).await;
        }

        self.control = None;
        self.video_channel = None;
        self.audio_channel = None;
        self.input_channel = None;

        *self.state.write().await = ConnectionState::Disconnected;

        Ok(())
    }
}

impl Drop for BridgeConnection {
    fn drop(&mut self) {
        // Channels will be dropped automatically
    }
}
