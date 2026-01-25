//! Common types used across Bridge components

use std::net::SocketAddr;
use serde::{Deserialize, Serialize};

/// Connection state for a Bridge session
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Not connected
    Disconnected,
    /// Attempting to connect
    Connecting,
    /// Connected and handshaking
    Handshaking,
    /// Fully connected and streaming
    Connected,
    /// Reconnecting after connection loss
    Reconnecting,
}

/// Information about a discovered Bridge server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Server name
    pub name: String,
    /// Server address
    pub address: SocketAddr,
    /// Server hostname
    pub hostname: String,
    /// Screen resolution
    pub resolution: Resolution,
    /// Whether server is available
    pub available: bool,
}

/// Screen resolution
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// 4K UHD resolution
    pub const UHD_4K: Self = Self::new(3840, 2160);
    /// 1440p resolution
    pub const QHD: Self = Self::new(2560, 1440);
    /// 1080p resolution
    pub const FHD: Self = Self::new(1920, 1080);
    /// 720p resolution
    pub const HD: Self = Self::new(1280, 720);

    pub fn pixels(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    pub fn aspect_ratio(&self) -> f64 {
        self.width as f64 / self.height as f64
    }
}

impl std::fmt::Display for Resolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// Rectangle for screen regions
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { x, y, width, height }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

/// Statistics for monitoring performance
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    /// Frames sent/received
    pub frames: u64,
    /// Bytes sent/received
    pub bytes: u64,
    /// Dropped frames
    pub dropped_frames: u64,
    /// Average frame time in microseconds
    pub avg_frame_time_us: u64,
    /// Current bitrate in bits per second
    pub bitrate_bps: u64,
    /// Packet loss percentage
    pub packet_loss: f32,
    /// Round-trip time in microseconds
    pub rtt_us: u64,
}

/// Buffer for video frames with metadata
#[derive(Debug)]
pub struct FrameBuffer {
    /// Raw pixel data
    pub data: Vec<u8>,
    /// Frame width
    pub width: u32,
    /// Frame height
    pub height: u32,
    /// Bytes per row (may include padding)
    pub bytes_per_row: u32,
    /// Pixel format
    pub format: crate::PixelFormat,
    /// Presentation timestamp in microseconds
    pub pts_us: u64,
    /// Frame number
    pub frame_number: u64,
}

impl FrameBuffer {
    pub fn new(width: u32, height: u32, format: crate::PixelFormat) -> Self {
        let bytes_per_pixel = match format {
            crate::PixelFormat::Bgra8 | crate::PixelFormat::Rgba8 => 4,
            crate::PixelFormat::Nv12 => 1, // Actually 1.5 bytes per pixel average
            crate::PixelFormat::I420 => 1, // Actually 1.5 bytes per pixel average
        };
        let bytes_per_row = width * bytes_per_pixel;
        let data_size = match format {
            crate::PixelFormat::Bgra8 | crate::PixelFormat::Rgba8 => {
                (bytes_per_row * height) as usize
            }
            crate::PixelFormat::Nv12 | crate::PixelFormat::I420 => {
                // Y plane + UV plane (half resolution)
                (width * height + width * height / 2) as usize
            }
        };

        Self {
            data: vec![0u8; data_size],
            width,
            height,
            bytes_per_row,
            format,
            pts_us: 0,
            frame_number: 0,
        }
    }
}

/// Ring buffer for frame timing
pub struct TimingRingBuffer {
    times: Vec<u64>,
    index: usize,
    count: usize,
}

impl TimingRingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            times: vec![0; capacity],
            index: 0,
            count: 0,
        }
    }

    pub fn push(&mut self, time_us: u64) {
        self.times[self.index] = time_us;
        self.index = (self.index + 1) % self.times.len();
        if self.count < self.times.len() {
            self.count += 1;
        }
    }

    pub fn average(&self) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let sum: u64 = self.times[..self.count].iter().sum();
        sum / self.count as u64
    }

    pub fn min(&self) -> u64 {
        if self.count == 0 {
            return 0;
        }
        *self.times[..self.count].iter().min().unwrap_or(&0)
    }

    pub fn max(&self) -> u64 {
        if self.count == 0 {
            return 0;
        }
        *self.times[..self.count].iter().max().unwrap_or(&0)
    }
}

/// Timestamp utilities
pub fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

/// Calculate microseconds elapsed since a timestamp
pub fn elapsed_us(start_us: u64) -> u64 {
    now_us().saturating_sub(start_us)
}
