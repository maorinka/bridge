//! Video encoding and decoding using VideoToolbox
//!
//! VideoToolbox provides hardware-accelerated video encoding and decoding
//! on macOS using the Apple Silicon media engine or Intel Quick Sync.

use bridge_common::{BridgeError, BridgeResult, VideoCodec, VideoConfig};
use bytes::Bytes;
use crossbeam_channel::{bounded, Receiver, Sender};
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::sys::*;
use crate::CapturedFrame;

/// Encoded video frame
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// Encoded data (H.264/H.265 NAL units)
    pub data: Bytes,
    /// Presentation timestamp in microseconds
    pub pts_us: u64,
    /// Decode timestamp in microseconds
    pub dts_us: u64,
    /// Whether this is a keyframe
    pub is_keyframe: bool,
    /// Frame number
    pub frame_number: u64,
}

/// Decoded video frame
#[derive(Debug)]
pub struct DecodedFrame {
    /// Pixel data (BGRA)
    pub data: Vec<u8>,
    /// Frame width
    pub width: u32,
    /// Frame height
    pub height: u32,
    /// Bytes per row
    pub bytes_per_row: u32,
    /// Presentation timestamp in microseconds
    pub pts_us: u64,
    /// IOSurface for zero-copy Metal rendering
    pub io_surface: Option<IOSurfaceRef>,
}

unsafe impl Send for DecodedFrame {}

/// Video encoder configuration
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    /// Video width
    pub width: u32,
    /// Video height
    pub height: u32,
    /// Target frame rate
    pub fps: u32,
    /// Target bitrate in bits per second
    pub bitrate: u32,
    /// Codec to use
    pub codec: VideoCodec,
    /// Keyframe interval in frames
    pub keyframe_interval: u32,
    /// Enable low-latency mode
    pub low_latency: bool,
    /// Enable real-time encoding
    pub realtime: bool,
    /// Maximum quality mode (for high-bandwidth connections like Thunderbolt)
    pub max_quality: bool,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate: 60_000_000, // 60 Mbps
            codec: VideoCodec::H265,
            keyframe_interval: 60, // 1 second at 60fps
            low_latency: true,
            realtime: true,
            max_quality: false,
        }
    }
}

impl From<&VideoConfig> for EncoderConfig {
    fn from(config: &VideoConfig) -> Self {
        Self {
            width: config.width,
            height: config.height,
            fps: config.fps,
            bitrate: config.bitrate,
            codec: config.codec,
            ..Default::default()
        }
    }
}

/// Context for encoder output callback
struct EncoderCallbackContext {
    frame_tx: Sender<EncodedFrame>,
    frame_count: Arc<AtomicU64>,
    drop_count: Arc<AtomicU64>,
    codec: VideoCodec,
}

/// Hardware video encoder using VideoToolbox
pub struct VideoEncoder {
    config: EncoderConfig,
    session: VTCompressionSessionRef,
    frame_rx: Receiver<EncodedFrame>,
    frame_count: Arc<AtomicU64>,
    _callback_ctx: Box<EncoderCallbackContext>,
}

unsafe impl Send for VideoEncoder {}

impl VideoEncoder {
    /// Create a new video encoder
    pub fn new(config: EncoderConfig) -> BridgeResult<Self> {
        info!(
            "Creating VideoToolbox encoder: {}x{} @ {}fps, {} codec, {} bps",
            config.width,
            config.height,
            config.fps,
            match config.codec {
                VideoCodec::H264 => "H.264",
                VideoCodec::H265 => "H.265",
                VideoCodec::Raw => "Raw",
            },
            config.bitrate
        );

        if config.codec == VideoCodec::Raw {
            return Err(BridgeError::Video("Raw codec doesn't need encoder".into()));
        }

        let (frame_tx, frame_rx) = bounded(8);
        let frame_count = Arc::new(AtomicU64::new(0));

        let codec_type = match config.codec {
            VideoCodec::H264 => K_CM_VIDEO_CODEC_TYPE_H264,
            VideoCodec::H265 => K_CM_VIDEO_CODEC_TYPE_HEVC,
            VideoCodec::Raw => unreachable!(),
        };

        let drop_count = Arc::new(AtomicU64::new(0));
        let callback_ctx = Box::new(EncoderCallbackContext {
            frame_tx,
            frame_count: frame_count.clone(),
            drop_count: drop_count.clone(),
            codec: config.codec,
        });

        let callback_ctx_ptr = &*callback_ctx as *const EncoderCallbackContext as *mut c_void;

        let mut session: VTCompressionSessionRef = ptr::null_mut();

        unsafe {
            let status = VTCompressionSessionCreate(
                kCFAllocatorDefault,
                config.width as i32,
                config.height as i32,
                codec_type,
                ptr::null(),
                ptr::null(),
                kCFAllocatorDefault,
                encoder_output_callback as *const c_void,
                callback_ctx_ptr,
                &mut session,
            );

            if status != NO_ERR {
                return Err(BridgeError::Video(format!(
                    "Failed to create encoder session: {}",
                    status
                )));
            }
        }

        info!("VideoToolbox encoder session created, configuring properties...");

        // Configure encoder properties for low-latency streaming
        let encoder = Self {
            config: config.clone(),
            session,
            frame_rx,
            frame_count,
            _callback_ctx: callback_ctx,
        };

        // Set all the low-latency properties
        unsafe {
            // Set bitrate
            let bitrate_num = cf_number_create_i64(config.bitrate as i64);
            let status = VTSessionSetProperty(
                encoder.session as *mut c_void,
                kVTCompressionPropertyKey_AverageBitRate,
                bitrate_num,
            );
            CFRelease(bitrate_num);
            if status != NO_ERR {
                warn!("Failed to set bitrate: {}", status);
            } else {
                info!("  Bitrate: {} Mbps", config.bitrate / 1_000_000);
            }

            // Set expected frame rate
            let fps_num = cf_number_create_i64(config.fps as i64);
            let status = VTSessionSetProperty(
                encoder.session as *mut c_void,
                kVTCompressionPropertyKey_ExpectedFrameRate,
                fps_num,
            );
            CFRelease(fps_num);
            if status != NO_ERR {
                warn!("Failed to set frame rate: {}", status);
            } else {
                info!("  Frame rate: {} fps", config.fps);
            }

            // Set max keyframe interval (every 2 seconds - balances recovery time vs compression efficiency)
            let keyframe_interval = (config.fps * 2) as i64;
            let keyframe_num = cf_number_create_i64(keyframe_interval);
            let status = VTSessionSetProperty(
                encoder.session as *mut c_void,
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                keyframe_num,
            );
            CFRelease(keyframe_num);
            if status != NO_ERR {
                warn!("Failed to set keyframe interval: {}", status);
            } else {
                info!("  Keyframe interval: {} frames", keyframe_interval);
            }

            // Disable B-frames (AllowFrameReordering = false) for lower latency
            let status = VTSessionSetProperty(
                encoder.session as *mut c_void,
                kVTCompressionPropertyKey_AllowFrameReordering,
                kCFBooleanFalse as CFTypeRef,
            );
            if status != NO_ERR {
                warn!("Failed to disable frame reordering: {}", status);
            } else {
                info!("  Frame reordering: disabled (no B-frames)");
            }

            // Set MaxFrameDelayCount to 0 for immediate output
            let delay_count = cf_number_create_i64(0);
            let status = VTSessionSetProperty(
                encoder.session as *mut c_void,
                kVTCompressionPropertyKey_MaxFrameDelayCount,
                delay_count,
            );
            CFRelease(delay_count);
            if status != NO_ERR {
                warn!("Failed to set max frame delay: {}", status);
            } else {
                info!("  Max frame delay: 0 (immediate output)");
            }

            // Enable real-time mode if configured
            if config.realtime {
                let status = VTSessionSetProperty(
                    encoder.session as *mut c_void,
                    kVTCompressionPropertyKey_RealTime,
                    kCFBooleanTrue as CFTypeRef,
                );
                if status != NO_ERR {
                    warn!("Failed to enable real-time mode: {}", status);
                } else {
                    info!("  Real-time mode: enabled");
                }
            }

            // Set profile level — use Main10 for max quality (10-bit color)
            let profile = match (config.codec, config.max_quality) {
                (VideoCodec::H265, true) => kVTProfileLevel_HEVC_Main10_AutoLevel,
                (VideoCodec::H265, false) => kVTProfileLevel_HEVC_Main_AutoLevel,
                (VideoCodec::H264, _) => kVTProfileLevel_H264_Main_AutoLevel,
                (VideoCodec::Raw, _) => ptr::null(),
            };
            if !profile.is_null() {
                let status = VTSessionSetProperty(
                    encoder.session as *mut c_void,
                    kVTCompressionPropertyKey_ProfileLevel,
                    profile as CFTypeRef,
                );
                if status != NO_ERR {
                    warn!("Failed to set profile level: {}", status);
                }
            }

            // For max quality mode: use DataRateLimits to allow high burst rates
            // Note: We do NOT set Quality=1.0 because it makes the encoder prioritize
            // quality over speed, adding significant latency. High bitrate alone gives
            // excellent quality without the latency penalty.
            if config.max_quality {
                // DataRateLimits: allow burst up to 4x average bitrate per second
                // Format: CFArray of [bytes_per_period: CFNumber, period_seconds: CFNumber]
                let burst_bytes = (config.bitrate as i64 * 4) / 8; // 4x bitrate in bytes
                let burst_num = cf_number_create_i64(burst_bytes);
                let period_num = cf_number_create_f64(1.0); // 1 second period
                if !burst_num.is_null() && !period_num.is_null() {
                    let values = [burst_num, period_num];
                    let limits_array = CFArrayCreate(
                        kCFAllocatorDefault,
                        values.as_ptr() as *const *const c_void,
                        2,
                        std::ptr::null(),
                    );
                    if !limits_array.is_null() {
                        let status = VTSessionSetProperty(
                            encoder.session as *mut c_void,
                            kVTCompressionPropertyKey_DataRateLimits,
                            limits_array as CFTypeRef,
                        );
                        CFRelease(limits_array as CFTypeRef);
                        if status != NO_ERR {
                            warn!("Failed to set data rate limits: {}", status);
                        } else {
                            info!("  Data rate limit: {} MB/s burst", burst_bytes / 1_000_000);
                        }
                    }
                    CFRelease(burst_num);
                    CFRelease(period_num);
                }
            }
        }

        info!("VideoToolbox encoder configured for {} streaming",
              if config.max_quality { "max-quality" } else { "low-latency" });

        Ok(encoder)
    }

    /// Encode a captured frame
    pub fn encode(&mut self, frame: &CapturedFrame) -> BridgeResult<()> {
        if self.session.is_null() {
            return Err(BridgeError::Video("Encoder not initialized".into()));
        }

        // Create CVPixelBuffer from frame data
        let mut pixel_buffer: CVPixelBufferRef = ptr::null_mut();

        unsafe {
            // Create a CVPixelBuffer with the frame dimensions
            let status = CVPixelBufferCreate(
                kCFAllocatorDefault,
                frame.width as usize,
                frame.height as usize,
                K_CV_PIXEL_FORMAT_TYPE_32_BGRA,
                ptr::null(),
                &mut pixel_buffer,
            );

            if status != NO_ERR || pixel_buffer.is_null() {
                return Err(BridgeError::Video(format!(
                    "Failed to create pixel buffer: {}",
                    status
                )));
            }

            // Lock and copy data into the pixel buffer
            let lock_status = CVPixelBufferLockBaseAddress(pixel_buffer, 0);
            if lock_status != NO_ERR {
                CVPixelBufferRelease(pixel_buffer);
                return Err(BridgeError::Video("Failed to lock pixel buffer".into()));
            }

            let dest_base = CVPixelBufferGetBaseAddress(pixel_buffer);
            let dest_bytes_per_row = CVPixelBufferGetBytesPerRow(pixel_buffer);

            if !dest_base.is_null() {
                // Copy row by row in case of different row strides
                let src_bytes_per_row = frame.bytes_per_row as usize;
                let copy_bytes_per_row = src_bytes_per_row.min(dest_bytes_per_row);

                for y in 0..frame.height as usize {
                    let src_offset = y * src_bytes_per_row;
                    let dest_offset = y * dest_bytes_per_row;

                    if src_offset + copy_bytes_per_row <= frame.data.len() {
                        ptr::copy_nonoverlapping(
                            frame.data.as_ptr().add(src_offset),
                            (dest_base as *mut u8).add(dest_offset),
                            copy_bytes_per_row,
                        );
                    }
                }
            }

            CVPixelBufferUnlockBaseAddress(pixel_buffer, 0);

            // Encode the frame
            let pts = CMTimeStruct::new(frame.pts_us as i64, 1_000_000);
            let duration = CMTimeStruct::new(1_000_000 / self.config.fps as i64, 1_000_000);

            let encode_status = VTCompressionSessionEncodeFrame(
                self.session,
                pixel_buffer,
                pts,
                duration,
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
            );

            // Release the pixel buffer (encoder will retain if needed)
            CVPixelBufferRelease(pixel_buffer);

            if encode_status != NO_ERR {
                return Err(BridgeError::Video(format!(
                    "Encode failed: {}",
                    encode_status
                )));
            }
        }

        Ok(())
    }

    /// Get the next encoded frame (non-blocking)
    pub fn recv_frame(&self) -> Option<EncodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    /// Get the next encoded frame (blocking)
    pub fn recv_frame_blocking(&self) -> BridgeResult<EncodedFrame> {
        self.frame_rx
            .recv()
            .map_err(|_| BridgeError::Video("Encoder channel closed".into()))
    }

    /// Get a receiver for encoded frames
    pub fn frame_receiver(&self) -> Receiver<EncodedFrame> {
        self.frame_rx.clone()
    }

    /// Get the number of frames encoded
    pub fn frame_count(&self) -> u64 {
        self.frame_count.load(Ordering::SeqCst)
    }

    /// Flush pending frames
    pub fn flush(&mut self) -> BridgeResult<()> {
        unsafe {
            let status = VTCompressionSessionCompleteFrames(
                self.session,
                CMTimeStruct::new(i64::MAX, 1),
            );

            if status != NO_ERR {
                return Err(BridgeError::Video(format!("Flush failed: {}", status)));
            }
        }
        Ok(())
    }

    /// Dynamically update the encoder bitrate
    ///
    /// This is used for adaptive bitrate control based on network conditions.
    pub fn set_bitrate(&mut self, bitrate: u32) -> BridgeResult<()> {
        if self.session.is_null() {
            return Err(BridgeError::Video("Encoder not initialized".into()));
        }

        unsafe {
            let bitrate_num = cf_number_create_i64(bitrate as i64);
            if bitrate_num.is_null() {
                return Err(BridgeError::Video("Failed to create bitrate CFNumber".into()));
            }

            let status = VTSessionSetProperty(
                self.session as *mut c_void,
                kVTCompressionPropertyKey_AverageBitRate,
                bitrate_num,
            );

            CFRelease(bitrate_num);

            if status != NO_ERR {
                return Err(BridgeError::Video(format!(
                    "Failed to set bitrate: {}",
                    status
                )));
            }
        }

        info!("Encoder bitrate updated to {} bps ({:.1} Mbps)",
            bitrate, bitrate as f64 / 1_000_000.0);
        self.config.bitrate = bitrate;
        Ok(())
    }

    /// Get current bitrate
    pub fn bitrate(&self) -> u32 {
        self.config.bitrate
    }

    /// Request a keyframe on next encode
    pub fn request_keyframe(&mut self) {
        // This is handled by setting frame properties during encode
        // For now, we just log - the actual implementation would require
        // tracking state and passing it to the next encode call
        debug!("Keyframe requested");
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        if !self.session.is_null() {
            unsafe {
                VTCompressionSessionInvalidate(self.session);
            }
        }
    }
}

/// Encoder output callback - called by VideoToolbox when a frame is encoded
extern "C" fn encoder_output_callback(
    output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: u32,
    sample_buffer: CMSampleBufferRef,
) {
    if status != NO_ERR || sample_buffer.is_null() {
        if status != NO_ERR {
            warn!("Encoder callback received error status: {}", status);
        }
        return;
    }

    let ctx = unsafe { &*(output_callback_ref_con as *const EncoderCallbackContext) };

    unsafe {
        // Get timestamp
        let pts = CMSampleBufferGetPresentationTimeStamp(sample_buffer);

        // Get encoded data
        let data_buffer = CMSampleBufferGetDataBuffer(sample_buffer);
        if data_buffer.is_null() {
            return;
        }

        let mut data_ptr: *mut u8 = ptr::null_mut();
        let mut length = 0usize;
        let mut total_length = 0usize;

        let result = CMBlockBufferGetDataPointer(
            data_buffer,
            0,
            &mut length,
            &mut total_length,
            &mut data_ptr,
        );

        if result != NO_ERR || data_ptr.is_null() || total_length == 0 {
            return;
        }

        let data = std::slice::from_raw_parts(data_ptr, total_length);
        let frame_num = ctx.frame_count.fetch_add(1, Ordering::SeqCst);

        // Check if keyframe - first frame is always keyframe
        // For subsequent frames, scan NAL units for IDR types
        let is_keyframe = if frame_num == 0 {
            true
        } else {
            // Scan all NAL units in AVCC format for IDR NAL types
            let mut found_keyframe = false;
            let mut offset = 0;
            while offset + 4 < data.len() {
                let nal_len = u32::from_be_bytes([
                    data[offset], data[offset + 1], data[offset + 2], data[offset + 3]
                ]) as usize;

                if nal_len == 0 || nal_len > data.len() - offset - 4 {
                    break;
                }

                if offset + 4 < data.len() {
                    let nal_type = match ctx.codec {
                        VideoCodec::H265 => (data[offset + 4] >> 1) & 0x3F,
                        VideoCodec::H264 => data[offset + 4] & 0x1F,
                        VideoCodec::Raw => 0,
                    };

                    let is_idr = match ctx.codec {
                        VideoCodec::H265 => nal_type >= 19 && nal_type <= 21, // IDR_W_RADL, IDR_N_LP, CRA
                        VideoCodec::H264 => nal_type == 5, // IDR
                        VideoCodec::Raw => false,
                    };

                    if is_idr {
                        found_keyframe = true;
                        debug!("Found IDR NAL type {} at offset {}", nal_type, offset);
                        break;
                    }
                }

                offset += 4 + nal_len;
            }
            found_keyframe
        };

        // For keyframes, prepend parameter sets from the format description
        let final_data = if is_keyframe {
            let format_desc = CMSampleBufferGetFormatDescription(sample_buffer);
            if !format_desc.is_null() {
                extract_and_prepend_parameter_sets(format_desc, data, ctx.codec)
            } else {
                data.to_vec()
            }
        } else {
            data.to_vec()
        };

        let frame = EncodedFrame {
            data: Bytes::from(final_data),
            pts_us: pts.microseconds(),
            dts_us: pts.microseconds(),
            is_keyframe,
            frame_number: frame_num,
        };

        if let Err(e) = ctx.frame_tx.try_send(frame) {
            let drops = ctx.drop_count.fetch_add(1, Ordering::Relaxed) + 1;
            if drops % 30 == 1 {
                warn!("Encoded frame dropped (total: {}): {}", drops, e);
            }
        }
    }
}

/// Extract parameter sets from format description and prepend to frame data
unsafe fn extract_and_prepend_parameter_sets(
    format_desc: CMFormatDescriptionRef,
    data: &[u8],
    codec: VideoCodec,
) -> Vec<u8> {
    let mut result = Vec::with_capacity(data.len() + 256);

    match codec {
        VideoCodec::H265 => {
            // For H.265, we need VPS, SPS, PPS (indices 0, 1, 2)
            for idx in 0..3 {
                let mut param_ptr: *const u8 = ptr::null();
                let mut param_size: usize = 0;
                let mut _count: usize = 0;
                let mut _nal_length: i32 = 0;

                let status = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format_desc,
                    idx,
                    &mut param_ptr,
                    &mut param_size,
                    &mut _count,
                    &mut _nal_length,
                );

                if status == NO_ERR && !param_ptr.is_null() && param_size > 0 {
                    let param_data = std::slice::from_raw_parts(param_ptr, param_size);
                    // Write 4-byte AVCC length prefix
                    result.extend_from_slice(&(param_size as u32).to_be_bytes());
                    result.extend_from_slice(param_data);
                    debug!("Prepended H.265 param set {} ({} bytes)", idx, param_size);
                }
            }
        }
        VideoCodec::H264 => {
            // For H.264, we need SPS, PPS (indices 0, 1)
            for idx in 0..2 {
                let mut param_ptr: *const u8 = ptr::null();
                let mut param_size: usize = 0;
                let mut _count: usize = 0;
                let mut _nal_length: i32 = 0;

                let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    format_desc,
                    idx,
                    &mut param_ptr,
                    &mut param_size,
                    &mut _count,
                    &mut _nal_length,
                );

                if status == NO_ERR && !param_ptr.is_null() && param_size > 0 {
                    let param_data = std::slice::from_raw_parts(param_ptr, param_size);
                    // Write 4-byte AVCC length prefix
                    result.extend_from_slice(&(param_size as u32).to_be_bytes());
                    result.extend_from_slice(param_data);
                    debug!("Prepended H.264 param set {} ({} bytes)", idx, param_size);
                }
            }
        }
        VideoCodec::Raw => {}
    }

    // Append original frame data
    result.extend_from_slice(data);
    result
}

/// Context for decoder output callback
struct DecoderCallbackContext {
    frame_tx: Sender<DecodedFrame>,
    frame_count: Arc<AtomicU64>,
}

/// Hardware video decoder using VideoToolbox
pub struct VideoDecoder {
    width: u32,
    height: u32,
    codec: VideoCodec,
    session: VTDecompressionSessionRef,
    format_desc: CMVideoFormatDescriptionRef,
    frame_rx: Receiver<DecodedFrame>,
    frame_count: Arc<AtomicU64>,
    _callback_ctx: Box<DecoderCallbackContext>,
    initialized: bool,
    /// Set to true when the decoder needs a keyframe to initialize or recover
    needs_keyframe: bool,
}

unsafe impl Send for VideoDecoder {}

impl VideoDecoder {
    /// Create a new video decoder
    pub fn new(width: u32, height: u32, codec: VideoCodec) -> BridgeResult<Self> {
        info!(
            "Creating VideoToolbox decoder: {}x{}, {} codec",
            width,
            height,
            match codec {
                VideoCodec::H264 => "H.264",
                VideoCodec::H265 => "H.265",
                VideoCodec::Raw => "Raw",
            }
        );

        if codec == VideoCodec::Raw {
            return Err(BridgeError::Video("Raw codec doesn't need decoder".into()));
        }

        let (frame_tx, frame_rx) = bounded(8);
        let frame_count = Arc::new(AtomicU64::new(0));

        let callback_ctx = Box::new(DecoderCallbackContext {
            frame_tx,
            frame_count: frame_count.clone(),
        });

        Ok(Self {
            width,
            height,
            codec,
            session: ptr::null_mut(),
            format_desc: ptr::null(),
            frame_rx,
            frame_count,
            _callback_ctx: callback_ctx,
            initialized: false,
            needs_keyframe: true,
        })
    }

    /// Initialize the decoder session from parameter sets extracted from the bitstream
    fn initialize_session(&mut self, sps: &[u8], pps: &[u8], vps: Option<&[u8]>) -> BridgeResult<()> {
        if self.initialized {
            return Ok(());
        }

        info!("Initializing decoder session with parameter sets");

        unsafe {
            // Create format description from parameter sets
            let format_desc = match self.codec {
                VideoCodec::H264 => {
                    let param_sets = [sps.as_ptr(), pps.as_ptr()];
                    let param_sizes = [sps.len(), pps.len()];

                    let mut desc: CMVideoFormatDescriptionRef = ptr::null();
                    let status = CMVideoFormatDescriptionCreateFromH264ParameterSets(
                        kCFAllocatorDefault,
                        2,
                        param_sets.as_ptr(),
                        param_sizes.as_ptr(),
                        4, // NAL unit length size
                        &mut desc,
                    );

                    if status != NO_ERR {
                        return Err(BridgeError::Video(format!(
                            "Failed to create H.264 format description: {}",
                            status
                        )));
                    }
                    desc
                }
                VideoCodec::H265 => {
                    let vps_data = vps.ok_or_else(|| {
                        BridgeError::Video("H.265 requires VPS".into())
                    })?;

                    let param_sets = [vps_data.as_ptr(), sps.as_ptr(), pps.as_ptr()];
                    let param_sizes = [vps_data.len(), sps.len(), pps.len()];

                    let mut desc: CMVideoFormatDescriptionRef = ptr::null();
                    let status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                        kCFAllocatorDefault,
                        3,
                        param_sets.as_ptr(),
                        param_sizes.as_ptr(),
                        4, // NAL unit length size
                        ptr::null(),
                        &mut desc,
                    );

                    if status != NO_ERR {
                        return Err(BridgeError::Video(format!(
                            "Failed to create H.265 format description: {}",
                            status
                        )));
                    }
                    desc
                }
                VideoCodec::Raw => unreachable!(),
            };

            self.format_desc = format_desc;

            // Create decompression session
            let callback_record = VTDecompressionOutputCallbackRecord {
                decompressionOutputCallback: decoder_output_callback as *const c_void,
                decompressionOutputRefCon: &*self._callback_ctx as *const DecoderCallbackContext
                    as *mut c_void,
            };

            // Create destination image buffer attributes to request BGRA output
            let dest_attrs = CFDictionaryCreateMutable(
                kCFAllocatorDefault,
                1,
                ptr::null(), // kCFTypeDictionaryKeyCallBacks
                ptr::null(), // kCFTypeDictionaryValueCallBacks
            );
            let pixel_format: i32 = K_CV_PIXEL_FORMAT_TYPE_32BGRA as i32;
            let pixel_format_num = CFNumberCreate(
                kCFAllocatorDefault,
                K_CF_NUMBER_SINT32_TYPE,
                &pixel_format as *const i32 as *const c_void,
            );
            CFDictionarySetValue(
                dest_attrs,
                kCVPixelBufferPixelFormatTypeKey as CFTypeRef,
                pixel_format_num as CFTypeRef,
            );
            debug!("Requesting BGRA output format from decoder");

            let mut session: VTDecompressionSessionRef = ptr::null_mut();
            let status = VTDecompressionSessionCreate(
                kCFAllocatorDefault,
                format_desc,
                ptr::null(),
                dest_attrs as CFDictionaryRef,
                &callback_record,
                &mut session,
            );

            // Clean up the dictionary (session retains what it needs)
            CFRelease(pixel_format_num as CFTypeRef);
            CFRelease(dest_attrs as CFTypeRef);

            if status != NO_ERR {
                return Err(BridgeError::Video(format!(
                    "Failed to create decoder session: {}",
                    status
                )));
            }

            self.session = session;
            self.initialized = true;

            info!("Decoder session initialized successfully");
        }

        Ok(())
    }

    /// Decode an encoded frame
    pub fn decode(&mut self, frame: &EncodedFrame) -> BridgeResult<()> {
        // If not initialized and this is a keyframe, try to extract parameter sets
        if !self.initialized && frame.is_keyframe {
            debug!("Received keyframe ({} bytes), attempting to extract parameter sets", frame.data.len());
            if let Some((sps, pps, vps)) = Self::extract_parameter_sets(&frame.data, self.codec) {
                debug!("Extracted SPS ({} bytes), PPS ({} bytes), VPS: {}",
                       sps.len(), pps.len(), vps.as_ref().map(|v| v.len()).unwrap_or(0));
                self.initialize_session(&sps, &pps, vps.as_deref())?;
            } else {
                debug!("Failed to extract parameter sets from keyframe");
            }
        }

        if !self.initialized || self.session.is_null() {
            // Signal that we need a keyframe so the client can request one
            self.needs_keyframe = true;
            warn!("Decoder not initialized, dropping frame {} (need keyframe)", frame.frame_number);
            return Ok(());
        }
        self.needs_keyframe = false;

        if self.format_desc.is_null() {
            warn!("Format description is NULL - decoder not properly initialized");
            return Ok(());
        }

        // For keyframes, strip parameter sets and only decode the slice data
        let decode_data = if frame.is_keyframe {
            Self::strip_parameter_sets(&frame.data, self.codec)
        } else {
            frame.data.to_vec()
        };

        if decode_data.is_empty() {
            debug!("No slice data to decode after stripping parameter sets");
            return Ok(());
        }

        // Log first bytes to verify format
        if decode_data.len() >= 8 {
            debug!("Decode data first 8 bytes: {:02x?}, total {} bytes",
                   &decode_data[..8], decode_data.len());
        }

        unsafe {
            // Create block buffer from encoded data
            let mut block_buffer: CMBlockBufferRef = ptr::null_mut();

            let data_len = decode_data.len();

            // Create an empty block buffer that will allocate and own its memory
            let status = CMBlockBufferCreateWithMemoryBlock(
                kCFAllocatorDefault,
                ptr::null_mut(),  // NULL = allocate new memory
                data_len,
                kCFAllocatorDefault,  // Block allocator for the memory
                ptr::null(),
                0,
                data_len,
                0,
                &mut block_buffer,
            );

            if status != NO_ERR || block_buffer.is_null() {
                return Err(BridgeError::Video(format!(
                    "Failed to create block buffer: {}",
                    status
                )));
            }

            // Copy data into the block buffer - the buffer owns this memory
            let copy_status = CMBlockBufferReplaceDataBytes(
                decode_data.as_ptr() as *const c_void,
                block_buffer,
                0,
                data_len,
            );

            if copy_status != NO_ERR {
                CFRelease(block_buffer as CFTypeRef);
                return Err(BridgeError::Video(format!(
                    "Failed to copy data to block buffer: {}",
                    copy_status
                )));
            }

            // Create sample buffer
            debug!("Creating sample buffer: format_desc={:?}, block_buffer={:?}, keyframe={}, data_len={}",
                   self.format_desc, block_buffer, frame.is_keyframe, data_len);

            // Create timing info for the sample
            let timing_info = CMSampleTimingInfo {
                duration: CMTimeStruct::new(1, 60), // 1/60th second
                presentation_time_stamp: CMTimeStruct::new(frame.pts_us as i64, 1_000_000),
                decode_time_stamp: CMTimeStruct::new(frame.pts_us as i64, 1_000_000),
            };

            let sample_size: usize = data_len;

            let mut sample_buffer: CMSampleBufferRef = ptr::null_mut();
            let status = CMSampleBufferCreate(
                kCFAllocatorDefault,
                block_buffer,
                true,
                ptr::null(),
                ptr::null_mut(),
                self.format_desc,
                1,  // num_samples (as isize)
                1,  // num_sample_timing_entries (as isize)
                &timing_info,
                1,  // num_sample_size_entries (as isize)
                &sample_size,
                &mut sample_buffer,
            );

            if status != NO_ERR || sample_buffer.is_null() {
                // Release block buffer on failure
                CFRelease(block_buffer as CFTypeRef);
                warn!("Failed to create sample buffer: {} (frame {}, {} bytes)",
                      status, frame.frame_number, data_len);
                // Don't fail completely, just skip this frame
                return Ok(());
            }

            // Decode the frame synchronously
            let decode_status = VTDecompressionSessionDecodeFrame(
                self.session,
                sample_buffer,
                0, // Sync decode - waits for completion
                ptr::null_mut(),
                ptr::null_mut(),
            );

            // Release sample buffer and block buffer after decode completes
            CFRelease(sample_buffer as CFTypeRef);
            CFRelease(block_buffer as CFTypeRef);

            if decode_status != NO_ERR {
                warn!("Decode failed with status: {} (frame {})", decode_status, frame.frame_number);
            }
        }

        Ok(())
    }

    /// Extract parameter sets (SPS, PPS, optionally VPS) from NAL units in the data
    ///
    /// Supports both AVCC format (4-byte length prefix) and Annex B format (start codes)
    fn extract_parameter_sets(data: &[u8], codec: VideoCodec) -> Option<(Vec<u8>, Vec<u8>, Option<Vec<u8>>)> {
        if data.len() < 8 {
            return None;
        }

        // Try to detect format and extract NAL units
        debug!("Attempting to parse NAL units from {} bytes", data.len());
        let nal_units = Self::parse_nal_units(data);
        if nal_units.is_empty() {
            debug!("No NAL units found in data (first 16 bytes: {:02x?})", &data[..data.len().min(16)]);
            return None;
        }
        debug!("Found {} NAL units", nal_units.len());

        match codec {
            VideoCodec::H264 => Self::extract_h264_params(&nal_units),
            VideoCodec::H265 => Self::extract_h265_params(&nal_units),
            VideoCodec::Raw => None,
        }
    }

    /// Strip parameter sets (VPS, SPS, PPS) from frame data, keeping only slice NAL units
    fn strip_parameter_sets(data: &[u8], codec: VideoCodec) -> Vec<u8> {
        let mut result = Vec::new();
        let mut offset = 0;

        while offset + 4 < data.len() {
            let length = u32::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;

            if length == 0 || length > data.len() - offset - 4 {
                break;
            }

            let nal_start = offset + 4;
            let nal_end = nal_start + length;

            if nal_end <= data.len() && length > 0 {
                let nal_data = &data[nal_start..nal_end];
                let is_param_set = match codec {
                    VideoCodec::H265 => {
                        let nal_type = (nal_data[0] >> 1) & 0x3F;
                        // VPS (32), SPS (33), PPS (34), SEI prefix (39), SEI suffix (40)
                        matches!(nal_type, 32 | 33 | 34 | 39 | 40)
                    }
                    VideoCodec::H264 => {
                        let nal_type = nal_data[0] & 0x1F;
                        // SPS (7), PPS (8), SEI (6)
                        matches!(nal_type, 6 | 7 | 8)
                    }
                    VideoCodec::Raw => false,
                };

                if !is_param_set {
                    // Keep this NAL unit - write length prefix and data
                    result.extend_from_slice(&data[offset..nal_end]);
                    debug!("Keeping NAL unit at offset {} (type for decode, {} bytes)", offset, length);
                } else {
                    debug!("Stripping parameter set NAL at offset {} ({} bytes)", offset, length);
                }
            }

            offset = nal_end;
        }

        debug!("Stripped parameter sets: {} bytes -> {} bytes", data.len(), result.len());
        result
    }

    /// Parse NAL units from data (supports AVCC 4-byte length prefix and Annex B start codes)
    fn parse_nal_units(data: &[u8]) -> Vec<&[u8]> {
        let mut units = Vec::new();

        // First, try AVCC format (4-byte big-endian length prefix)
        if Self::try_parse_avcc(data, &mut units) {
            return units;
        }

        // Fall back to Annex B format (start code prefix: 0x00 0x00 0x01 or 0x00 0x00 0x00 0x01)
        Self::parse_annex_b(data, &mut units);
        units
    }

    /// Try to parse AVCC format (4-byte length prefixed NAL units)
    fn try_parse_avcc<'a>(data: &'a [u8], units: &mut Vec<&'a [u8]>) -> bool {
        let mut offset = 0;
        let mut found_valid = false;

        while offset + 4 < data.len() {
            let length = u32::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;

            // Sanity check: length should be reasonable
            if length == 0 || length > data.len() - offset - 4 {
                if !found_valid {
                    // If we haven't found any valid units, this isn't AVCC format
                    units.clear();
                    return false;
                }
                break;
            }

            let nal_start = offset + 4;
            let nal_end = nal_start + length;

            if nal_end <= data.len() {
                units.push(&data[nal_start..nal_end]);
                found_valid = true;
            }

            offset = nal_end;
        }

        found_valid
    }

    /// Parse Annex B format (start code delimited NAL units)
    fn parse_annex_b<'a>(data: &'a [u8], units: &mut Vec<&'a [u8]>) {
        let mut i = 0;
        let mut nal_start = None;

        while i < data.len() {
            // Look for start code (0x00 0x00 0x01 or 0x00 0x00 0x00 0x01)
            let is_start_code = if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 {
                if data[i + 2] == 1 {
                    Some(3) // 3-byte start code
                } else if i + 3 < data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                    Some(4) // 4-byte start code
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(start_code_len) = is_start_code {
                // Found a start code
                if let Some(start) = nal_start {
                    // End of previous NAL unit (excluding trailing zeros)
                    let mut end = i;
                    while end > start && data[end - 1] == 0 {
                        end -= 1;
                    }
                    if end > start {
                        units.push(&data[start..end]);
                    }
                }
                nal_start = Some(i + start_code_len);
                i += start_code_len;
            } else {
                i += 1;
            }
        }

        // Don't forget the last NAL unit
        if let Some(start) = nal_start {
            if start < data.len() {
                units.push(&data[start..]);
            }
        }
    }

    /// Extract H.264 parameter sets (SPS and PPS)
    fn extract_h264_params(nal_units: &[&[u8]]) -> Option<(Vec<u8>, Vec<u8>, Option<Vec<u8>>)> {
        let mut sps: Option<Vec<u8>> = None;
        let mut pps: Option<Vec<u8>> = None;

        for nal in nal_units {
            if nal.is_empty() {
                continue;
            }

            // H.264 NAL unit type is in bits 0-4 of the first byte
            let nal_type = nal[0] & 0x1F;

            match nal_type {
                7 => {
                    // SPS (Sequence Parameter Set)
                    debug!("Found H.264 SPS ({} bytes)", nal.len());
                    sps = Some(nal.to_vec());
                }
                8 => {
                    // PPS (Picture Parameter Set)
                    debug!("Found H.264 PPS ({} bytes)", nal.len());
                    pps = Some(nal.to_vec());
                }
                _ => {}
            }

            // Stop once we have both
            if sps.is_some() && pps.is_some() {
                break;
            }
        }

        match (sps, pps) {
            (Some(s), Some(p)) => {
                debug!("Extracted H.264 params: SPS={} bytes, PPS={} bytes", s.len(), p.len());
                Some((s, p, None))
            }
            _ => {
                debug!("Could not find both SPS and PPS");
                None
            }
        }
    }

    /// Extract H.265/HEVC parameter sets (VPS, SPS, and PPS)
    fn extract_h265_params(nal_units: &[&[u8]]) -> Option<(Vec<u8>, Vec<u8>, Option<Vec<u8>>)> {
        let mut vps: Option<Vec<u8>> = None;
        let mut sps: Option<Vec<u8>> = None;
        let mut pps: Option<Vec<u8>> = None;

        for nal in nal_units {
            if nal.len() < 2 {
                continue;
            }

            // H.265 NAL unit type is in bits 1-6 of the first byte
            let nal_type = (nal[0] >> 1) & 0x3F;

            match nal_type {
                32 => {
                    // VPS (Video Parameter Set)
                    debug!("Found H.265 VPS ({} bytes)", nal.len());
                    vps = Some(nal.to_vec());
                }
                33 => {
                    // SPS (Sequence Parameter Set)
                    debug!("Found H.265 SPS ({} bytes)", nal.len());
                    sps = Some(nal.to_vec());
                }
                34 => {
                    // PPS (Picture Parameter Set)
                    debug!("Found H.265 PPS ({} bytes)", nal.len());
                    pps = Some(nal.to_vec());
                }
                _ => {
                    debug!("Found H.265 NAL type {} ({} bytes)", nal_type, nal.len());
                }
            }

            // Stop once we have all three
            if vps.is_some() && sps.is_some() && pps.is_some() {
                break;
            }
        }

        match (sps, pps, vps) {
            (Some(s), Some(p), Some(v)) => {
                debug!(
                    "Extracted H.265 params: VPS={} bytes, SPS={} bytes, PPS={} bytes",
                    v.len(), s.len(), p.len()
                );
                Some((s, p, Some(v)))
            }
            (Some(s), Some(p), None) => {
                // Some encoders don't include VPS in every keyframe
                debug!("Extracted H.265 params without VPS: SPS={} bytes, PPS={} bytes", s.len(), p.len());
                // We need VPS for H.265, so return None
                warn!("H.265 stream missing VPS, cannot initialize decoder");
                None
            }
            _ => {
                debug!("Could not find required H.265 parameter sets");
                None
            }
        }
    }

    /// Manually initialize with known parameter sets
    pub fn initialize_with_params(
        &mut self,
        sps: &[u8],
        pps: &[u8],
        vps: Option<&[u8]>,
    ) -> BridgeResult<()> {
        self.initialize_session(sps, pps, vps)
    }

    /// Get the next decoded frame (non-blocking)
    pub fn recv_frame(&self) -> Option<DecodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    /// Get the next decoded frame (blocking)
    pub fn recv_frame_blocking(&self) -> BridgeResult<DecodedFrame> {
        self.frame_rx
            .recv()
            .map_err(|_| BridgeError::Video("Decoder channel closed".into()))
    }

    /// Get a receiver for decoded frames
    pub fn frame_receiver(&self) -> Receiver<DecodedFrame> {
        self.frame_rx.clone()
    }

    /// Get the number of frames decoded
    pub fn frame_count(&self) -> u64 {
        self.frame_count.load(Ordering::SeqCst)
    }

    /// Check if decoder is initialized
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Check if the decoder needs a keyframe to initialize or recover
    pub fn needs_keyframe(&self) -> bool {
        self.needs_keyframe
    }

    /// Flush pending frames
    pub fn flush(&mut self) -> BridgeResult<()> {
        if !self.session.is_null() {
            unsafe {
                let status = VTDecompressionSessionWaitForAsynchronousFrames(self.session);
                if status != NO_ERR {
                    return Err(BridgeError::Video(format!("Flush failed: {}", status)));
                }
            }
        }
        Ok(())
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        if !self.session.is_null() {
            unsafe {
                VTDecompressionSessionInvalidate(self.session);
            }
        }
    }
}

/// Decoder output callback - called by VideoToolbox when a frame is decoded
extern "C" fn decoder_output_callback(
    decompression_output_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: u32,
    image_buffer: CVImageBufferRef,
    presentation_time_stamp: CMTimeStruct,
    _presentation_duration: CMTimeStruct,
) {
    if status != NO_ERR || image_buffer.is_null() {
        if status != NO_ERR {
            warn!("Decoder callback received error status: {}", status);
        }
        return;
    }

    let ctx = unsafe { &*(decompression_output_ref_con as *const DecoderCallbackContext) };

    unsafe {
        let width = CVPixelBufferGetWidth(image_buffer) as u32;
        let height = CVPixelBufferGetHeight(image_buffer) as u32;
        let bytes_per_row = CVPixelBufferGetBytesPerRow(image_buffer) as u32;

        // Lock pixel buffer for CPU access
        let lock_result = CVPixelBufferLockBaseAddress(image_buffer, 1); // Read-only
        if lock_result != NO_ERR {
            warn!("Failed to lock pixel buffer: {}", lock_result);
            return;
        }

        let base_address = CVPixelBufferGetBaseAddress(image_buffer);
        let data_len = (bytes_per_row * height) as usize;

        let data = if !base_address.is_null() && data_len > 0 {
            std::slice::from_raw_parts(base_address as *const u8, data_len).to_vec()
        } else {
            vec![]
        };

        CVPixelBufferUnlockBaseAddress(image_buffer, 1);

        // Get IOSurface for zero-copy Metal rendering
        let io_surface = CVPixelBufferGetIOSurface(image_buffer);

        let frame_num = ctx.frame_count.fetch_add(1, Ordering::SeqCst);

        // Check pixel format
        let pixel_format = CVPixelBufferGetPixelFormatType(image_buffer);
        let format_str = match pixel_format {
            0x42475241 => "BGRA",
            0x34323076 => "420v (NV12)",
            0x34323066 => "420f (NV12)",
            _ => "unknown",
        };

        // Log every 60 frames (once per second at 60fps)
        if frame_num % 60 == 0 {
            info!("Decoded frame {} ({}x{}, format={} 0x{:08X}, {} bytes)",
                  frame_num, width, height, format_str, pixel_format, data.len());
        }

        // For NV12 format, the data copy won't work correctly
        // Force disable IOSurface path for now and only use if format is BGRA
        let use_io_surface = !io_surface.is_null() && pixel_format == 0x42475241;

        let frame = DecodedFrame {
            data,
            width,
            height,
            bytes_per_row,
            pts_us: presentation_time_stamp.microseconds(),
            io_surface: if use_io_surface {
                Some(io_surface)
            } else {
                None
            },
        };

        if let Err(e) = ctx.frame_tx.try_send(frame) {
            warn!("Decoded frame dropped (consumer too slow): {}", e);
        } else {
            debug!("Decoded frame {} ({}x{})", frame_num, width, height);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_config_default() {
        let config = EncoderConfig::default();
        assert_eq!(config.width, 1920);
        assert_eq!(config.height, 1080);
        assert_eq!(config.fps, 60);
        assert_eq!(config.codec, VideoCodec::H265);
    }

    #[test]
    fn test_encoder_config_from_video_config() {
        let video_config = VideoConfig {
            width: 3840,
            height: 2160,
            fps: 30,
            codec: VideoCodec::H264,
            bitrate: 50_000_000,
            pixel_format: bridge_common::PixelFormat::Bgra8,
        };
        let encoder_config = EncoderConfig::from(&video_config);
        assert_eq!(encoder_config.width, 3840);
        assert_eq!(encoder_config.height, 2160);
        assert_eq!(encoder_config.fps, 30);
        assert_eq!(encoder_config.codec, VideoCodec::H264);
    }

    #[test]
    fn test_encoder_creation_h264() {
        let config = EncoderConfig {
            width: 1280,
            height: 720,
            fps: 30,
            codec: VideoCodec::H264,
            ..Default::default()
        };
        let result = VideoEncoder::new(config);
        assert!(result.is_ok(), "Failed to create H.264 encoder: {:?}", result.err());
    }

    #[test]
    fn test_encoder_creation_h265() {
        let config = EncoderConfig {
            width: 1280,
            height: 720,
            fps: 30,
            codec: VideoCodec::H265,
            ..Default::default()
        };
        let result = VideoEncoder::new(config);
        assert!(result.is_ok(), "Failed to create H.265 encoder: {:?}", result.err());
    }

    #[test]
    fn test_encoder_raw_codec_rejected() {
        let config = EncoderConfig {
            codec: VideoCodec::Raw,
            ..Default::default()
        };
        let result = VideoEncoder::new(config);
        assert!(result.is_err());
    }

    #[test]
    fn test_decoder_creation_h264() {
        let result = VideoDecoder::new(1280, 720, VideoCodec::H264);
        assert!(result.is_ok(), "Failed to create H.264 decoder: {:?}", result.err());
    }

    #[test]
    fn test_decoder_creation_h265() {
        let result = VideoDecoder::new(1280, 720, VideoCodec::H265);
        assert!(result.is_ok(), "Failed to create H.265 decoder: {:?}", result.err());
    }

    #[test]
    fn test_decoder_raw_codec_rejected() {
        let result = VideoDecoder::new(1280, 720, VideoCodec::Raw);
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_frame() {
        let config = EncoderConfig {
            width: 320,
            height: 240,
            fps: 30,
            codec: VideoCodec::H264,
            ..Default::default()
        };

        let mut encoder = VideoEncoder::new(config).expect("Failed to create encoder");

        // Create a test frame
        let frame = CapturedFrame {
            data: vec![128u8; 320 * 240 * 4], // Gray frame
            width: 320,
            height: 240,
            bytes_per_row: 320 * 4,
            pts_us: 0,
            frame_number: 0,
            io_surface: None,
        };

        let result = encoder.encode(&frame);
        assert!(result.is_ok(), "Encode failed: {:?}", result.err());

        // Flush to ensure output
        encoder.flush().expect("Flush failed");

        // Give encoder time to produce output
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Check if we got encoded output
        let encoded = encoder.recv_frame();
        // Note: Encoder may batch frames, so we might not get output immediately
        if let Some(frame) = encoded {
            assert!(!frame.data.is_empty(), "Encoded frame is empty");
            println!("Encoded frame: {} bytes, keyframe={}", frame.data.len(), frame.is_keyframe);
        }
    }

    #[test]
    fn test_encode_multiple_frames() {
        let config = EncoderConfig {
            width: 320,
            height: 240,
            fps: 30,
            codec: VideoCodec::H264,
            keyframe_interval: 10,
            ..Default::default()
        };

        let mut encoder = VideoEncoder::new(config).expect("Failed to create encoder");

        // Encode several frames
        for i in 0..5 {
            let frame = CapturedFrame {
                data: vec![(i * 50) as u8; 320 * 240 * 4],
                width: 320,
                height: 240,
                bytes_per_row: 320 * 4,
                pts_us: i as u64 * 33333, // ~30fps
                frame_number: i,
                io_surface: None,
            };

            encoder.encode(&frame).expect("Encode failed");
        }

        encoder.flush().expect("Flush failed");
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Count encoded frames
        let mut count = 0;
        while encoder.recv_frame().is_some() {
            count += 1;
        }

        println!("Encoded {} frames", count);
        // We should have at least some output
        assert!(count > 0 || encoder.frame_count() > 0, "No frames were encoded");
    }
}
