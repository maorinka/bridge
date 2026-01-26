//! Virtual display creation for headless/KVM scenarios
//!
//! Uses CGVirtualDisplay API to create virtual displays at any resolution.
//! This allows the server to render at the client's native resolution.

use std::ptr::NonNull;
use tracing::{info, warn};
use bridge_common::{BridgeResult, BridgeError};

use objc2::rc::{Retained, Id};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, msg_send_id, sel};
use objc2_foundation::{NSString, NSArray, NSObject};

/// A virtual display that can be used for headless rendering
pub struct VirtualDisplay {
    display: Retained<AnyObject>,
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
            // Step 1: Create descriptor with basic properties
            let descriptor_class = class!(CGVirtualDisplayDescriptor);
            let descriptor: Retained<AnyObject> = msg_send_id![descriptor_class, new];

            // Set display name
            let name = NSString::from_str("Bridge Virtual Display");
            let _: () = msg_send![&descriptor, setName: &*name];

            // Get main dispatch queue using dlsym to avoid linker issues
            let main_queue = {
                let handle = libc::dlsym(
                    libc::RTLD_DEFAULT,
                    c"_dispatch_main_q".as_ptr()
                );
                if handle.is_null() {
                    return Err(BridgeError::Video("Failed to get main dispatch queue".into()));
                }
                handle
            };
            let _: () = msg_send![&descriptor, setDispatchQueue: main_queue];

            // Step 2: Create the virtual display from descriptor FIRST
            // (before applying settings - this is the correct order)
            let display_class = class!(CGVirtualDisplay);
            let display: Option<Retained<AnyObject>> = msg_send_id![
                msg_send_id![display_class, alloc],
                initWithDescriptor: &*descriptor
            ];

            let display = display.ok_or_else(|| {
                BridgeError::Video("Failed to create CGVirtualDisplay".into())
            })?;

            // Get the display ID
            let display_id: u32 = msg_send![&display, displayID];
            info!("Virtual display created with ID={}", display_id);

            // Step 3: Create settings with the display mode
            let settings_class = class!(CGVirtualDisplaySettings);
            let settings: Retained<AnyObject> = msg_send_id![settings_class, new];

            // Set hiDPI mode
            let _: () = msg_send![&settings, setHiDPI: true];

            // Create CGVirtualDisplayMode
            let mode_class = class!(CGVirtualDisplayMode);
            let mode: Retained<AnyObject> = msg_send_id![
                msg_send_id![mode_class, alloc],
                initWithWidth: width as usize,
                height: height as usize,
                refreshRate: refresh_rate as f64
            ];

            // Create modes array and set on settings
            let modes_array = NSArray::from_retained_slice(&[mode.clone()]);
            let _: () = msg_send![&settings, setModes: &*modes_array];

            // Step 4: Apply settings to the DISPLAY object (not descriptor!)
            // This is the correct API: [display applySettings:settings]
            let success: bool = msg_send![&display, applySettings: &*settings];
            if !success {
                warn!("applySettings returned false, display may not have correct resolution");
            }

            info!("Virtual display configured: ID={}, {}x{} @ {}Hz", display_id, width, height, refresh_rate);

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
        info!("Releasing virtual display {}", self.display_id);
        // Retained will automatically release when dropped
    }
}

/// Check if virtual display creation is supported
pub fn is_virtual_display_supported() -> bool {
    // Check if CGVirtualDisplay class exists (macOS 14+)
    let class_exists = objc2::runtime::AnyClass::get(c"CGVirtualDisplay").is_some();
    if !class_exists {
        warn!("CGVirtualDisplay not available - requires macOS 14+");
    }
    class_exists
}
