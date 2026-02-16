//! ScreenCaptureKit-based screen capture (macOS 12.3+)
//!
//! Modern replacement for CGDisplayStream with:
//! - Lower latency capture
//! - Better virtual display support
//! - Direct IOSurface output for zero-copy pipeline
//! - Content filters (capture specific display/window/app)

use screencapturekit::prelude::*;
use screencapturekit::cm::SCFrameStatus;
use screencapturekit::stream::configuration::SCCaptureResolutionType;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crossbeam_channel::Sender;
use tracing::{debug, info, warn};

use crate::capture::CapturedFrame;
// Use specific imports from sys to avoid name conflicts with screencapturekit's CMTime
use crate::sys::{
    CFTypeRef, CFRetain, IOSurfaceRef, IOSurfaceIncrementUseCount,
    CVPixelBufferGetIOSurface,
};

/// ScreenCaptureKit frame output handler
///
/// Receives frames from SCStream and converts them to CapturedFrame
/// with zero-copy IOSurface access for the encoder pipeline.
struct SckHandler {
    frame_tx: Sender<CapturedFrame>,
    frame_count: Arc<AtomicU64>,
    is_stopped: Arc<AtomicBool>,
}

impl SCStreamOutputTrait for SckHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if !matches!(of_type, SCStreamOutputType::Screen) {
            return;
        }

        // Check frame status — only process complete frames.
        // Note: frame_status() returns None when the crate's Swift bridge can't parse
        // the attachment. Treat None as Complete since the frame data is valid.
        match sample.frame_status() {
            Some(SCFrameStatus::Complete) | None => {}
            Some(SCFrameStatus::Stopped) => {
                warn!("ScreenCaptureKit stream stopped (display disconnected?)");
                self.is_stopped.store(true, Ordering::SeqCst);
                return;
            }
            Some(SCFrameStatus::Idle) => return,   // No new content
            Some(SCFrameStatus::Blank) => return,   // Display blanked
            Some(_) => return,
        }

        let pixel_buffer = match sample.image_buffer() {
            Some(pb) => pb,
            None => return,
        };

        let raw_pb = pixel_buffer.as_ptr();
        let width = pixel_buffer.width() as u32;
        let height = pixel_buffer.height() as u32;
        let bytes_per_row = pixel_buffer.bytes_per_row() as u32;

        // Get IOSurface from pixel buffer using our own FFI bindings
        // (avoids ownership issues with the crate's IOSurface wrapper)
        let io_surface = unsafe { CVPixelBufferGetIOSurface(raw_pb) };
        if io_surface.is_null() {
            warn!("SCK frame has no IOSurface, skipping");
            return;
        }

        // Retain the IOSurface for our zero-copy pipeline:
        // - CFRetain keeps the object alive after this callback returns
        // - IncrementUseCount prevents SCStream from recycling the buffer
        unsafe {
            CFRetain(io_surface as CFTypeRef);
            IOSurfaceIncrementUseCount(io_surface);
        }

        // Get presentation timestamp
        let pts = sample.presentation_timestamp();
        let pts_us = if pts.timescale > 0 {
            (pts.value as f64 / pts.timescale as f64 * 1_000_000.0) as u64
        } else {
            0
        };

        let frame_num = self.frame_count.fetch_add(1, Ordering::SeqCst);

        let frame = CapturedFrame {
            data: Vec::new(), // Zero-copy: data lives in IOSurface
            width,
            height,
            bytes_per_row,
            pts_us,
            frame_number: frame_num,
            io_surface: Some(io_surface),
        };

        // Send frame (non-blocking). If channel is full, frame drops immediately,
        // which triggers CapturedFrame::Drop → releases IOSurface.
        match self.frame_tx.try_send(frame) {
            Ok(()) => {
                debug!("SCK frame {} sent ({}x{}, zero-copy IOSurface)", frame_num, width, height);
            }
            Err(e) => {
                warn!("SCK frame {} dropped: {}", frame_num, e);
            }
        }
    }
}

/// ScreenCaptureKit capture backend
pub struct SckBackend {
    stream: SCStream,
}

// SCStream is managed by the crate and callbacks run on internal dispatch queues
unsafe impl Send for SckBackend {}

impl SckBackend {
    /// Try to create and start a ScreenCaptureKit capture.
    ///
    /// Returns the backend on success, or an error string if SCK is unavailable.
    pub fn start(
        config: &crate::capture::CaptureConfig,
        frame_tx: Sender<CapturedFrame>,
        frame_count: Arc<AtomicU64>,
        is_stopped: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        info!("Attempting ScreenCaptureKit capture");

        // Get shareable content (displays, windows, apps)
        let content = SCShareableContent::get()
            .map_err(|e| format!("SCShareableContent::get() failed: {}", e))?;

        let displays = content.displays();
        if displays.is_empty() {
            return Err("No displays found via ScreenCaptureKit".into());
        }

        // Find the target display
        let target_display = if let Some(target_id) = config.display_id {
            displays.iter()
                .find(|d| d.display_id() == target_id)
                .ok_or_else(|| format!("Display {} not found in SCK", target_id))?
        } else {
            &displays[0]
        };

        let display_width = target_display.width();
        let display_height = target_display.height();
        info!("SCK display: {}x{} (ID={})", display_width, display_height, target_display.display_id());

        // Determine capture resolution
        let capture_width = if config.width > 0 { config.width } else { display_width };
        let capture_height = if config.height > 0 { config.height } else { display_height };

        // Create content filter — capture entire display
        let filter = SCContentFilter::create()
            .with_display(target_display)
            .with_excluding_windows(&[])
            .build();

        // Create stream configuration
        let frame_interval = CMTime::new(1, config.fps as i32);
        let stream_config = SCStreamConfiguration::new()
            .with_width(capture_width)
            .with_height(capture_height)
            .with_pixel_format(PixelFormat::BGRA)
            .with_minimum_frame_interval(&frame_interval)
            .with_shows_cursor(config.show_cursor)
            .with_scales_to_fit(false)
            .with_capture_resolution_type(SCCaptureResolutionType::Best);

        // Create handler
        let handler = SckHandler {
            frame_tx,
            frame_count,
            is_stopped,
        };

        // Create and configure stream
        let mut stream = SCStream::new(&filter, &stream_config);
        stream.add_output_handler(handler, SCStreamOutputType::Screen);

        // Start capture
        stream.start_capture()
            .map_err(|e| format!("SCStream start_capture failed: {}", e))?;

        info!("ScreenCaptureKit capture started: {}x{} @ {}fps (BGRA, zero-copy IOSurface)",
              capture_width, capture_height, config.fps);

        Ok(Self { stream })
    }

    /// Stop the capture
    pub fn stop(&self) -> Result<(), String> {
        self.stream.stop_capture()
            .map_err(|e| format!("SCStream stop_capture failed: {}", e))?;
        info!("ScreenCaptureKit capture stopped");
        Ok(())
    }
}
