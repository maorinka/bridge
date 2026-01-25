//! Virtual display creation for headless/KVM scenarios
//!
//! Uses CGVirtualDisplay API to create virtual displays at any resolution.
//! This allows the server to render at the client's native resolution.

use std::ffi::c_void;
use std::ptr;
use tracing::info;
use bridge_common::{BridgeResult, BridgeError};

// Objective-C runtime bindings
#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn objc_getClass(name: *const i8) -> *mut c_void;
    fn sel_registerName(name: *const i8) -> *mut c_void;
    fn objc_msgSend(obj: *mut c_void, sel: *mut c_void, ...) -> *mut c_void;
}

// CoreGraphics for CGVirtualDisplay
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {}

/// A virtual display that can be used for headless rendering
pub struct VirtualDisplay {
    display: *mut c_void,
    display_id: u32,
    width: u32,
    height: u32,
}

unsafe impl Send for VirtualDisplay {}
unsafe impl Sync for VirtualDisplay {}

impl VirtualDisplay {
    /// Create a new virtual display at the specified resolution
    pub fn new(width: u32, height: u32, refresh_rate: u32) -> BridgeResult<Self> {
        info!("Creating virtual display: {}x{} @ {}Hz", width, height, refresh_rate);

        unsafe {
            // Get CGVirtualDisplayDescriptor class
            let descriptor_class = objc_getClass(b"CGVirtualDisplayDescriptor\0".as_ptr() as *const i8);
            if descriptor_class.is_null() {
                return Err(BridgeError::Video(
                    "CGVirtualDisplayDescriptor not available - requires macOS 14+".into()
                ));
            }

            // Create descriptor: [[CGVirtualDisplayDescriptor alloc] init]
            let alloc_sel = sel_registerName(b"alloc\0".as_ptr() as *const i8);
            let init_sel = sel_registerName(b"init\0".as_ptr() as *const i8);

            let descriptor: *mut c_void = objc_msgSend(descriptor_class, alloc_sel);
            if descriptor.is_null() {
                return Err(BridgeError::Video("Failed to allocate CGVirtualDisplayDescriptor".into()));
            }

            let descriptor: *mut c_void = objc_msgSend(descriptor, init_sel);
            if descriptor.is_null() {
                return Err(BridgeError::Video("Failed to init CGVirtualDisplayDescriptor".into()));
            }

            // Set display name
            let set_name_sel = sel_registerName(b"setName:\0".as_ptr() as *const i8);
            let nsstring_class = objc_getClass(b"NSString\0".as_ptr() as *const i8);
            let string_with_utf8_sel = sel_registerName(b"stringWithUTF8String:\0".as_ptr() as *const i8);
            let name_str: *mut c_void = objc_msgSend(
                nsstring_class,
                string_with_utf8_sel,
                b"Bridge Virtual Display\0".as_ptr()
            );
            let _: *mut c_void = objc_msgSend(descriptor, set_name_sel, name_str);

            // Set queue for
            let set_queue_sel = sel_registerName(b"setDispatchQueue:\0".as_ptr() as *const i8);
            let dispatch_get_main_queue: extern "C" fn() -> *mut c_void;
            dispatch_get_main_queue = std::mem::transmute(
                libc::dlsym(libc::RTLD_DEFAULT, b"dispatch_get_main_queue\0".as_ptr() as *const i8)
            );
            let main_queue = dispatch_get_main_queue();
            let _: *mut c_void = objc_msgSend(descriptor, set_queue_sel, main_queue);

            // Create CGVirtualDisplaySettings
            let settings_class = objc_getClass(b"CGVirtualDisplaySettings\0".as_ptr() as *const i8);
            if settings_class.is_null() {
                return Err(BridgeError::Video("CGVirtualDisplaySettings not available".into()));
            }

            let settings: *mut c_void = objc_msgSend(settings_class, alloc_sel);
            let settings: *mut c_void = objc_msgSend(settings, init_sel);
            if settings.is_null() {
                return Err(BridgeError::Video("Failed to create CGVirtualDisplaySettings".into()));
            }

            // Set hiDPI mode (BOOL is i8 on arm64, but passed as i32 in variadic)
            let set_hidpi_sel = sel_registerName(b"setHiDPI:\0".as_ptr() as *const i8);
            let _: *mut c_void = objc_msgSend(settings, set_hidpi_sel, 1i32);

            // Create CGVirtualDisplayMode
            let mode_class = objc_getClass(b"CGVirtualDisplayMode\0".as_ptr() as *const i8);
            if mode_class.is_null() {
                return Err(BridgeError::Video("CGVirtualDisplayMode not available".into()));
            }

            // initWithWidth:height:refreshRate:
            let init_mode_sel = sel_registerName(b"initWithWidth:height:refreshRate:\0".as_ptr() as *const i8);
            let mode: *mut c_void = objc_msgSend(mode_class, alloc_sel);
            let mode: *mut c_void = objc_msgSend(
                mode,
                init_mode_sel,
                width as usize,
                height as usize,
                refresh_rate as f64
            );
            if mode.is_null() {
                return Err(BridgeError::Video("Failed to create CGVirtualDisplayMode".into()));
            }

            // Set modes array on settings
            let nsarray_class = objc_getClass(b"NSArray\0".as_ptr() as *const i8);
            let array_with_object_sel = sel_registerName(b"arrayWithObject:\0".as_ptr() as *const i8);
            let modes_array: *mut c_void = objc_msgSend(nsarray_class, array_with_object_sel, mode);

            let set_modes_sel = sel_registerName(b"setModes:\0".as_ptr() as *const i8);
            let _: *mut c_void = objc_msgSend(settings, set_modes_sel, modes_array);

            // Apply settings to descriptor
            let apply_settings_sel = sel_registerName(b"applySettings:\0".as_ptr() as *const i8);
            let _: *mut c_void = objc_msgSend(descriptor, apply_settings_sel, settings);

            // Set size on descriptor
            let set_size_sel = sel_registerName(b"setSizeInMillimeters:\0".as_ptr() as *const i8);
            // Approximate physical size based on resolution (assume ~110 PPI for a typical display)
            let width_mm = (width as f64 / 110.0) * 25.4;
            let height_mm = (height as f64 / 110.0) * 25.4;

            #[repr(C)]
            struct CGSize {
                width: f64,
                height: f64,
            }
            let size = CGSize { width: width_mm, height: height_mm };
            let _: *mut c_void = objc_msgSend(descriptor, set_size_sel, size);

            // Create the virtual display
            let display_class = objc_getClass(b"CGVirtualDisplay\0".as_ptr() as *const i8);
            if display_class.is_null() {
                return Err(BridgeError::Video("CGVirtualDisplay not available".into()));
            }

            let init_with_descriptor_sel = sel_registerName(b"initWithDescriptor:\0".as_ptr() as *const i8);
            let display: *mut c_void = objc_msgSend(display_class, alloc_sel);
            let display: *mut c_void = objc_msgSend(display, init_with_descriptor_sel, descriptor);

            if display.is_null() {
                return Err(BridgeError::Video("Failed to create CGVirtualDisplay".into()));
            }

            // Get the display ID (returns CGDirectDisplayID which is u32)
            let display_id_sel = sel_registerName(b"displayID\0".as_ptr() as *const i8);
            let display_id_ptr = objc_msgSend(display, display_id_sel);
            let display_id = display_id_ptr as usize as u32;

            info!("Virtual display created successfully: ID={}, {}x{}", display_id, width, height);

            Ok(Self {
                display,
                display_id,
                width,
                height,
            })
        }
    }

    /// Get the display ID for use with CGDisplayStream
    pub fn display_id(&self) -> u32 {
        self.display_id
    }

    /// Get the width
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Get the height
    pub fn height(&self) -> u32 {
        self.height
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        if !self.display.is_null() {
            info!("Releasing virtual display {}", self.display_id);
            unsafe {
                let release_sel = sel_registerName(b"release\0".as_ptr() as *const i8);
                let _: *mut c_void = objc_msgSend(self.display, release_sel);
            }
        }
    }
}

/// Check if virtual display creation is supported
pub fn is_virtual_display_supported() -> bool {
    unsafe {
        let class = objc_getClass(b"CGVirtualDisplay\0".as_ptr() as *const i8);
        !class.is_null()
    }
}
