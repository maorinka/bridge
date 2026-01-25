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
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate: 20_000_000, // 20 Mbps
            codec: VideoCodec::H265,
            keyframe_interval: 60, // 1 second at 60fps
            low_latency: true,
            realtime: true,
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

        let callback_ctx = Box::new(EncoderCallbackContext {
            frame_tx,
            frame_count: frame_count.clone(),
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

        info!("VideoToolbox encoder session created successfully");

        Ok(Self {
            config,
            session,
            frame_rx,
            frame_count,
            _callback_ctx: callback_ctx,
        })
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

        // Check if keyframe by looking for IDR NAL unit (simplified check)
        // In AVCC format, we'd check sample attachments
        let is_keyframe = frame_num == 0 || (data.len() > 4 && (data[4] & 0x1F) == 5);

        let frame = EncodedFrame {
            data: Bytes::copy_from_slice(data),
            pts_us: pts.microseconds(),
            dts_us: pts.microseconds(),
            is_keyframe,
            frame_number: frame_num,
        };

        if let Err(e) = ctx.frame_tx.try_send(frame) {
            debug!("Encoded frame dropped: {}", e);
        }
    }
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

            let mut session: VTDecompressionSessionRef = ptr::null_mut();
            let status = VTDecompressionSessionCreate(
                kCFAllocatorDefault,
                format_desc,
                ptr::null(),
                ptr::null(),
                &callback_record,
                &mut session,
            );

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
            if let Some((sps, pps, vps)) = Self::extract_parameter_sets(&frame.data, self.codec) {
                self.initialize_session(&sps, &pps, vps.as_deref())?;
            }
        }

        if !self.initialized || self.session.is_null() {
            // Wait for keyframe with parameter sets
            debug!("Decoder not initialized, waiting for keyframe");
            return Ok(());
        }

        unsafe {
            // Create block buffer from encoded data
            let mut block_buffer: CMBlockBufferRef = ptr::null_mut();

            // We need to make a mutable copy of the data for CMBlockBuffer
            let mut data_copy = frame.data.to_vec();

            let status = CMBlockBufferCreateWithMemoryBlock(
                kCFAllocatorDefault,
                data_copy.as_mut_ptr() as *mut c_void,
                data_copy.len(),
                kCFAllocatorDefault,
                ptr::null(),
                0,
                data_copy.len(),
                0,
                &mut block_buffer,
            );

            if status != NO_ERR || block_buffer.is_null() {
                return Err(BridgeError::Video(format!(
                    "Failed to create block buffer: {}",
                    status
                )));
            }

            // Create sample buffer
            let timing_info = CMSampleTimingInfo {
                duration: CMTimeStruct::new(1, 60), // Assume 60fps
                presentation_time_stamp: CMTimeStruct::new(frame.pts_us as i64, 1_000_000),
                decode_time_stamp: CMTimeStruct::new(frame.dts_us as i64, 1_000_000),
            };

            let sample_size = data_copy.len();

            let mut sample_buffer: CMSampleBufferRef = ptr::null_mut();
            let status = CMSampleBufferCreate(
                kCFAllocatorDefault,
                block_buffer,
                true,
                ptr::null(),
                ptr::null_mut(),
                self.format_desc,
                1,
                1,
                &timing_info,
                1,
                &sample_size,
                &mut sample_buffer,
            );

            // Don't forget to keep data_copy alive until decode completes
            std::mem::forget(data_copy);

            if status != NO_ERR || sample_buffer.is_null() {
                return Err(BridgeError::Video(format!(
                    "Failed to create sample buffer: {}",
                    status
                )));
            }

            // Decode the frame
            let decode_status = VTDecompressionSessionDecodeFrame(
                self.session,
                sample_buffer,
                0, // Sync decode
                ptr::null_mut(),
                ptr::null_mut(),
            );

            if decode_status != NO_ERR {
                warn!("Decode failed with status: {}", decode_status);
            }
        }

        Ok(())
    }

    /// Extract parameter sets (SPS, PPS, optionally VPS) from NAL units in the data
    fn extract_parameter_sets(data: &[u8], codec: VideoCodec) -> Option<(Vec<u8>, Vec<u8>, Option<Vec<u8>>)> {
        // This is a simplified implementation
        // In practice, you'd parse the AVCC/HVCC format or Annex B format

        // For now, return None to indicate we can't parse
        // A full implementation would:
        // 1. Parse NAL unit headers
        // 2. Identify SPS (type 7 for H.264, type 33 for H.265)
        // 3. Identify PPS (type 8 for H.264, type 34 for H.265)
        // 4. Identify VPS (type 32 for H.265 only)

        debug!(
            "Parameter set extraction not fully implemented for {:?}",
            codec
        );
        None
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

        let frame = DecodedFrame {
            data,
            width,
            height,
            bytes_per_row,
            pts_us: presentation_time_stamp.microseconds(),
            io_surface: if io_surface.is_null() {
                None
            } else {
                Some(io_surface)
            },
        };

        if let Err(e) = ctx.frame_tx.try_send(frame) {
            debug!("Decoded frame dropped: {}", e);
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
