//! Bridge Audio Subsystem
//!
//! Provides low-latency audio capture and playback:
//! - Audio capture via CoreAudio (or DriverKit virtual device)
//! - Audio playback via CoreAudio with low-latency buffer configuration

pub mod capture;
pub mod playback;

pub use capture::*;
pub use playback::*;

use bridge_common::{AudioConfig, AudioCodec};

/// Calculate bytes per audio sample
pub fn bytes_per_sample(config: &AudioConfig) -> usize {
    (config.bits_per_sample as usize / 8) * config.channels as usize
}

/// Calculate buffer size in bytes
pub fn buffer_size_bytes(config: &AudioConfig) -> usize {
    bytes_per_sample(config) * config.buffer_size as usize
}

/// Calculate latency in milliseconds for a buffer size
pub fn buffer_latency_ms(config: &AudioConfig) -> f64 {
    (config.buffer_size as f64 / config.sample_rate as f64) * 1000.0
}
