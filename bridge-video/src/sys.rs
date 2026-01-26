//! Low-level system bindings for macOS video frameworks
//!
//! This module provides FFI bindings to:
//! - ScreenCaptureKit (SCStream, SCContentFilter, etc.)
//! - VideoToolbox (VTCompressionSession, VTDecompressionSession)
//! - IOSurface (for zero-copy frame access)
//! - CoreMedia (CMSampleBuffer, CMTime, etc.)

use std::ffi::c_void;
use std::ptr;

// Type aliases for Apple framework types
pub type CFTypeRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFMutableDictionaryRef = *mut c_void;
pub type CFStringRef = *const c_void;
pub type CFNumberRef = *const c_void;
pub type CFBooleanRef = *const c_void;
pub type CFArrayRef = *const c_void;

pub type OSStatus = i32;
pub type CGDirectDisplayID = u32;
pub type CGDisplayModeRef = *mut c_void;

// IOSurface types
pub type IOSurfaceRef = *mut c_void;

// CoreMedia types
pub type CMSampleBufferRef = *mut c_void;
pub type CMFormatDescriptionRef = *const c_void;
pub type CMVideoFormatDescriptionRef = *const c_void;
pub type CMBlockBufferRef = *mut c_void;
pub type CMTime = CMTimeStruct;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CMTimeStruct {
    pub value: i64,
    pub timescale: i32,
    pub flags: u32,
    pub epoch: i64,
}

impl CMTimeStruct {
    pub fn new(value: i64, timescale: i32) -> Self {
        Self {
            value,
            timescale,
            flags: 1, // kCMTimeFlags_Valid
            epoch: 0,
        }
    }

    pub fn invalid() -> Self {
        Self {
            value: 0,
            timescale: 0,
            flags: 0, // kCMTimeFlags_Invalid
            epoch: 0,
        }
    }

    pub fn seconds(&self) -> f64 {
        if self.timescale == 0 {
            return 0.0;
        }
        self.value as f64 / self.timescale as f64
    }

    pub fn microseconds(&self) -> u64 {
        (self.seconds() * 1_000_000.0) as u64
    }
}

// VideoToolbox types
pub type VTCompressionSessionRef = *mut c_void;
pub type VTDecompressionSessionRef = *mut c_void;

// ScreenCaptureKit types (opaque - accessed through Objective-C)
pub type SCStreamRef = *mut c_void;
pub type SCContentFilterRef = *mut c_void;
pub type SCStreamConfigurationRef = *mut c_void;
pub type SCShareableContentRef = *mut c_void;
pub type SCDisplayRef = *mut c_void;

// CoreVideo types
pub type CVPixelBufferRef = *mut c_void;
pub type CVImageBufferRef = CVPixelBufferRef;

// Pixel format constants
pub const K_CV_PIXEL_FORMAT_TYPE_32_BGRA: u32 = 0x42475241; // 'BGRA'
pub const K_CV_PIXEL_FORMAT_TYPE_420_YP_CB_CR_8_BI_PLANAR_VIDEO_RANGE: u32 = 0x34323076; // '420v'

// VideoToolbox constants
pub const K_VT_PROFILE_LEVEL_H264_HIGH_AUTO_LEVEL: CFStringRef = ptr::null();
pub const K_VT_H264_ENTROPY_MODE_CABAC: CFStringRef = ptr::null();

// VideoToolbox property keys (linked externally)
#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    pub static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    pub static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    pub static kVTCompressionPropertyKey_RealTime: CFStringRef;
    pub static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    pub static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    pub static kVTCompressionPropertyKey_MaxFrameDelayCount: CFStringRef;
    pub static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    pub static kVTProfileLevel_HEVC_Main_AutoLevel: CFStringRef;
    pub static kVTProfileLevel_H264_Main_AutoLevel: CFStringRef;
}

// OSStatus codes
pub const NO_ERR: OSStatus = 0;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFAllocatorDefault: CFAllocatorRef;
    pub static kCFBooleanTrue: CFBooleanRef;
    pub static kCFBooleanFalse: CFBooleanRef;

    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFRelease(cf: CFTypeRef);

    pub fn CFDictionaryCreateMutable(
        allocator: CFAllocatorRef,
        capacity: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFMutableDictionaryRef;

    pub fn CFDictionarySetValue(
        dict: CFMutableDictionaryRef,
        key: CFTypeRef,
        value: CFTypeRef,
    );

    pub fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: i32,
        value_ptr: *const c_void,
    ) -> CFNumberRef;
}

// CoreVideo pixel buffer keys
#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    pub static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
}

// Pixel format types
pub const K_CV_PIXEL_FORMAT_TYPE_32BGRA: u32 = 0x42475241; // 'BGRA'
pub const K_CF_NUMBER_SINT32_TYPE: i32 = 3; // kCFNumberSInt32Type

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    pub fn CMSampleBufferGetImageBuffer(sbuf: CMSampleBufferRef) -> CVImageBufferRef;
    pub fn CMSampleBufferGetPresentationTimeStamp(sbuf: CMSampleBufferRef) -> CMTimeStruct;
    pub fn CMSampleBufferGetFormatDescription(sbuf: CMSampleBufferRef) -> CMFormatDescriptionRef;
    pub fn CMVideoFormatDescriptionGetDimensions(
        video_desc: CMVideoFormatDescriptionRef,
    ) -> CMVideoDimensions;

    pub fn CMSampleBufferGetDataBuffer(sbuf: CMSampleBufferRef) -> CMBlockBufferRef;
    pub fn CMBlockBufferGetDataLength(buffer: CMBlockBufferRef) -> usize;
    pub fn CMBlockBufferGetDataPointer(
        buffer: CMBlockBufferRef,
        offset: usize,
        length_at_offset: *mut usize,
        total_length: *mut usize,
        data_pointer: *mut *mut u8,
    ) -> OSStatus;

    // Format description creation for H.264
    pub fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFAllocatorRef,
        parameter_set_count: usize,
        parameter_set_pointers: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        format_description_out: *mut CMVideoFormatDescriptionRef,
    ) -> OSStatus;

    // Format description creation for HEVC (H.265)
    pub fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocator: CFAllocatorRef,
        parameter_set_count: usize,
        parameter_set_pointers: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        extensions: CFDictionaryRef,
        format_description_out: *mut CMVideoFormatDescriptionRef,
    ) -> OSStatus;

    // Extract parameter sets from format descriptions
    pub fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        video_desc: CMVideoFormatDescriptionRef,
        parameter_set_index: usize,
        parameter_set_pointer_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut i32,
    ) -> OSStatus;

    pub fn CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
        video_desc: CMVideoFormatDescriptionRef,
        parameter_set_index: usize,
        parameter_set_pointer_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut i32,
    ) -> OSStatus;

    // Sample buffer creation for decoding
    // Note: CMItemCount is CFIndex which is signed long (isize on 64-bit)
    pub fn CMSampleBufferCreate(
        allocator: CFAllocatorRef,
        data_buffer: CMBlockBufferRef,
        data_ready: bool,
        make_data_ready_callback: *const c_void,
        make_data_ready_refcon: *mut c_void,
        format_description: CMFormatDescriptionRef,
        num_samples: isize,
        num_sample_timing_entries: isize,
        sample_timing_array: *const CMSampleTimingInfo,
        num_sample_size_entries: isize,
        sample_size_array: *const usize,
        sample_buffer_out: *mut CMSampleBufferRef,
    ) -> OSStatus;

    // Block buffer creation
    pub fn CMBlockBufferCreateWithMemoryBlock(
        allocator: CFAllocatorRef,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: CFAllocatorRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        block_buffer_out: *mut CMBlockBufferRef,
    ) -> OSStatus;

    pub fn CMBlockBufferCreateEmpty(
        allocator: CFAllocatorRef,
        sub_block_capacity: u32,
        flags: u32,
        block_buffer_out: *mut CMBlockBufferRef,
    ) -> OSStatus;

    pub fn CMBlockBufferAppendMemoryBlock(
        block_buffer: CMBlockBufferRef,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: CFAllocatorRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
    ) -> OSStatus;

    pub fn CMBlockBufferReplaceDataBytes(
        source_bytes: *const c_void,
        dest_buffer: CMBlockBufferRef,
        offset_into_dest: usize,
        data_length: usize,
    ) -> OSStatus;
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CMSampleTimingInfo {
    pub duration: CMTimeStruct,
    pub presentation_time_stamp: CMTimeStruct,
    pub decode_time_stamp: CMTimeStruct,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CMVideoDimensions {
    pub width: i32,
    pub height: i32,
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    pub fn CVPixelBufferGetWidth(pixel_buffer: CVPixelBufferRef) -> usize;
    pub fn CVPixelBufferGetHeight(pixel_buffer: CVPixelBufferRef) -> usize;
    pub fn CVPixelBufferGetBytesPerRow(pixel_buffer: CVPixelBufferRef) -> usize;
    pub fn CVPixelBufferGetPixelFormatType(pixel_buffer: CVPixelBufferRef) -> u32;
    pub fn CVPixelBufferGetIOSurface(pixel_buffer: CVPixelBufferRef) -> IOSurfaceRef;

    pub fn CVPixelBufferLockBaseAddress(pixel_buffer: CVPixelBufferRef, flags: u64) -> OSStatus;
    pub fn CVPixelBufferUnlockBaseAddress(pixel_buffer: CVPixelBufferRef, flags: u64) -> OSStatus;
    pub fn CVPixelBufferGetBaseAddress(pixel_buffer: CVPixelBufferRef) -> *mut c_void;
    pub fn CVPixelBufferGetBaseAddressOfPlane(
        pixel_buffer: CVPixelBufferRef,
        plane_index: usize,
    ) -> *mut c_void;

    pub fn CVPixelBufferCreate(
        allocator: CFAllocatorRef,
        width: usize,
        height: usize,
        pixel_format_type: u32,
        pixel_buffer_attributes: CFDictionaryRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> OSStatus;

    pub fn CVPixelBufferCreateWithBytes(
        allocator: CFAllocatorRef,
        width: usize,
        height: usize,
        pixel_format_type: u32,
        base_address: *mut c_void,
        bytes_per_row: usize,
        release_callback: *const c_void,
        release_ref_con: *mut c_void,
        pixel_buffer_attributes: CFDictionaryRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> OSStatus;

    pub fn CVPixelBufferRelease(pixel_buffer: CVPixelBufferRef);
    pub fn CVPixelBufferRetain(pixel_buffer: CVPixelBufferRef) -> CVPixelBufferRef;
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    pub fn VTCompressionSessionCreate(
        allocator: CFAllocatorRef,
        width: i32,
        height: i32,
        codec_type: u32,
        encoder_specification: CFDictionaryRef,
        source_image_buffer_attributes: CFDictionaryRef,
        compressed_data_allocator: CFAllocatorRef,
        output_callback: *const c_void,
        output_callback_ref_con: *mut c_void,
        compression_session_out: *mut VTCompressionSessionRef,
    ) -> OSStatus;

    pub fn VTCompressionSessionInvalidate(session: VTCompressionSessionRef);

    pub fn VTCompressionSessionEncodeFrame(
        session: VTCompressionSessionRef,
        image_buffer: CVImageBufferRef,
        presentation_timestamp: CMTimeStruct,
        duration: CMTimeStruct,
        frame_properties: CFDictionaryRef,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;

    pub fn VTCompressionSessionCompleteFrames(
        session: VTCompressionSessionRef,
        complete_until_presentation_timestamp: CMTimeStruct,
    ) -> OSStatus;

    pub fn VTSessionSetProperty(
        session: *mut c_void,
        property_key: CFStringRef,
        property_value: CFTypeRef,
    ) -> OSStatus;

    pub fn VTDecompressionSessionCreate(
        allocator: CFAllocatorRef,
        video_format_description: CMVideoFormatDescriptionRef,
        video_decoder_specification: CFDictionaryRef,
        destination_image_buffer_attributes: CFDictionaryRef,
        output_callback: *const VTDecompressionOutputCallbackRecord,
        decompression_session_out: *mut VTDecompressionSessionRef,
    ) -> OSStatus;

    pub fn VTDecompressionSessionInvalidate(session: VTDecompressionSessionRef);

    pub fn VTDecompressionSessionDecodeFrame(
        session: VTDecompressionSessionRef,
        sample_buffer: CMSampleBufferRef,
        decode_flags: u32,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;

    pub fn VTDecompressionSessionWaitForAsynchronousFrames(
        session: VTDecompressionSessionRef,
    ) -> OSStatus;
}

#[repr(C)]
pub struct VTDecompressionOutputCallbackRecord {
    pub decompressionOutputCallback: *const c_void,
    pub decompressionOutputRefCon: *mut c_void,
}

#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    pub fn IOSurfaceGetWidth(surface: IOSurfaceRef) -> usize;
    pub fn IOSurfaceGetHeight(surface: IOSurfaceRef) -> usize;
    pub fn IOSurfaceGetBytesPerRow(surface: IOSurfaceRef) -> usize;
    pub fn IOSurfaceGetPixelFormat(surface: IOSurfaceRef) -> u32;
    pub fn IOSurfaceLock(surface: IOSurfaceRef, options: u32, seed: *mut u32) -> OSStatus;
    pub fn IOSurfaceUnlock(surface: IOSurfaceRef, options: u32, seed: *mut u32) -> OSStatus;
    pub fn IOSurfaceGetBaseAddress(surface: IOSurfaceRef) -> *mut c_void;
}

// Codec type constants
pub const K_CM_VIDEO_CODEC_TYPE_H264: u32 = 0x61766331; // 'avc1'
pub const K_CM_VIDEO_CODEC_TYPE_HEVC: u32 = 0x68766331; // 'hvc1'

// VideoToolbox property keys (these would normally be loaded from the framework)
// For now we'll use dynamic lookup in the actual implementation

/// CFNumber type constants
pub const K_CF_NUMBER_SINT64_TYPE: i32 = 4; // kCFNumberSInt64Type

/// Create a CFNumber from an i64 value
pub fn cf_number_create_i64(value: i64) -> CFTypeRef {
    unsafe {
        CFNumberCreate(
            kCFAllocatorDefault,
            K_CF_NUMBER_SINT64_TYPE,
            &value as *const i64 as *const c_void,
        )
    }
}

/// Safe wrapper for CFRelease
pub fn cf_release(cf: CFTypeRef) {
    if !cf.is_null() {
        unsafe { CFRelease(cf) };
    }
}

/// Safe wrapper for CFRetain
pub fn cf_retain(cf: CFTypeRef) -> CFTypeRef {
    if cf.is_null() {
        cf
    } else {
        unsafe { CFRetain(cf) }
    }
}

// CGDisplayStream types and bindings
pub type CGDisplayStreamRef = *mut c_void;
pub type CGDisplayStreamUpdateRef = *mut c_void;
pub type DispatchQueueRef = *mut c_void;

/// CGDisplayStream frame status
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CGDisplayStreamFrameStatus {
    FrameComplete = 0,
    FrameIdle = 1,
    FrameBlank = 2,
    Stopped = 3,
}

/// CGDisplayStream update rect type
#[repr(i32)]
#[derive(Debug, Clone, Copy)]
pub enum CGDisplayStreamUpdateRectType {
    MovedRects = 0,
    DirtyRects = 1,
    ReducedDirtyRects = 2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGMainDisplayID() -> CGDirectDisplayID;
    pub fn CGDisplayPixelsWide(display: CGDirectDisplayID) -> usize;
    pub fn CGDisplayPixelsHigh(display: CGDirectDisplayID) -> usize;

    // Native resolution (actual pixels, not scaled)
    pub fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeGetPixelHeight(mode: CGDisplayModeRef) -> usize;
    pub fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> CGDisplayModeRef;
    pub fn CGDisplayModeRelease(mode: CGDisplayModeRef);
    pub fn CGDisplayIsMain(display: CGDirectDisplayID) -> bool;
    pub fn CGGetActiveDisplayList(
        max_displays: u32,
        active_displays: *mut CGDirectDisplayID,
        display_count: *mut u32,
    ) -> i32;

    // CGDisplayStream functions
    pub fn CGDisplayStreamCreateWithDispatchQueue(
        display: CGDirectDisplayID,
        output_width: usize,
        output_height: usize,
        pixel_format: i32,
        properties: CFDictionaryRef,
        queue: DispatchQueueRef,
        handler: *const c_void, // Block pointer
    ) -> CGDisplayStreamRef;

    pub fn CGDisplayStreamStart(stream: CGDisplayStreamRef) -> i32;
    pub fn CGDisplayStreamStop(stream: CGDisplayStreamRef) -> i32;

    pub fn CGDisplayStreamUpdateGetRects(
        update: CGDisplayStreamUpdateRef,
        rect_type: CGDisplayStreamUpdateRectType,
        rect_count: *mut usize,
    ) -> *const CGRect;
}

// CGDisplayStream property keys
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub static kCGDisplayStreamShowCursor: CFStringRef;
    pub static kCGDisplayStreamMinimumFrameTime: CFStringRef;
    pub static kCGDisplayStreamPreserveAspectRatio: CFStringRef;
    pub static kCGDisplayStreamQueueDepth: CFStringRef;
}

// Dispatch queue functions
#[link(name = "System")]
extern "C" {
    pub fn dispatch_queue_create(label: *const i8, attr: *const c_void) -> DispatchQueueRef;
    pub fn dispatch_release(object: *mut c_void);
}
