//! Bridge Video Subsystem
//!
//! Provides low-latency video capture, encoding, decoding, and display.
//!
//! Shared types (CapturedFrame, EncodedFrame, etc.) live in the top-level modules.
//! Platform-specific implementations live in `macos/` and `linux/` subdirectories.

pub mod capture;
pub mod codec;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub mod linux;

pub use capture::*;
pub use codec::*;

// Re-export platform-specific implementations
#[cfg(target_os = "macos")]
pub use macos::capture::{ScreenCapturer, get_displays, is_capture_supported};
#[cfg(target_os = "macos")]
pub use macos::codec::{VideoEncoder, VideoDecoder};
#[cfg(target_os = "macos")]
pub use macos::display::{MetalDisplay, DisplayStats};

#[cfg(target_os = "macos")]
pub mod virtual_display {
    pub use crate::macos::virtual_display::*;
}

use bridge_common::{VideoConfig, VideoCodec, PixelFormat};

/// Check if hardware encoding is available
pub fn is_hw_encoding_available() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// Calculate bytes per frame for a given configuration
pub fn bytes_per_frame(config: &VideoConfig) -> usize {
    let pixels = config.width as usize * config.height as usize;
    match config.pixel_format {
        PixelFormat::Bgra8 | PixelFormat::Rgba8 => pixels * 4,
        PixelFormat::Nv12 | PixelFormat::I420 => pixels * 3 / 2,
    }
}

/// Calculate raw bitrate for uncompressed video
pub fn raw_bitrate(config: &VideoConfig) -> u64 {
    let bytes = bytes_per_frame(config);
    bytes as u64 * config.fps as u64 * 8
}

/// Recommended bitrate for compressed video
pub fn recommended_bitrate(config: &VideoConfig) -> u32 {
    let pixels = config.width as u64 * config.height as u64;
    let fps = config.fps as u64;

    // Base bitrate calculation (roughly)
    let base = match config.codec {
        VideoCodec::H265 => pixels * fps / 100, // ~10 bits per pixel for HEVC
        VideoCodec::H264 => pixels * fps / 50,  // ~20 bits per pixel for AVC
        VideoCodec::Raw => pixels * fps * 32,   // 32 bits per pixel
    };

    // Cap at reasonable values
    base.min(200_000_000) as u32 // Max 200 Mbps
}
