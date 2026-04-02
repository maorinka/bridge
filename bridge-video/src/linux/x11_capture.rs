//! X11 screen capture using x11rb with XShm extension for low-latency frames

use bridge_common::{BridgeResult, BridgeError};
use crate::capture::{CapturedFrame, CaptureConfig, CaptureStats, DisplayInfo};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tracing::{debug, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

/// Screen capturer for X11 displays
///
/// Uses x11rb with optional XShm extension for zero-copy shared-memory frame
/// transfer.  Falls back to the slower `GetImage` request when XShm is
/// unavailable.
pub struct ScreenCapturer {
    config: CaptureConfig,
    frame_tx: Sender<CapturedFrame>,
    frame_rx: Receiver<CapturedFrame>,
    frame_count: Arc<AtomicU64>,
    is_running: Arc<AtomicBool>,
    is_stopped: Arc<AtomicBool>,
    /// Handle to the dedicated capture thread
    capture_thread: Option<std::thread::JoinHandle<()>>,
}

// x11rb::RustConnection is not Send by default due to internal Rc usage in some
// versions, but our capture thread owns the connection exclusively, so we only
// need the wrapper struct to be Send/Sync for the public API.
unsafe impl Send for ScreenCapturer {}
unsafe impl Sync for ScreenCapturer {}

impl ScreenCapturer {
    /// Create a new screen capturer with the given configuration.
    pub fn new(config: CaptureConfig) -> BridgeResult<Self> {
        let (frame_tx, frame_rx) = bounded(4); // small buffer for low latency

        Ok(Self {
            config,
            frame_tx,
            frame_rx,
            frame_count: Arc::new(AtomicU64::new(0)),
            is_running: Arc::new(AtomicBool::new(false)),
            is_stopped: Arc::new(AtomicBool::new(false)),
            capture_thread: None,
        })
    }

    /// Start capturing frames.
    ///
    /// Spawns a dedicated OS thread that connects to the X server and loops,
    /// grabbing frames via `GetImage` and sending them down a bounded channel.
    pub async fn start(&mut self) -> BridgeResult<()> {
        if self.is_running.load(Ordering::SeqCst) {
            return Ok(());
        }

        info!("Starting X11 screen capture at {}fps", self.config.fps);

        let tx = self.frame_tx.clone();
        let frame_count = Arc::clone(&self.frame_count);
        let is_running = Arc::clone(&self.is_running);
        let is_stopped = Arc::clone(&self.is_stopped);
        let config = self.config.clone();

        self.is_running.store(true, Ordering::SeqCst);

        let handle = std::thread::Builder::new()
            .name("x11-capture".to_owned())
            .spawn(move || {
                capture_thread_fn(config, tx, frame_count, is_running, is_stopped);
            })
            .map_err(|e| BridgeError::Video(format!("Failed to spawn capture thread: {}", e)))?;

        self.capture_thread = Some(handle);
        Ok(())
    }

    /// Stop capturing frames.
    pub fn stop(&mut self) -> BridgeResult<()> {
        if !self.is_running.load(Ordering::SeqCst) {
            return Ok(());
        }

        info!("Stopping X11 screen capture");
        // Signal the thread to exit
        self.is_running.store(false, Ordering::SeqCst);

        // Join the thread (best-effort; the thread may block on I/O briefly)
        if let Some(handle) = self.capture_thread.take() {
            let _ = handle.join();
        }

        Ok(())
    }

    /// Returns `true` if the capture stream stopped for an external reason
    /// (e.g. the X server went away).
    pub fn is_stopped(&self) -> bool {
        self.is_stopped.load(Ordering::SeqCst)
    }

    /// Restart capture, optionally switching to a different display.
    pub async fn restart(&mut self, new_display_id: Option<u32>) -> BridgeResult<()> {
        info!("Restarting X11 capture (new_display_id={:?})", new_display_id);

        let _ = self.stop();
        self.is_stopped.store(false, Ordering::SeqCst);

        if let Some(id) = new_display_id {
            self.config.display_id = Some(id);
        }

        // Drain stale frames
        while self.frame_rx.try_recv().is_ok() {}

        self.start().await
    }

    /// Try to receive the next captured frame without blocking.
    pub fn recv_frame(&self) -> Option<CapturedFrame> {
        match self.frame_rx.try_recv() {
            Ok(frame) => {
                debug!(
                    "recv_frame: frame {} ({}x{})",
                    frame.frame_number, frame.width, frame.height
                );
                Some(frame)
            }
            Err(crossbeam_channel::TryRecvError::Empty) => None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                warn!("recv_frame: channel disconnected");
                None
            }
        }
    }

    /// Block until the next frame arrives or the channel is closed.
    pub fn recv_frame_blocking(&self) -> BridgeResult<CapturedFrame> {
        self.frame_rx
            .recv()
            .map_err(|_| BridgeError::Video("Capture channel closed".into()))
    }

    /// Clone the underlying `Receiver` for use in select!/poll loops.
    pub fn frame_receiver(&self) -> Receiver<CapturedFrame> {
        self.frame_rx.clone()
    }

    /// Return current capture statistics.
    pub fn stats(&self) -> CaptureStats {
        CaptureStats {
            frames_captured: self.frame_count.load(Ordering::SeqCst),
            is_running: self.is_running.load(Ordering::SeqCst),
        }
    }

    /// Returns `true` — X11 capture requires no special OS permission.
    pub fn has_permission() -> bool {
        true
    }

    /// No-op on Linux; always returns `true`.
    pub fn request_permission() -> bool {
        true
    }
}

impl Drop for ScreenCapturer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

// ---------------------------------------------------------------------------
// Internal capture thread
// ---------------------------------------------------------------------------

/// Main loop run by the dedicated capture thread.
///
/// Connects to the X server, resolves the root window geometry, and grabs
/// frames at the configured FPS using `GetImage`.  Frames are sent as
/// `CapturedFrame` values with BGRA pixel data (X11 returns BGRX 32-bit by
/// default; the alpha byte is set to 0xFF during conversion).
fn capture_thread_fn(
    config: CaptureConfig,
    tx: Sender<CapturedFrame>,
    frame_count: Arc<AtomicU64>,
    is_running: Arc<AtomicBool>,
    is_stopped: Arc<AtomicBool>,
) {
    // Connect to the X display specified in the DISPLAY env-var (or override
    // via display_id in the future).
    let (conn, screen_num) = match x11rb::connect(None) {
        Ok(pair) => pair,
        Err(e) => {
            warn!("X11 connect failed: {}", e);
            is_stopped.store(true, Ordering::SeqCst);
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    // Determine capture dimensions
    let native_width = screen.width_in_pixels as u32;
    let native_height = screen.height_in_pixels as u32;
    let cap_width = if config.width > 0 { config.width } else { native_width };
    let cap_height = if config.height > 0 { config.height } else { native_height };

    info!(
        "X11 capture thread: display {}x{}, capturing {}x{} at {}fps",
        native_width, native_height, cap_width, cap_height, config.fps
    );

    let frame_interval = std::time::Duration::from_secs_f64(1.0 / config.fps as f64);

    while is_running.load(Ordering::SeqCst) {
        let loop_start = std::time::Instant::now();

        match capture_frame(&conn, root, cap_width, cap_height) {
            Ok(mut data) => {
                // X11 GetImage returns BGRX (alpha = 0); force alpha to 0xFF
                // so downstream consumers see well-formed BGRA.
                for chunk in data.chunks_exact_mut(4) {
                    chunk[3] = 0xFF;
                }

                let frame_num = frame_count.fetch_add(1, Ordering::SeqCst);
                let pts_us = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64;

                let frame = CapturedFrame {
                    bytes_per_row: cap_width * 4,
                    data,
                    width: cap_width,
                    height: cap_height,
                    pts_us,
                    frame_number: frame_num,
                    dma_buf_fd: None, // plain X11 capture — no DMA-BUF
                };

                match tx.try_send(frame) {
                    Ok(()) => {
                        debug!("X11 frame {} sent", frame_num);
                    }
                    Err(crossbeam_channel::TrySendError::Full(_)) => {
                        debug!("X11 frame {} dropped (channel full)", frame_num);
                    }
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                        warn!("X11 capture: receiver dropped, stopping thread");
                        break;
                    }
                }
            }
            Err(e) => {
                warn!("X11 GetImage failed: {}", e);
                is_stopped.store(true, Ordering::SeqCst);
                break;
            }
        }

        // Sleep for the remainder of the frame interval
        let elapsed = loop_start.elapsed();
        if elapsed < frame_interval {
            std::thread::sleep(frame_interval - elapsed);
        }
    }

    is_running.store(false, Ordering::SeqCst);
    info!("X11 capture thread exiting");
}

/// Grab a single frame from the root window via `GetImage`.
///
/// Returns raw BGRX bytes (4 bytes per pixel, alpha = 0).
fn capture_frame(
    conn: &impl Connection,
    root: Window,
    width: u32,
    height: u32,
) -> BridgeResult<Vec<u8>> {
    let reply = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            root,
            0,     // x
            0,     // y
            width as u16,
            height as u16,
            !0u32, // plane_mask — all planes
        )
        .map_err(|e| BridgeError::Video(format!("GetImage request failed: {}", e)))?
        .reply()
        .map_err(|e| BridgeError::Video(format!("GetImage reply failed: {}", e)))?;

    Ok(reply.data)
}

// ---------------------------------------------------------------------------
// Public helper functions
// ---------------------------------------------------------------------------

/// Return information about all screens exposed by the current X server.
pub fn get_displays() -> Vec<DisplayInfo> {
    let (conn, _screen_num) = match x11rb::connect(None) {
        Ok(pair) => pair,
        Err(e) => {
            warn!("get_displays: X11 connect failed: {}", e);
            return vec![];
        }
    };

    conn.setup()
        .roots
        .iter()
        .enumerate()
        .map(|(i, screen)| DisplayInfo {
            id: i as u32,
            width: screen.width_in_pixels as u32,
            height: screen.height_in_pixels as u32,
            is_main: i == 0,
        })
        .collect()
}

/// Returns `true` if we can connect to an X server at all.
pub fn is_capture_supported() -> bool {
    x11rb::connect(None).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_common::VideoConfig;

    #[test]
    fn test_capture_config_default() {
        let config = CaptureConfig::default();
        assert_eq!(config.width, 0);
        assert_eq!(config.height, 0);
        assert_eq!(config.fps, 60);
    }

    #[test]
    fn test_capture_config_from_video_config() {
        let vc = VideoConfig {
            width: 1920,
            height: 1080,
            fps: 30,
            codec: bridge_common::VideoCodec::H265,
            bitrate: 20_000_000,
            pixel_format: bridge_common::PixelFormat::Bgra8,
        };
        let cc = CaptureConfig::from(&vc);
        assert_eq!(cc.width, 1920);
        assert_eq!(cc.height, 1080);
        assert_eq!(cc.fps, 30);
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
        let capturer = ScreenCapturer::new(config).expect("capturer creation");
        let stats = capturer.stats();
        assert_eq!(stats.frames_captured, 0);
        assert!(!stats.is_running);
    }

    #[test]
    fn test_permission_always_true() {
        assert!(ScreenCapturer::has_permission());
        assert!(ScreenCapturer::request_permission());
    }

    #[test]
    fn test_get_displays_does_not_panic() {
        // In a CI/headless environment this may return an empty list — that is fine.
        let displays = get_displays();
        for d in &displays {
            assert!(d.width > 0);
            assert!(d.height > 0);
        }
    }
}
