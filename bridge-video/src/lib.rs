//! Bridge Video Subsystem
//!
//! Provides low-latency video capture, encoding, decoding, and display:
//! - Screen capture via ScreenCaptureKit (macOS 12.3+)
//! - Hardware encoding/decoding via VideoToolbox
//! - Metal-based rendering for display

pub mod capture;
pub mod codec;
pub mod display;
mod sys;

pub use capture::*;
pub use codec::*;
pub use display::*;

use bridge_common::{VideoConfig, VideoCodec, PixelFormat};

// is_capture_supported is re-exported from capture module

/// Check if hardware encoding is available
pub fn is_hw_encoding_available() -> bool {
    // VideoToolbox hardware encoding is available on all Apple Silicon Macs
    cfg!(target_os = "macos")
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
