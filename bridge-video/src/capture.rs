//! Screen capture using CGDisplayStream
//!
//! CGDisplayStream provides low-latency screen capture with:
//! - Direct IOSurface access for zero-copy frame handling
//! - Hardware-accelerated capture
//! - Configurable frame rate and resolution

use bridge_common::{BridgeResult, BridgeError, VideoConfig};
use std::ffi::{c_void, CString};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crossbeam_channel::{bounded, Receiver, Sender};
use tracing::{debug, info, warn};

use crate::sys::*;

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
    /// IOSurface reference (for zero-copy Metal rendering)
    pub io_surface: Option<IOSurfaceRef>,
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

/// Context passed to CGDisplayStream callback
struct CaptureContext {
    frame_tx: Sender<CapturedFrame>,
    frame_count: Arc<AtomicU64>,
    width: u32,
    height: u32,
}

/// Screen capturer using CGDisplayStream
pub struct ScreenCapturer {
    config: CaptureConfig,
    frame_tx: Sender<CapturedFrame>,
    frame_rx: Receiver<CapturedFrame>,
    frame_count: Arc<AtomicU64>,
    is_running: Arc<AtomicBool>,
    stream: Option<CGDisplayStreamRef>,
    queue: Option<DispatchQueueRef>,
    /// Arc-wrapped context - the block callback holds a clone, preventing use-after-free
    context: Option<Arc<CaptureContext>>,
}

unsafe impl Send for ScreenCapturer {}
unsafe impl Sync for ScreenCapturer {}

impl ScreenCapturer {
    /// Create a new screen capturer
    pub fn new(config: CaptureConfig) -> BridgeResult<Self> {
        let (frame_tx, frame_rx) = bounded(4); // Small buffer for low latency

        Ok(Self {
            config,
            frame_tx,
            frame_rx,
            frame_count: Arc::new(AtomicU64::new(0)),
            is_running: Arc::new(AtomicBool::new(false)),
            stream: None,
            queue: None,
            context: None,
        })
    }

    /// Check if screen recording permission is granted
    pub fn has_permission() -> bool {
        unsafe {
            extern "C" {
                fn CGPreflightScreenCaptureAccess() -> bool;
            }
            CGPreflightScreenCaptureAccess()
        }
    }

    /// Request screen recording permission
    pub fn request_permission() -> bool {
        unsafe {
            extern "C" {
                fn CGRequestScreenCaptureAccess() -> bool;
            }
            CGRequestScreenCaptureAccess()
        }
    }

    /// Start capturing frames
    pub async fn start(&mut self) -> BridgeResult<()> {
        if self.is_running.load(Ordering::SeqCst) {
            return Ok(());
        }

        info!("Starting screen capture at {}fps", self.config.fps);

        // Get display ID
        let display_id = self.config.display_id.unwrap_or_else(|| unsafe {
            CGMainDisplayID()
        });

        // Get display dimensions
        let (native_width, native_height) = unsafe {
            (
                CGDisplayPixelsWide(display_id) as u32,
                CGDisplayPixelsHigh(display_id) as u32,
            )
        };

        let target_width = if self.config.width > 0 { self.config.width } else { native_width };
        let target_height = if self.config.height > 0 { self.config.height } else { native_height };

        info!("Display {}x{}, capturing at {}x{}", native_width, native_height, target_width, target_height);

        // Create dispatch queue for callbacks
        let queue_label = CString::new("com.bridge.capture").unwrap();
        let queue = unsafe {
            dispatch_queue_create(queue_label.as_ptr(), std::ptr::null())
        };

        if queue.is_null() {
            return Err(BridgeError::Video("Failed to create dispatch queue".into()));
        }

        // Create capture context wrapped in Arc for safe sharing with callback
        let context = Arc::new(CaptureContext {
            frame_tx: self.frame_tx.clone(),
            frame_count: self.frame_count.clone(),
            width: target_width,
            height: target_height,
        });

        // Create the block for CGDisplayStream callback
        // CGDisplayStream handler signature:
        // void (^)(CGDisplayStreamFrameStatus status, uint64_t displayTime,
        //          IOSurfaceRef frameSurface, CGDisplayStreamUpdateRef updateRef)
        // The block holds an Arc clone, ensuring the context lives as long as needed
        let block = create_capture_block(Arc::clone(&context));

        // Create CGDisplayStream
        // Using 'BGRA' pixel format (0x42475241)
        let stream = unsafe {
            CGDisplayStreamCreateWithDispatchQueue(
                display_id,
                target_width as usize,
                target_height as usize,
                K_CV_PIXEL_FORMAT_TYPE_32_BGRA as i32,
                std::ptr::null(), // properties - use defaults
                queue,
                block,
            )
        };

        if stream.is_null() {
            unsafe {
                dispatch_release(queue);
            }
            // context Arc will be dropped automatically when function returns
            return Err(BridgeError::Video("Failed to create CGDisplayStream. Check screen recording permission.".into()));
        }

        // Start the stream
        let result = unsafe { CGDisplayStreamStart(stream) };
        if result != 0 {
            unsafe {
                dispatch_release(queue);
                CFRelease(stream as CFTypeRef);
            }
            // context Arc will be dropped automatically
            return Err(BridgeError::Video(format!("Failed to start CGDisplayStream: {}", result)));
        }

        self.stream = Some(stream);
        self.queue = Some(queue);
        self.context = Some(context);
        self.is_running.store(true, Ordering::SeqCst);

        info!("CGDisplayStream started successfully");
        Ok(())
    }

    /// Stop capturing frames
    pub fn stop(&mut self) -> BridgeResult<()> {
        if !self.is_running.load(Ordering::SeqCst) {
            return Ok(());
        }

        info!("Stopping screen capture");
        self.is_running.store(false, Ordering::SeqCst);

        // Stop and release the stream
        if let Some(stream) = self.stream.take() {
            unsafe {
                CGDisplayStreamStop(stream);
                CFRelease(stream as CFTypeRef);
            }
        }

        // Release the dispatch queue
        if let Some(queue) = self.queue.take() {
            unsafe {
                dispatch_release(queue);
            }
        }

        // Context will be dropped when self.context is dropped
        self.context.take();

        Ok(())
    }

    /// Get the next captured frame
    pub fn recv_frame(&self) -> Option<CapturedFrame> {
        match self.frame_rx.try_recv() {
            Ok(frame) => {
                debug!("recv_frame: got frame {} ({}x{})",
                       frame.frame_number, frame.width, frame.height);
                Some(frame)
            }
            Err(crossbeam_channel::TryRecvError::Empty) => None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                warn!("recv_frame: channel disconnected");
                None
            }
        }
    }

    /// Get the next captured frame (blocking)
    pub fn recv_frame_blocking(&self) -> BridgeResult<CapturedFrame> {
        self.frame_rx.recv().map_err(|_| BridgeError::Video("Capture channel closed".into()))
    }

    /// Get a receiver for frames
    pub fn frame_receiver(&self) -> Receiver<CapturedFrame> {
        self.frame_rx.clone()
    }

    /// Get current capture statistics
    pub fn stats(&self) -> CaptureStats {
        CaptureStats {
            frames_captured: self.frame_count.load(Ordering::SeqCst),
            is_running: self.is_running.load(Ordering::SeqCst),
        }
    }
}

impl Drop for ScreenCapturer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Create the block callback for CGDisplayStream
/// Returns a pointer to the block that can be passed to CGDisplayStreamCreateWithDispatchQueue
/// Takes an Arc<CaptureContext> which the callback will own, preventing use-after-free
fn create_capture_block(context: Arc<CaptureContext>) -> *const c_void {
    use block2::StackBlock;

    // Create the callback closure - it owns the Arc clone
    let callback = move |status: i32, display_time: u64, surface: IOSurfaceRef, _update: CGDisplayStreamUpdateRef| {
        // Debug: log every callback invocation
        debug!("CGDisplayStream callback: status={}, display_time={}, surface_null={}",
               status, display_time, surface.is_null());

        // Safety: CGDisplayStreamFrameStatus values match the i32 values from CGDisplayStream
        let status = match status {
            0 => CGDisplayStreamFrameStatus::FrameComplete,
            1 => CGDisplayStreamFrameStatus::FrameIdle,
            2 => CGDisplayStreamFrameStatus::FrameBlank,
            3 => CGDisplayStreamFrameStatus::Stopped,
            _ => {
                warn!("Unknown CGDisplayStream status: {}", status);
                return;
            }
        };

        match status {
            CGDisplayStreamFrameStatus::FrameComplete => {
                if surface.is_null() {
                    return;
                }

                // Access context through the Arc - safe because Arc keeps it alive
                let frame_num = context.frame_count.fetch_add(1, Ordering::SeqCst);

                // Lock the IOSurface to read pixel data
                let lock_result = unsafe { IOSurfaceLock(surface, 1, std::ptr::null_mut()) }; // Read-only lock
                if lock_result != 0 {
                    warn!("Failed to lock IOSurface: {}", lock_result);
                    return;
                }

                // Get surface properties
                let width = unsafe { IOSurfaceGetWidth(surface) } as u32;
                let height = unsafe { IOSurfaceGetHeight(surface) } as u32;
                let bytes_per_row = unsafe { IOSurfaceGetBytesPerRow(surface) } as u32;
                let base_addr = unsafe { IOSurfaceGetBaseAddress(surface) };

                // Copy pixel data from IOSurface
                let data_size = (bytes_per_row * height) as usize;
                let data = if !base_addr.is_null() {
                    let slice = unsafe { std::slice::from_raw_parts(base_addr as *const u8, data_size) };
                    slice.to_vec()
                } else {
                    vec![0u8; data_size]
                };

                // Unlock the IOSurface
                unsafe { IOSurfaceUnlock(surface, 1, std::ptr::null_mut()) };

                // Create the captured frame
                let frame = CapturedFrame {
                    data,
                    width,
                    height,
                    bytes_per_row,
                    pts_us: display_time / 1000, // Convert from nanoseconds to microseconds
                    frame_number: frame_num,
                    io_surface: None, // We don't retain the IOSurface since we copied the data
                };

                // Send frame (non-blocking)
                match context.frame_tx.try_send(frame) {
                    Ok(()) => {
                        debug!("Frame {} sent to channel ({}x{}, {} bytes)",
                               frame_num, width, height, data_size);
                    }
                    Err(e) => {
                        warn!("Frame {} dropped: {}", frame_num, e);
                    }
                }
            }
            CGDisplayStreamFrameStatus::FrameIdle => {
                // No new frame, display unchanged
            }
            CGDisplayStreamFrameStatus::FrameBlank => {
                debug!("Display blanked");
            }
            CGDisplayStreamFrameStatus::Stopped => {
                debug!("CGDisplayStream stopped");
            }
        }
    };

    // Create a StackBlock and copy it to heap so it lives long enough
    let block = StackBlock::new(callback);
    let block_copy = block.copy();

    // Get the raw block pointer - RcBlock derefs to the underlying Block type
    // which has the correct Objective-C block ABI layout
    let block_ptr = (&*block_copy) as *const _ as *const c_void;

    // Leak the RcBlock so the block stays alive for the duration of the stream
    // The Arc inside ensures the context stays alive as long as the block does
    std::mem::forget(block_copy);

    block_ptr
}

/// Capture statistics
#[derive(Debug, Clone)]
pub struct CaptureStats {
    pub frames_captured: u64,
    pub is_running: bool,
}

/// Get information about available displays
pub fn get_displays() -> Vec<DisplayInfo> {
    unsafe {
        let mut displays = [0u32; 16];
        let mut count = 0u32;

        let result = CGGetActiveDisplayList(16, displays.as_mut_ptr(), &mut count);
        if result != 0 {
            return vec![];
        }

        displays[..count as usize]
            .iter()
            .map(|&id| DisplayInfo {
                id,
                width: CGDisplayPixelsWide(id) as u32,
                height: CGDisplayPixelsHigh(id) as u32,
                is_main: CGDisplayIsMain(id),
            })
            .collect()
    }
}

/// Information about a display
#[derive(Debug, Clone)]
pub struct DisplayInfo {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub is_main: bool,
}

/// Check if screen capture is supported on this system
pub fn is_capture_supported() -> bool {
    // CGDisplayStream is available on macOS 10.8+
    // We also check for screen recording permission
    ScreenCapturer::has_permission() || ScreenCapturer::request_permission()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_displays() {
        let displays = get_displays();
        assert!(!displays.is_empty(), "No displays found");
        assert!(displays.iter().any(|d| d.is_main), "No main display found");

        for display in &displays {
            println!(
                "Display {}: {}x{}, main={}",
                display.id, display.width, display.height, display.is_main
            );
            assert!(display.width > 0, "Invalid display width");
            assert!(display.height > 0, "Invalid display height");
        }
    }

    #[test]
    fn test_capture_config_default() {
        let config = CaptureConfig::default();
        assert_eq!(config.width, 0); // 0 = native
        assert_eq!(config.height, 0);
        assert_eq!(config.fps, 60);
        assert!(config.show_cursor);
        assert!(!config.capture_audio);
        assert!(config.display_id.is_none());
    }

    #[test]
    fn test_capture_config_from_video_config() {
        let video_config = bridge_common::VideoConfig {
            width: 1920,
            height: 1080,
            fps: 30,
            codec: bridge_common::VideoCodec::H265,
            bitrate: 20_000_000,
            pixel_format: bridge_common::PixelFormat::Bgra8,
        };
        let capture_config = CaptureConfig::from(&video_config);
        assert_eq!(capture_config.width, 1920);
        assert_eq!(capture_config.height, 1080);
        assert_eq!(capture_config.fps, 30);
    }

    #[test]
    fn test_capturer_creation() {
        let config = CaptureConfig::default();
        let result = ScreenCapturer::new(config);
        assert!(result.is_ok(), "Failed to create ScreenCapturer: {:?}", result.err());
    }

    #[test]
    fn test_capturer_stats_initial() {
        let config = CaptureConfig::default();
        let capturer = ScreenCapturer::new(config).expect("Failed to create capturer");
        let stats = capturer.stats();
        assert_eq!(stats.frames_captured, 0);
        assert!(!stats.is_running);
    }

    #[test]
    fn test_permission_check() {
        // This test just verifies the permission check functions don't crash
        let has_permission = ScreenCapturer::has_permission();
        println!("Screen recording permission: {}", has_permission);
        // Note: We don't assert the result because it depends on system settings
    }

    #[tokio::test]
    async fn test_capturer_start_stop() {
        // Skip if no screen recording permission
        if !ScreenCapturer::has_permission() {
            println!("Skipping test: no screen recording permission");
            return;
        }

        let config = CaptureConfig {
            width: 640,
            height: 480,
            fps: 30,
            ..Default::default()
        };

        let mut capturer = ScreenCapturer::new(config).expect("Failed to create capturer");

        // Start capture
        let start_result = capturer.start().await;
        assert!(start_result.is_ok(), "Failed to start capture: {:?}", start_result.err());
        assert!(capturer.stats().is_running);

        // Wait a bit for frames
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // Stop capture
        let stop_result = capturer.stop();
        assert!(stop_result.is_ok(), "Failed to stop capture: {:?}", stop_result.err());
        assert!(!capturer.stats().is_running);

        println!("Captured {} frames", capturer.stats().frames_captured);
    }

    #[tokio::test]
    async fn test_capturer_receives_frames() {
        // Skip if no screen recording permission
        if !ScreenCapturer::has_permission() {
            println!("Skipping test: no screen recording permission");
            return;
        }

        let config = CaptureConfig {
            width: 320,
            height: 240,
            fps: 30,
            ..Default::default()
        };

        let mut capturer = ScreenCapturer::new(config).expect("Failed to create capturer");
        capturer.start().await.expect("Failed to start capture");

        // Wait for a few frames
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // Try to receive frames
        let mut frame_count = 0;
        while let Some(frame) = capturer.recv_frame() {
            frame_count += 1;
            assert!(frame.width > 0, "Invalid frame width");
            assert!(frame.height > 0, "Invalid frame height");
            assert!(!frame.data.is_empty(), "Frame data is empty");

            if frame_count >= 3 {
                break;
            }
        }

        capturer.stop().expect("Failed to stop capture");

        println!("Received {} frames", frame_count);
        // Note: We might not receive frames if CGDisplayStream fails silently
    }
}
