//! V4L2 memory-to-memory hardware encoder for Jetson Nano
//!
//! This module provides a `VideoEncoder` that targets the Tegra V4L2 M2M
//! encoder device (`/dev/nvhost-msenc` or `/dev/video{0,1}`).
//!
//! # Current status
//!
//! The public API and the BGRA→NV12 conversion path are fully implemented.
//! The V4L2 ioctl call sequence is stubbed with `TODO` comments — these can
//! only be completed and tested on actual Jetson Nano hardware because the
//! V4L2 M2M driver is not present on other platforms.

use bridge_common::{BridgeResult, BridgeError, VideoCodec};
use crate::capture::CapturedFrame;
use crate::codec::{EncodedFrame, EncoderConfig};
use crate::linux::color_convert::bgra_to_nv12;
use bytes::Bytes;
use crossbeam_channel::{bounded, Receiver, Sender};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// V4L2 device paths to probe (in order)
// ---------------------------------------------------------------------------

const V4L2_DEVICE_CANDIDATES: &[&str] = &[
    "/dev/nvhost-msenc", // Tegra hardware encoder (Jetson Nano)
    "/dev/video0",
    "/dev/video1",
];

// ---------------------------------------------------------------------------
// Public struct
// ---------------------------------------------------------------------------

/// V4L2 M2M hardware video encoder.
///
/// Call `new` to open the device, then feed raw frames with `encode`.
/// Retrieve compressed NAL-unit frames with `recv_frame`.
pub struct VideoEncoder {
    config: EncoderConfig,
    /// File descriptor of the opened V4L2 device (–1 if not open)
    #[allow(dead_code)]
    device_fd: i32,
    /// Reusable NV12 scratch buffer
    nv12_buf: Vec<u8>,
    /// Encoded frame output channel
    encoded_tx: Sender<EncodedFrame>,
    encoded_rx: Receiver<EncodedFrame>,
    frame_count: Arc<AtomicU64>,
    request_keyframe: Arc<AtomicBool>,
}

impl VideoEncoder {
    /// Open the V4L2 encoder device and configure it for the given parameters.
    ///
    /// Probes `V4L2_DEVICE_CANDIDATES` in order and returns the first one that
    /// can be opened.  Returns an error if none are available.
    pub fn new(config: EncoderConfig) -> BridgeResult<Self> {
        let device_fd = open_v4l2_device()?;

        info!(
            "V4L2 encoder opened (fd={}), configuring {}x{} @ {}fps {:?}",
            device_fd, config.width, config.height, config.fps, config.codec
        );

        // TODO: VIDIOC_QUERYCAP — verify device supports V4L2_CAP_VIDEO_M2M
        // TODO: VIDIOC_S_FMT (OUTPUT queue) — set NV12 input format
        //         v4l2_format.type  = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE
        //         v4l2_pix_format_mplane.pixelformat = V4L2_PIX_FMT_NV12
        //         v4l2_pix_format_mplane.width  = config.width
        //         v4l2_pix_format_mplane.height = config.height
        // TODO: VIDIOC_S_FMT (CAPTURE queue) — set H.264/H.265 output format
        //         v4l2_format.type  = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE
        //         v4l2_pix_format_mplane.pixelformat = V4L2_PIX_FMT_H264 (or HEVC)
        // TODO: VIDIOC_S_PARM — set frame rate (v4l2_streamparm.parm.output.timeperframe)
        // TODO: set bitrate via V4L2_CID_MPEG_VIDEO_BITRATE control (VIDIOC_S_CTRL)
        // TODO: set keyframe interval via V4L2_CID_MPEG_VIDEO_H264_I_PERIOD
        // TODO: VIDIOC_REQBUFS (OUTPUT queue) — allocate input buffers (MMAP or USERPTR)
        // TODO: VIDIOC_REQBUFS (CAPTURE queue) — allocate output buffers
        // TODO: VIDIOC_QBUF  (CAPTURE queue) — enqueue all output buffers upfront
        // TODO: VIDIOC_STREAMON (OUTPUT queue)
        // TODO: VIDIOC_STREAMON (CAPTURE queue)

        let (encoded_tx, encoded_rx) = bounded(8);

        Ok(Self {
            config,
            device_fd,
            nv12_buf: Vec::new(),
            encoded_tx,
            encoded_rx,
            frame_count: Arc::new(AtomicU64::new(0)),
            request_keyframe: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Submit a raw BGRA frame for hardware encoding.
    ///
    /// The frame is converted to NV12 in software, then queued to the V4L2
    /// OUTPUT buffer.  Any newly available encoded frames on the CAPTURE queue
    /// are dequeued and forwarded to the internal channel.
    pub fn encode(&mut self, frame: &mut CapturedFrame) -> BridgeResult<()> {
        // 1. Convert BGRA → NV12
        bgra_to_nv12(&frame.data, frame.width, frame.height, &mut self.nv12_buf);

        let frame_num = self.frame_count.fetch_add(1, Ordering::SeqCst);
        let is_keyframe = self.request_keyframe.swap(false, Ordering::SeqCst);

        debug!(
            "V4L2 encode: frame {} ({}x{}, nv12={} bytes, keyframe={})",
            frame_num,
            frame.width,
            frame.height,
            self.nv12_buf.len(),
            is_keyframe
        );

        // TODO: if is_keyframe { VIDIOC_S_CTRL V4L2_CID_MPEG_VIDEO_FORCE_KEY_FRAME }
        // TODO: VIDIOC_DQBUF (OUTPUT queue) — get a free input buffer slot
        // TODO: memcpy self.nv12_buf into the mmap'd input buffer
        // TODO: VIDIOC_QBUF (OUTPUT queue) — submit the populated input buffer
        // TODO: poll()/select() on device_fd to wait for encoded output
        // TODO: VIDIOC_DQBUF (CAPTURE queue) — dequeue the encoded output buffer
        // TODO: copy encoded bytes out of the mmap'd output buffer
        // TODO: VIDIOC_QBUF (CAPTURE queue) — return output buffer to driver

        // Placeholder: forward a zero-length encoded frame so the channel stays
        // live during unit-test / non-Jetson builds.
        let encoded = EncodedFrame {
            data: Bytes::new(),
            pts_us: frame.pts_us,
            dts_us: frame.pts_us,
            is_keyframe,
            frame_number: frame_num,
        };

        match self.encoded_tx.try_send(encoded) {
            Ok(()) => {}
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                warn!("V4L2 encoded frame {} dropped (channel full)", frame_num);
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                return Err(BridgeError::Video("Encoder output channel closed".into()));
            }
        }

        Ok(())
    }

    /// Try to receive the next encoded frame without blocking.
    pub fn recv_frame(&self) -> Option<EncodedFrame> {
        self.encoded_rx.try_recv().ok()
    }

    /// Request that the next encoded frame be a keyframe (IDR).
    pub fn request_keyframe(&mut self) {
        self.request_keyframe.store(true, Ordering::SeqCst);
    }

    /// Update the target bitrate dynamically.
    pub fn set_bitrate(&mut self, bitrate: u32) -> BridgeResult<()> {
        info!("V4L2 set_bitrate: {} bps", bitrate);
        self.config.bitrate = bitrate;

        // TODO: VIDIOC_S_CTRL with V4L2_CID_MPEG_VIDEO_BITRATE = bitrate

        Ok(())
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        if self.device_fd >= 0 {
            // TODO: VIDIOC_STREAMOFF (OUTPUT queue)
            // TODO: VIDIOC_STREAMOFF (CAPTURE queue)
            // TODO: munmap all mmap'd buffers
            // TODO: VIDIOC_REQBUFS with count=0 to release kernel buffers
            unsafe {
                libc::close(self.device_fd);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Try to open one of the known V4L2 M2M encoder device paths.
fn open_v4l2_device() -> BridgeResult<i32> {
    for path in V4L2_DEVICE_CANDIDATES {
        let c_path = std::ffi::CString::new(*path)
            .map_err(|e| BridgeError::Video(format!("Invalid device path: {}", e)))?;

        let fd = unsafe {
            libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK)
        };

        if fd >= 0 {
            info!("Opened V4L2 device: {}", path);
            return Ok(fd);
        }
    }

    Err(BridgeError::Video(
        "No V4L2 M2M encoder device found. \
         Checked: /dev/nvhost-msenc, /dev/video0, /dev/video1. \
         Ensure you are running on Jetson Nano hardware with the \
         appropriate kernel driver loaded (nvhost-msenc or tegra-video)."
            .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The device open will fail in CI / on macOS, which is expected.
    #[test]
    fn test_encoder_new_fails_gracefully_without_device() {
        let config = EncoderConfig::default();
        let result = VideoEncoder::new(config);
        // On non-Jetson hardware this should return an error about missing device.
        match result {
            Err(BridgeError::Video(msg)) => {
                assert!(
                    msg.contains("No V4L2") || msg.contains("device"),
                    "Unexpected error message: {}",
                    msg
                );
            }
            Ok(_) => {
                // Somehow found a V4L2 device — that is fine too.
            }
            Err(e) => panic!("Unexpected error type: {:?}", e),
        }
    }
}
