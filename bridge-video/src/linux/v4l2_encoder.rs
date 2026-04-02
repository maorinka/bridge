//! V4L2 hardware encoder for Jetson Nano via NVIDIA Multimedia API
//!
//! Uses a C helper (nvenc_helper.c) that links against libnvv4l2.so —
//! NVIDIA's V4L2 wrapper for /dev/nvhost-msenc. The C helper handles
//! all V4L2 ioctl setup, buffer management, and streaming. This Rust
//! module provides the safe interface.

use bridge_common::{BridgeError, BridgeResult};
use crate::capture::CapturedFrame;
use crate::codec::{EncodedFrame, EncoderConfig};
use crate::linux::color_convert::bgra_to_nv12;
use bytes::Bytes;
use crossbeam_channel::{bounded, Receiver, Sender};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, info, warn};

// FFI declarations for nvenc_helper.c
extern "C" {
    fn nvenc_create(
        width: i32,
        height: i32,
        codec: i32,       // 0=H264, 1=H265
        bitrate: i32,
        fps: i32,
    ) -> *mut std::ffi::c_void;

    fn nvenc_encode_frame(
        handle: *mut std::ffi::c_void,
        nv12_data: *const u8,
        nv12_size: i32,
        out_data: *mut u8,
        out_max_size: i32,
        force_keyframe: i32,
    ) -> i32;

    fn nvenc_destroy(handle: *mut std::ffi::c_void);
}

const ENCODE_BUF_SIZE: usize = 4 * 1024 * 1024; // 4MB max encoded frame

/// V4L2 M2M hardware video encoder for Jetson Nano.
pub struct VideoEncoder {
    config: EncoderConfig,
    handle: *mut std::ffi::c_void,
    nv12_buf: Vec<u8>,
    encode_buf: Vec<u8>,
    encoded_tx: Sender<EncodedFrame>,
    encoded_rx: Receiver<EncodedFrame>,
    frame_count: Arc<AtomicU64>,
    request_keyframe: Arc<AtomicBool>,
}

unsafe impl Send for VideoEncoder {}

impl VideoEncoder {
    pub fn new(config: EncoderConfig) -> BridgeResult<Self> {
        let codec = match config.codec {
            bridge_common::VideoCodec::H264 => 0,
            bridge_common::VideoCodec::H265 => 1,
            bridge_common::VideoCodec::Raw => {
                return Err(BridgeError::Video("Raw codec doesn't need encoder".into()));
            }
        };

        info!(
            "Creating NVIDIA V4L2 encoder: {}x{} @ {}fps, {} bps, {}",
            config.width, config.height, config.fps, config.bitrate,
            if codec == 0 { "H.264" } else { "H.265" }
        );

        let handle = unsafe {
            nvenc_create(
                config.width as i32,
                config.height as i32,
                codec,
                config.bitrate as i32,
                config.fps as i32,
            )
        };

        if handle.is_null() {
            return Err(BridgeError::Video(
                "Failed to create NVIDIA V4L2 encoder. Check /dev/nvhost-msenc permissions \
                 and that libnvv4l2.so is installed."
                    .into(),
            ));
        }

        info!("NVIDIA V4L2 encoder created successfully");

        let (encoded_tx, encoded_rx) = bounded(64);

        Ok(Self {
            config,
            handle,
            nv12_buf: Vec::new(),
            encode_buf: vec![0u8; ENCODE_BUF_SIZE],
            encoded_tx,
            encoded_rx,
            frame_count: Arc::new(AtomicU64::new(0)),
            request_keyframe: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn encode(&mut self, frame: &mut CapturedFrame) -> BridgeResult<()> {
        // Convert BGRA → NV12
        bgra_to_nv12(&frame.data, frame.width, frame.height, &mut self.nv12_buf);

        let frame_num = self.frame_count.fetch_add(1, Ordering::SeqCst);
        let force_kf = self.request_keyframe.swap(false, Ordering::SeqCst);

        let result = unsafe {
            nvenc_encode_frame(
                self.handle,
                self.nv12_buf.as_ptr(),
                self.nv12_buf.len() as i32,
                self.encode_buf.as_mut_ptr(),
                ENCODE_BUF_SIZE as i32,
                if force_kf { 1 } else { 0 },
            )
        };

        if result < 0 {
            warn!("V4L2 encode error on frame {}", frame_num);
            return Err(BridgeError::Video("V4L2 encode failed".into()));
        }

        if result > 0 {
            let encoded_data = Bytes::copy_from_slice(&self.encode_buf[..result as usize]);

            // Check if this is a keyframe by looking for IDR NAL unit
            let is_keyframe = is_idr_frame(&encoded_data);

            debug!(
                "Encoded frame {}: {} bytes, keyframe={}",
                frame_num, result, is_keyframe
            );

            let encoded = EncodedFrame {
                data: encoded_data,
                pts_us: frame.pts_us,
                dts_us: frame.pts_us,
                is_keyframe,
                frame_number: frame_num,
            };

            match self.encoded_tx.try_send(encoded) {
                Ok(()) => {}
                Err(crossbeam_channel::TrySendError::Full(_)) => {
                    warn!("Encoded frame {} dropped (channel full)", frame_num);
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    return Err(BridgeError::Video("Encoder channel closed".into()));
                }
            }
        } else {
            debug!("Frame {} queued, no output yet (pipeline filling)", frame_num);
        }

        Ok(())
    }

    pub fn recv_frame(&self) -> Option<EncodedFrame> {
        self.encoded_rx.try_recv().ok()
    }

    pub fn request_keyframe(&mut self) {
        self.request_keyframe.store(true, Ordering::SeqCst);
    }

    pub fn set_bitrate(&mut self, bitrate: u32) -> BridgeResult<()> {
        info!("V4L2 set_bitrate: {} Mbps", bitrate / 1_000_000);
        self.config.bitrate = bitrate;
        // Dynamic bitrate change would need VIDIOC_S_EXT_CTRLS on the live session.
        // The C helper doesn't expose this yet — would need an nvenc_set_bitrate() function.
        Ok(())
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { nvenc_destroy(self.handle); }
            self.handle = std::ptr::null_mut();
        }
    }
}

/// Check if an H.264/H.265 bitstream starts with an IDR NAL unit
fn is_idr_frame(data: &[u8]) -> bool {
    if data.len() < 5 {
        return false;
    }
    // Look for start code (0x00000001) then check NAL type
    for i in 0..data.len().saturating_sub(5) {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 {
            let nal_type = data[i + 4] & 0x1F;
            // H.264 IDR = 5, H.265 IDR_W_RADL = 19, IDR_N_LP = 20
            if nal_type == 5 || nal_type == 19 || nal_type == 20 {
                return true;
            }
            // H.264 SPS = 7 (typically precedes IDR in keyframes)
            if nal_type == 7 {
                return true;
            }
        }
    }
    false
}
