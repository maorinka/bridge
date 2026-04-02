//! Shared codec types used across all platforms
//!
//! Platform-specific encoder/decoder implementations live in:
//! - `macos/codec.rs` — VideoToolbox hardware encoding/decoding
//! - `linux/v4l2_encoder.rs` — NVIDIA V4L2/GStreamer encoding

use bridge_common::{VideoCodec, VideoConfig};
use bytes::Bytes;

/// Encoded video frame
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// Encoded data (H.264/H.265 NAL units)
    pub data: Bytes,
    /// Presentation timestamp in microseconds
    pub pts_us: u64,
    /// Decode timestamp in microseconds
    pub dts_us: u64,
    /// Whether this is a keyframe
    pub is_keyframe: bool,
    /// Frame number
    pub frame_number: u64,
}

/// Decoded video frame
#[derive(Debug)]
pub struct DecodedFrame {
    /// Pixel data (BGRA) — empty in zero-copy path
    pub data: Vec<u8>,
    /// Frame width
    pub width: u32,
    /// Frame height
    pub height: u32,
    /// Bytes per row
    pub bytes_per_row: u32,
    /// Presentation timestamp in microseconds
    pub pts_us: u64,
    /// IOSurface for zero-copy Metal rendering — macOS only
    #[cfg(target_os = "macos")]
    pub io_surface: Option<crate::macos::sys::IOSurfaceRef>,
    /// Retained CVPixelBuffer that owns the IOSurface — released on drop (macOS only)
    #[cfg(target_os = "macos")]
    pub cv_pixel_buffer: Option<crate::macos::sys::CVPixelBufferRef>,
}

unsafe impl Send for DecodedFrame {}

#[cfg(target_os = "macos")]
impl Drop for DecodedFrame {
    fn drop(&mut self) {
        if let Some(pb) = self.cv_pixel_buffer.take() {
            unsafe {
                crate::macos::sys::CVPixelBufferRelease(pb);
            }
        }
    }
}

/// Video encoder configuration
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    /// Video width
    pub width: u32,
    /// Video height
    pub height: u32,
    /// Target frame rate
    pub fps: u32,
    /// Target bitrate in bits per second
    pub bitrate: u32,
    /// Codec to use
    pub codec: VideoCodec,
    /// Keyframe interval in frames
    pub keyframe_interval: u32,
    /// Enable low-latency mode
    pub low_latency: bool,
    /// Enable real-time encoding
    pub realtime: bool,
    /// Maximum quality mode (for high-bandwidth connections like Thunderbolt)
    pub max_quality: bool,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate: 60_000_000,
            codec: VideoCodec::H265,
            keyframe_interval: 60,
            low_latency: true,
            realtime: true,
            max_quality: false,
        }
    }
}

impl From<&VideoConfig> for EncoderConfig {
    fn from(config: &VideoConfig) -> Self {
        Self {
            width: config.width,
            height: config.height,
            fps: config.fps,
            bitrate: config.bitrate,
            codec: config.codec,
            ..Default::default()
        }
    }
}
