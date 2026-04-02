//! Shared capture types used across all platforms
//!
//! Platform-specific capture implementations live in:
//! - `macos/capture.rs` — ScreenCaptureKit / CGDisplayStream
//! - `linux/` — (future) X11/XCB capture

use bridge_common::VideoConfig;
use tracing::warn;

/// Captured frame with metadata
#[derive(Debug)]
pub struct CapturedFrame {
    /// Pixel data (BGRA format)
    pub data: Vec<u8>,
    /// Frame width
    pub width: u32,
    /// Frame height
    pub height: u32,
    /// Bytes per row
    pub bytes_per_row: u32,
    /// Presentation timestamp in microseconds
    pub pts_us: u64,
    /// Frame number
    pub frame_number: u64,
    /// IOSurface reference (for zero-copy Metal rendering) — macOS only
    #[cfg(target_os = "macos")]
    pub io_surface: Option<crate::macos::sys::IOSurfaceRef>,
    /// DMA-BUF file descriptor (for zero-copy GPU access) — Linux only
    #[cfg(target_os = "linux")]
    pub dma_buf_fd: Option<i32>,
}

impl CapturedFrame {
    /// Read pixel data from the frame.
    /// In the zero-copy path, `data` is empty and this reads from the platform surface.
    pub fn read_pixel_data(&self) -> Vec<u8> {
        if !self.data.is_empty() {
            return self.data.clone();
        }

        #[cfg(target_os = "macos")]
        if let Some(surface) = self.io_surface {
            unsafe {
                use crate::macos::sys::*;
                let lock_result = IOSurfaceLock(surface, 1, std::ptr::null_mut());
                if lock_result != 0 {
                    warn!("Failed to lock IOSurface for read: {}", lock_result);
                    return Vec::new();
                }

                let base_addr = IOSurfaceGetBaseAddress(surface);
                let data_size = (self.bytes_per_row * self.height) as usize;

                let data = if !base_addr.is_null() {
                    let slice = std::slice::from_raw_parts(base_addr as *const u8, data_size);
                    slice.to_vec()
                } else {
                    vec![0u8; data_size]
                };

                IOSurfaceUnlock(surface, 1, std::ptr::null_mut());
                data
            }
        } else {
            Vec::new()
        }

        #[cfg(not(target_os = "macos"))]
        Vec::new()
    }
}

#[cfg(target_os = "macos")]
impl Drop for CapturedFrame {
    fn drop(&mut self) {
        if let Some(surface) = self.io_surface.take() {
            unsafe {
                use crate::macos::sys::*;
                IOSurfaceDecrementUseCount(surface);
                CFRelease(surface as CFTypeRef);
            }
        }
    }
}

unsafe impl Send for CapturedFrame {}

/// Screen capture configuration
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Target width (0 = native)
    pub width: u32,
    /// Target height (0 = native)
    pub height: u32,
    /// Target frame rate
    pub fps: u32,
    /// Capture cursor
    pub show_cursor: bool,
    /// Capture audio (not implemented yet)
    pub capture_audio: bool,
    /// Display to capture (None = main display)
    pub display_id: Option<u32>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            fps: 60,
            show_cursor: true,
            capture_audio: false,
            display_id: None,
        }
    }
}

impl From<&VideoConfig> for CaptureConfig {
    fn from(config: &VideoConfig) -> Self {
        Self {
            width: config.width,
            height: config.height,
            fps: config.fps,
            ..Default::default()
        }
    }
}

/// Capture statistics
#[derive(Debug, Clone)]
pub struct CaptureStats {
    pub frames_captured: u64,
    pub is_running: bool,
}

/// Information about a display
#[derive(Debug, Clone)]
pub struct DisplayInfo {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub is_main: bool,
}
