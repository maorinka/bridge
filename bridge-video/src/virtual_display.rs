//! Virtual display creation for headless/KVM scenarios
//!
//! Uses CGVirtualDisplay API (private, macOS 14+) to create virtual displays
//! at any resolution. Based on Chromium's virtual_display_util_mac.mm which
//! documents the required descriptor fields for macOS 14+ (Sonoma).

use tracing::{info, warn};
use bridge_common::{BridgeResult, BridgeError};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, msg_send_id};
use objc2_foundation::{NSPoint, NSSize, NSString, NSArray};

// Chromium uses vendorID=505 — macOS 14+ rejects vendorID=0
const VENDOR_ID: u32 = 505;

// Default PPI for size calculation (standard Retina density)
const DEFAULT_PPI: f64 = 160.0;

/// A virtual display that can be used for headless rendering
pub struct VirtualDisplay {
    _display: Retained<AnyObject>,
    display_id: u32,
    width: u32,
    height: u32,
}

unsafe impl Send for VirtualDisplay {}
unsafe impl Sync for VirtualDisplay {}

impl VirtualDisplay {
    /// Create a new virtual display at the specified resolution.
    ///
    /// The descriptor must include identity fields (vendorID, serial),
    /// physical size, max pixel dimensions, and color primaries —
    /// macOS 14+ validates these and returns nil if they're missing.
    pub fn new(width: u32, height: u32, refresh_rate: u32) -> BridgeResult<Self> {
        info!("Creating virtual display: {}x{} @ {}Hz", width, height, refresh_rate);

        unsafe {
            let descriptor_class = class!(CGVirtualDisplayDescriptor);
            let descriptor: Retained<AnyObject> = msg_send_id![descriptor_class, new];

            // Display name
            let name = NSString::from_str("Bridge Virtual Display");
            let _: () = msg_send![&descriptor, setName: &*name];

            // Dispatch queue — use global high-priority queue (Chromium does this)
            extern "C" {
                fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut std::ffi::c_void;
            }
            const QOS_CLASS_USER_INTERACTIVE: isize = 0x21;
            let queue = dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE, 0);
            if queue.is_null() {
                return Err(BridgeError::Video("Failed to get dispatch queue".into()));
            }
            let _: () = msg_send![&descriptor, setQueue: queue];

            // Max pixel dimensions — required for macOS 14+
            let _: () = msg_send![&descriptor, setMaxPixelsWide: width as usize];
            let _: () = msg_send![&descriptor, setMaxPixelsHigh: height as usize];

            // Physical size in millimeters — derived from resolution / PPI
            // Without this, macOS may reject the display for exceeding pixel density limits
            let size_mm = NSSize {
                width: 25.4 * width as f64 / DEFAULT_PPI,
                height: 25.4 * height as f64 / DEFAULT_PPI,
            };
            let _: () = msg_send![&descriptor, setSizeInMillimeters: size_mm];

            // Identity fields — macOS 14+ requires non-zero vendorID
            let _: () = msg_send![&descriptor, setVendorID: VENDOR_ID];
            let _: () = msg_send![&descriptor, setProductID: 0u32];

            // Serial number — monotonic counter (matching Chromium's approach)
            use std::sync::atomic::{AtomicU32, Ordering};
            static SERIAL_COUNTER: AtomicU32 = AtomicU32::new(1);
            let serial = SERIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
            let _: () = msg_send![&descriptor, setSerialNum: serial];
            let _: () = msg_send![&descriptor, setSerialNumber: serial];

            // Color primaries and white point (sRGB, matching Chromium)
            let _: () = msg_send![&descriptor, setRedPrimary: NSPoint { x: 0.6797, y: 0.3203 }];
            let _: () = msg_send![&descriptor, setGreenPrimary: NSPoint { x: 0.2559, y: 0.6983 }];
            let _: () = msg_send![&descriptor, setBluePrimary: NSPoint { x: 0.1494, y: 0.0557 }];
            let _: () = msg_send![&descriptor, setWhitePoint: NSPoint { x: 0.3125, y: 0.3291 }];

            info!("Descriptor configured: {}x{}, vendorID={}, serial={}, size={:.0}x{:.0}mm",
                  width, height, VENDOR_ID, serial, size_mm.width, size_mm.height);

            // Create the virtual display from descriptor
            let display_class = class!(CGVirtualDisplay);
            let display: Option<Retained<AnyObject>> = msg_send_id![
                msg_send_id![display_class, alloc],
                initWithDescriptor: &*descriptor
            ];

            let display = display.ok_or_else(|| {
                BridgeError::Video(
                    "CGVirtualDisplay initWithDescriptor returned nil. \
                     This can happen if: (1) running from SSH instead of GUI session, \
                     (2) macOS rejected the descriptor fields, or \
                     (3) WindowServer is in degraded headless mode. \
                     Try running from a GUI terminal or as a LaunchAgent.".into()
                )
            })?;

            let display_id: u32 = msg_send![&display, displayID];
            info!("Virtual display created with ID={}", display_id);

            if display_id == 0 {
                return Err(BridgeError::Video(
                    "CGVirtualDisplay returned displayID=0 (invalid)".into()
                ));
            }

            // Apply settings with display mode
            let settings_class = class!(CGVirtualDisplaySettings);
            let settings: Retained<AnyObject> = msg_send_id![settings_class, new];

            let _: () = msg_send![&settings, setHiDPI: false];

            // Create display mode at native (1x) resolution
            let mode_class = class!(CGVirtualDisplayMode);
            let mode: Retained<AnyObject> = msg_send_id![
                msg_send_id![mode_class, alloc],
                initWithWidth: width as usize,
                height: height as usize,
                refreshRate: refresh_rate as f64
            ];

            let modes_array = NSArray::from_retained_slice(&[mode.clone()]);
            let _: () = msg_send![&settings, setModes: &*modes_array];

            let success: bool = msg_send![&display, applySettings: &*settings];
            if !success {
                warn!("applySettings returned false — display may not have correct resolution");
            } else {
                info!("applySettings succeeded for {}x{} @ {}Hz", width, height, refresh_rate);
            }

            info!("Virtual display ready: ID={}, {}x{} @ {}Hz", display_id, width, height, refresh_rate);

            // Wait for macOS to register the virtual display in the display list.
            // Poll CGDisplayPixelsWide to confirm it's available before proceeding.
            extern "C" {
                fn CGDisplayPixelsWide(display: u32) -> usize;
                fn CGDisplayPixelsHigh(display: u32) -> usize;
                fn CGDisplayShowCursor(display: u32) -> i32;
                fn CGWarpMouseCursorPosition(point: NSPoint) -> i32;
            }
            let mut ready = false;
            for attempt in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let pw = CGDisplayPixelsWide(display_id);
                let ph = CGDisplayPixelsHigh(display_id);
                if pw > 0 && ph > 0 {
                    info!("Virtual display confirmed in display list after {}ms: {}x{}",
                          (attempt + 1) * 100, pw, ph);
                    ready = true;
                    break;
                }
            }
            if !ready {
                warn!("Virtual display {} not yet visible in display list after 2s", display_id);
            }

            // Ensure cursor is visible on the virtual display.
            // On headless setups the cursor is often hidden by default.
            CGDisplayShowCursor(display_id);
            CGWarpMouseCursorPosition(NSPoint {
                x: width as f64 / 2.0,
                y: height as f64 / 2.0,
            });
            info!("Cursor shown and warped to center of virtual display");

            Ok(Self {
                _display: display,
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
    }
}

/// Check if virtual display creation is supported
pub fn is_virtual_display_supported() -> bool {
    let class_exists = objc2::runtime::AnyClass::get(c"CGVirtualDisplay").is_some();
    if !class_exists {
        warn!("CGVirtualDisplay not available — requires macOS 14+");
    }
    class_exists
}
