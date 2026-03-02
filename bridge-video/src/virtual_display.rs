//! Virtual display creation for headless/KVM scenarios
//!
//! Uses CGVirtualDisplay API (private, macOS 14+) to create virtual displays
//! at any resolution. Based on Chromium's virtual_display_util_mac.mm which
//! documents the required descriptor fields for macOS 14+ (Sonoma).

use tracing::{info, warn};
use bridge_common::{BridgeResult, BridgeError};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSSize, NSString, NSArray};

// Chromium uses vendorID=505 — macOS 14+ rejects vendorID=0
const VENDOR_ID: u32 = 505;

// PPI for physical size — matches a real 27" 4K Retina display (~163 PPI).
// This tells macOS the display is a realistic desktop monitor, not a TV.
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
            let descriptor: Retained<AnyObject> = msg_send![descriptor_class, new];

            // Display name
            let name = NSString::from_str("Bridge Virtual Display");
            let _: () = msg_send![&descriptor, setName: &*name];

            // Dispatch queue — use global high-priority queue (Chromium does this).
            // dispatch_queue_t is toll-free bridged to NSObject, so cast to *const AnyObject
            // for the objc2 msg_send! macro which expects ObjC types.
            extern "C" {
                fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *const AnyObject;
            }
            const QOS_CLASS_USER_INTERACTIVE: isize = 0x21;
            let queue = dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE, 0);
            if queue.is_null() {
                return Err(BridgeError::Video("Failed to get dispatch queue".into()));
            }
            let _: () = msg_send![&descriptor, setQueue: queue];

            // Max pixel dimensions — required for macOS 14+.
            // Set to exactly the target resolution (no HiDPI, 1x scaling).
            let max_wide = width;
            let max_high = height;
            let _: () = msg_send![&descriptor, setMaxPixelsWide: max_wide];
            let _: () = msg_send![&descriptor, setMaxPixelsHigh: max_high];

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

            info!("Descriptor configured: maxPixels={}x{}, vendorID={}, serial={}, size={:.0}x{:.0}mm",
                  max_wide, max_high, VENDOR_ID, serial, size_mm.width, size_mm.height);

            // Create the virtual display from descriptor
            let display_class = class!(CGVirtualDisplay);
            let display: Option<Retained<AnyObject>> = msg_send![
                msg_send![display_class, alloc],
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
            let settings: Retained<AnyObject> = msg_send![settings_class, new];

            // Disable HiDPI — use full resolution as the logical mode (1x scaling).
            // With HiDPI=1, macOS renders at half resolution (e.g. 1920x1080) and
            // upscales to 3840x2160 — text looks blurry in captures.
            // With HiDPI=0 and mode=3840x2160, macOS renders at native 4K pixels.
            // Text will be small (no 2x scaling) but capture is pixel-perfect.
            let _: () = msg_send![&settings, setHiDPI: 0u32];

            let mode_class = class!(CGVirtualDisplayMode);
            let mode: Retained<AnyObject> = msg_send![
                msg_send![mode_class, alloc],
                initWithWidth: width,
                height: height,
                refreshRate: refresh_rate as f64
            ];

            let modes_array = NSArray::from_retained_slice(&[mode.clone()]);
            let _: () = msg_send![&settings, setModes: &*modes_array];

            info!("Applying settings: HiDPI=0, mode={}x{} (native 1x scaling)",
                  width, height);

            let success: bool = msg_send![&display, applySettings: &*settings];
            if !success {
                warn!("applySettings returned false — display may not have correct resolution");
            } else {
                info!("applySettings succeeded for {}x{} @ {}Hz", width, height, refresh_rate);
            }

            // Wait for macOS to register the virtual display in the display list.
            extern "C" {
                fn CGDisplayPixelsWide(display: u32) -> usize;
                fn CGDisplayPixelsHigh(display: u32) -> usize;
                fn CGDisplayShowCursor(display: u32) -> i32;
                fn CGDisplayMoveCursorToPoint(display: u32, point: NSPoint) -> i32;
                fn CGDisplayCopyAllDisplayModes(
                    display: u32,
                    options: *const std::ffi::c_void,
                ) -> *const std::ffi::c_void;
                fn CGDisplayCopyDisplayMode(display: u32) -> *const std::ffi::c_void;
                fn CGDisplayModeGetWidth(mode: *const std::ffi::c_void) -> usize;
                fn CGDisplayModeGetHeight(mode: *const std::ffi::c_void) -> usize;
                fn CGDisplayModeGetPixelWidth(mode: *const std::ffi::c_void) -> usize;
                fn CGDisplayModeGetPixelHeight(mode: *const std::ffi::c_void) -> usize;
                fn CGDisplaySetDisplayMode(
                    display: u32,
                    mode: *const std::ffi::c_void,
                    options: *const std::ffi::c_void,
                ) -> i32;
                fn CFArrayGetCount(array: *const std::ffi::c_void) -> isize;
                fn CFArrayGetValueAtIndex(
                    array: *const std::ffi::c_void,
                    index: isize,
                ) -> *const std::ffi::c_void;
                fn CFRelease(cf: *const std::ffi::c_void);
            }

            let mut ready = false;
            for attempt in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let pw = CGDisplayPixelsWide(display_id);
                let ph = CGDisplayPixelsHigh(display_id);
                if pw > 0 && ph > 0 {
                    info!("Virtual display in display list after {}ms: {}x{} pixels",
                          (attempt + 1) * 100, pw, ph);
                    ready = true;
                    break;
                }
            }
            if !ready {
                warn!("Virtual display {} not yet visible after 2s", display_id);
            }

            // Log current mode — CGDisplayCopyDisplayMode may return null for virtual displays
            let cur_mode = CGDisplayCopyDisplayMode(display_id);
            if !cur_mode.is_null() {
                let lw = CGDisplayModeGetWidth(cur_mode);
                let lh = CGDisplayModeGetHeight(cur_mode);
                let pw = CGDisplayModeGetPixelWidth(cur_mode);
                let ph = CGDisplayModeGetPixelHeight(cur_mode);
                info!("Current display mode: {}x{} logical, {}x{} pixels", lw, lh, pw, ph);
                CFRelease(cur_mode);
            } else {
                info!("CGDisplayCopyDisplayMode returned null (virtual display)");
            }

            // Enumerate all available modes and try to select the one with the
            // most pixels (ideally width x height backing pixels).
            // Pass kCGDisplayShowDuplicateLowResolutionModes = true to see all modes.
            // Build the options CFDictionary using CoreFoundation C API.
            extern "C" {
                fn CFDictionaryCreate(
                    allocator: *const std::ffi::c_void,
                    keys: *const *const std::ffi::c_void,
                    values: *const *const std::ffi::c_void,
                    num_values: isize,
                    key_callbacks: *const std::ffi::c_void,
                    value_callbacks: *const std::ffi::c_void,
                ) -> *const std::ffi::c_void;
                static kCFTypeDictionaryKeyCallBacks: std::ffi::c_void;
                static kCFTypeDictionaryValueCallBacks: std::ffi::c_void;
                static kCFBooleanTrue: *const std::ffi::c_void;
            }
            let opt_key = NSString::from_str("kCGDisplayShowDuplicateLowResolutionModes");
            let cf_key = &*opt_key as *const _ as *const std::ffi::c_void;
            let cf_val = kCFBooleanTrue;
            let options_dict = CFDictionaryCreate(
                std::ptr::null(),
                &cf_key,
                &cf_val,
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );

            let modes_cf = CGDisplayCopyAllDisplayModes(display_id, options_dict);
            if !options_dict.is_null() {
                CFRelease(options_dict);
            }

            if !modes_cf.is_null() {
                let count = CFArrayGetCount(modes_cf);
                info!("Available display modes ({}):", count);

                let mut best_mode: *const std::ffi::c_void = std::ptr::null();
                let mut best_pw: usize = 0;

                for i in 0..count {
                    let m = CFArrayGetValueAtIndex(modes_cf, i);
                    let lw = CGDisplayModeGetWidth(m);
                    let lh = CGDisplayModeGetHeight(m);
                    let pw = CGDisplayModeGetPixelWidth(m);
                    let ph = CGDisplayModeGetPixelHeight(m);
                    let is_hidpi = pw != lw;
                    info!("  Mode {}: {}x{} logical, {}x{} pixels{}",
                          i, lw, lh, pw, ph,
                          if is_hidpi { " (HiDPI)" } else { "" });

                    if pw > best_pw {
                        best_pw = pw;
                        best_mode = m;
                    }
                }

                // If the best mode has more pixels than current, switch to it
                let cur_pw = CGDisplayPixelsWide(display_id);
                if !best_mode.is_null() && best_pw > cur_pw {
                    info!("Switching to best mode: {}x{} pixels",
                          CGDisplayModeGetPixelWidth(best_mode),
                          CGDisplayModeGetPixelHeight(best_mode));
                    let err = CGDisplaySetDisplayMode(display_id, best_mode, std::ptr::null());
                    if err != 0 {
                        warn!("CGDisplaySetDisplayMode failed with error {}", err);
                    } else {
                        // Wait for mode change to take effect
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        let new_pw = CGDisplayPixelsWide(display_id);
                        let new_ph = CGDisplayPixelsHigh(display_id);
                        info!("After mode switch: {}x{} pixels", new_pw, new_ph);
                    }
                } else {
                    info!("Current mode already uses max pixel resolution ({}x{})",
                          cur_pw, CGDisplayPixelsHigh(display_id));
                }

                CFRelease(modes_cf);
            } else {
                warn!("CGDisplayCopyAllDisplayModes returned null");
            }

            // Ensure cursor is visible on the virtual display.
            CGDisplayShowCursor(display_id);
            CGDisplayMoveCursorToPoint(display_id, NSPoint {
                x: width as f64 / 2.0,
                y: height as f64 / 2.0,
            });
            info!("Cursor shown and moved to center of virtual display");

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
