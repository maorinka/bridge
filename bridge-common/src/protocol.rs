//! Protocol message definitions for Bridge communication
//!
//! The Bridge protocol uses:
//! - UDP for video and audio data (low latency, unreliable)
//! - TCP/QUIC for control messages (reliable)

use serde::{Deserialize, Serialize};
use bytes::Bytes;

/// Protocol version for compatibility checking
pub const PROTOCOL_VERSION: u32 = 1;

/// Magic bytes for packet identification
pub const MAGIC: [u8; 4] = *b"BRDG";

/// Maximum packet size for UDP (MTU-safe)
pub const MAX_UDP_PACKET_SIZE: usize = 1400;

/// Maximum video frame size (for allocation)
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024; // 16MB for 4K frames

// ============================================================================
// Packet Header
// ============================================================================

/// Header present at the start of every packet
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[repr(C)]
pub struct PacketHeader {
    /// Magic bytes for identification
    pub magic: [u8; 4],
    /// Protocol version
    pub version: u32,
    /// Packet type
    pub packet_type: PacketType,
    /// Sequence number for ordering/loss detection
    pub sequence: u64,
    /// Timestamp in microseconds (for latency measurement)
    pub timestamp_us: u64,
    /// Payload length
    pub payload_len: u32,
}

impl PacketHeader {
    pub const SIZE: usize = 32;

    pub fn new(packet_type: PacketType, sequence: u64, payload_len: u32) -> Self {
        Self {
            magic: MAGIC,
            version: PROTOCOL_VERSION,
            packet_type,
            sequence,
            timestamp_us: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as u64,
            payload_len,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.magic == MAGIC && self.version == PROTOCOL_VERSION
    }
}

/// Type of packet being sent
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum PacketType {
    /// Control channel messages
    Control = 0,
    /// Video frame data
    Video = 1,
    /// Audio sample data
    Audio = 2,
    /// Input events
    Input = 3,
    /// Keepalive/ping
    Ping = 4,
    /// Pong response
    Pong = 5,
}

// ============================================================================
// Control Messages
// ============================================================================

/// Control channel messages (sent over reliable connection)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMessage {
    /// Initial connection handshake
    Hello(HelloMessage),
    /// Response to Hello
    Welcome(WelcomeMessage),
    /// Request video stream configuration
    ConfigureVideo(VideoConfig),
    /// Request audio stream configuration
    ConfigureAudio(AudioConfig),
    /// Start streaming
    StartStream,
    /// Stop streaming
    StopStream,
    /// Latency report for adaptive quality
    LatencyReport(LatencyReport),
    /// Disconnect gracefully
    Disconnect,
}

/// Initial handshake message from client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloMessage {
    /// Client name/identifier
    pub client_name: String,
    /// Supported protocol version
    pub protocol_version: u32,
    /// Requested video configuration
    pub video_config: VideoConfig,
    /// Requested audio configuration
    pub audio_config: AudioConfig,
}

/// Server response to Hello
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WelcomeMessage {
    /// Server name/identifier
    pub server_name: String,
    /// Negotiated video configuration
    pub video_config: VideoConfig,
    /// Negotiated audio configuration
    pub audio_config: AudioConfig,
    /// UDP port for video stream
    pub video_port: u16,
    /// UDP port for audio stream
    pub audio_port: u16,
}

// ============================================================================
// Video Types
// ============================================================================

/// Video stream configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoConfig {
    /// Frame width in pixels
    pub width: u32,
    /// Frame height in pixels
    pub height: u32,
    /// Target frame rate
    pub fps: u32,
    /// Video codec to use
    pub codec: VideoCodec,
    /// Target bitrate in bits per second
    pub bitrate: u32,
    /// Pixel format
    pub pixel_format: PixelFormat,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            width: 3840,
            height: 2160,
            fps: 60,
            codec: VideoCodec::H265,
            bitrate: 50_000_000, // 50 Mbps
            pixel_format: PixelFormat::Bgra8,
        }
    }
}

/// Supported video codecs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    /// Raw uncompressed (for testing/Thunderbolt)
    Raw,
    /// H.264/AVC
    H264,
    /// H.265/HEVC (preferred)
    H265,
}

/// Pixel format for raw video
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    /// 32-bit BGRA (native for macOS)
    Bgra8,
    /// 32-bit RGBA
    Rgba8,
    /// NV12 (YUV 4:2:0 semi-planar)
    Nv12,
    /// I420 (YUV 4:2:0 planar)
    I420,
}

/// Video frame header (prepended to video data)
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[repr(C)]
pub struct VideoFrameHeader {
    /// Frame number
    pub frame_number: u64,
    /// Presentation timestamp in microseconds
    pub pts_us: u64,
    /// Whether this is a keyframe
    pub is_keyframe: bool,
    /// Total frame size in bytes
    pub frame_size: u32,
    /// Fragment index (for frames split across packets)
    pub fragment_index: u16,
    /// Total number of fragments
    pub fragment_count: u16,
    /// Frame width
    pub width: u32,
    /// Frame height
    pub height: u32,
}

impl VideoFrameHeader {
    pub const SIZE: usize = 40;
}

// ============================================================================
// Audio Types
// ============================================================================

/// Audio stream configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of channels
    pub channels: u16,
    /// Audio codec
    pub codec: AudioCodec,
    /// Bits per sample (for raw audio)
    pub bits_per_sample: u16,
    /// Buffer size in samples
    pub buffer_size: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            codec: AudioCodec::Raw,
            bits_per_sample: 32, // Float32
            buffer_size: 480, // 10ms at 48kHz
        }
    }
}

/// Supported audio codecs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodec {
    /// Raw PCM (lowest latency)
    Raw,
    /// Opus (good quality, low latency)
    Opus,
}

/// Audio packet header
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[repr(C)]
pub struct AudioPacketHeader {
    /// Sequence number
    pub sequence: u64,
    /// Presentation timestamp in microseconds
    pub pts_us: u64,
    /// Number of samples in this packet
    pub sample_count: u32,
    /// Payload size in bytes
    pub payload_size: u32,
}

impl AudioPacketHeader {
    pub const SIZE: usize = 24;
}

// ============================================================================
// Input Types
// ============================================================================

/// Input event types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InputEvent {
    /// Keyboard event
    Keyboard(KeyboardEvent),
    /// Mouse movement
    MouseMove(MouseMoveEvent),
    /// Mouse button
    MouseButton(MouseButtonEvent),
    /// Mouse scroll
    MouseScroll(MouseScrollEvent),
    /// Raw HID report (for gaming peripherals)
    RawHid(RawHidEvent),
}

/// Keyboard event
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct KeyboardEvent {
    /// Virtual key code (macOS key code)
    pub key_code: u16,
    /// Whether key is pressed (true) or released (false)
    pub is_pressed: bool,
    /// Modifier flags
    pub modifiers: ModifierFlags,
    /// Timestamp in microseconds
    pub timestamp_us: u64,
}

/// Mouse movement event
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MouseMoveEvent {
    /// Absolute X position (0.0 - 1.0 normalized)
    pub x: f64,
    /// Absolute Y position (0.0 - 1.0 normalized)
    pub y: f64,
    /// Delta X (for relative mode)
    pub delta_x: f64,
    /// Delta Y (for relative mode)
    pub delta_y: f64,
    /// Whether to use absolute or relative positioning
    pub is_absolute: bool,
    /// Timestamp in microseconds
    pub timestamp_us: u64,
}

/// Mouse button event
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MouseButtonEvent {
    /// Button number (0 = left, 1 = right, 2 = middle, etc.)
    pub button: u8,
    /// Whether button is pressed (true) or released (false)
    pub is_pressed: bool,
    /// Click count (1 = single, 2 = double, etc.)
    pub click_count: u8,
    /// Timestamp in microseconds
    pub timestamp_us: u64,
}

/// Mouse scroll event
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MouseScrollEvent {
    /// Horizontal scroll delta
    pub delta_x: f64,
    /// Vertical scroll delta
    pub delta_y: f64,
    /// Whether this is a continuous (trackpad) or discrete (wheel) scroll
    pub is_continuous: bool,
    /// Timestamp in microseconds
    pub timestamp_us: u64,
}

/// Raw HID report for gaming peripherals
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawHidEvent {
    /// Device vendor ID
    pub vendor_id: u16,
    /// Device product ID
    pub product_id: u16,
    /// Report ID
    pub report_id: u8,
    /// Raw report data
    pub data: Vec<u8>,
    /// Timestamp in microseconds
    pub timestamp_us: u64,
}

/// Keyboard modifier flags
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ModifierFlags {
    pub shift: bool,
    pub control: bool,
    pub option: bool,
    pub command: bool,
    pub caps_lock: bool,
    pub fn_key: bool,
}

// ============================================================================
// Latency Monitoring
// ============================================================================

/// Latency report for adaptive quality
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyReport {
    /// Round-trip time in microseconds
    pub rtt_us: u64,
    /// Video decode latency in microseconds
    pub decode_latency_us: u64,
    /// Display latency in microseconds
    pub display_latency_us: u64,
    /// Packet loss percentage (0.0 - 1.0)
    pub packet_loss: f32,
    /// Jitter in microseconds
    pub jitter_us: u64,
}

// ============================================================================
// Serialization Helpers
// ============================================================================

impl PacketHeader {
    /// Serialize header to bytes
    pub fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    /// Deserialize header from bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }
}

impl ControlMessage {
    /// Serialize message to bytes
    pub fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    /// Deserialize message from bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }
}

impl InputEvent {
    /// Serialize event to bytes
    pub fn to_bytes(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }

    /// Deserialize event from bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self, bincode::Error> {
        bincode::deserialize(data)
    }
}
