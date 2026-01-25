//! Error types for Bridge

use thiserror::Error;

/// Main error type for Bridge operations
#[derive(Error, Debug)]
pub enum BridgeError {
    #[error("Connection error: {0}")]
    Connection(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Video error: {0}")]
    Video(String),

    #[error("Audio error: {0}")]
    Audio(String),

    #[error("Input error: {0}")]
    Input(String),

    #[error("Transport error: {0}")]
    Transport(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Timeout")]
    Timeout,

    #[error("Not connected")]
    NotConnected,

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("macOS error: {0}")]
    MacOS(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),
}

impl From<bincode::Error> for BridgeError {
    fn from(e: bincode::Error) -> Self {
        BridgeError::Serialization(e.to_string())
    }
}

/// Result type alias for Bridge operations
pub type BridgeResult<T> = Result<T, BridgeError>;

/// Video-specific errors
#[derive(Error, Debug)]
pub enum VideoError {
    #[error("Screen capture not available")]
    CaptureNotAvailable,

    #[error("Screen recording permission required")]
    PermissionRequired,

    #[error("Encoder error: {0}")]
    Encoder(String),

    #[error("Decoder error: {0}")]
    Decoder(String),

    #[error("Frame error: {0}")]
    Frame(String),

    #[error("Display error: {0}")]
    Display(String),

    #[error("Metal error: {0}")]
    Metal(String),

    #[error("VideoToolbox error: code {0}")]
    VideoToolbox(i32),
}

/// Audio-specific errors
#[derive(Error, Debug)]
pub enum AudioError {
    #[error("Audio device not found")]
    DeviceNotFound,

    #[error("Audio capture error: {0}")]
    Capture(String),

    #[error("Audio playback error: {0}")]
    Playback(String),

    #[error("CoreAudio error: code {0}")]
    CoreAudio(i32),
}

/// Input-specific errors
#[derive(Error, Debug)]
pub enum InputError {
    #[error("Accessibility permission required")]
    AccessibilityRequired,

    #[error("HID device error: {0}")]
    HidDevice(String),

    #[error("Event tap error: {0}")]
    EventTap(String),

    #[error("Virtual device error: {0}")]
    VirtualDevice(String),

    #[error("IOKit error: code {0}")]
    IOKit(i32),
}

/// Transport-specific errors
#[derive(Error, Debug)]
pub enum TransportError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Connection lost")]
    ConnectionLost,

    #[error("Handshake failed: {0}")]
    HandshakeFailed(String),

    #[error("Send error: {0}")]
    Send(String),

    #[error("Receive error: {0}")]
    Receive(String),

    #[error("Invalid packet")]
    InvalidPacket,

    #[error("Timeout")]
    Timeout,
}
