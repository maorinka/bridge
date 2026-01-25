//! Input capture using CGEventTap and IOHIDManager
//!
//! CGEventTap provides system-wide keyboard and mouse event capture.
//! IOHIDManager provides raw HID device access for gaming peripherals.

use bridge_common::{
    BridgeResult, BridgeError, InputEvent, KeyboardEvent, MouseMoveEvent,
    MouseButtonEvent, MouseScrollEvent, ModifierFlags, now_us,
};
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use parking_lot::Mutex;
use crossbeam_channel::{bounded, Receiver, Sender};
use tracing::{debug, error, info};

use crate::sys::*;

/// Input capture configuration
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Capture keyboard events
    pub capture_keyboard: bool,
    /// Capture mouse movement
    pub capture_mouse_move: bool,
    /// Capture mouse buttons
    pub capture_mouse_buttons: bool,
    /// Capture scroll wheel
    pub capture_scroll: bool,
    /// Use raw HID for gaming devices
    pub use_raw_hid: bool,
    /// Capture in listen-only mode (don't block events)
    pub listen_only: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            capture_keyboard: true,
            capture_mouse_move: true,
            capture_mouse_buttons: true,
            capture_scroll: true,
            use_raw_hid: false,
            listen_only: false, // Block events when capturing
        }
    }
}

/// Input event capturer
pub struct InputCapturer {
    config: CaptureConfig,
    event_tx: Sender<InputEvent>,
    event_rx: Receiver<InputEvent>,
    is_running: Arc<AtomicBool>,
    tap_thread: Option<thread::JoinHandle<()>>,
    screen_width: f64,
    screen_height: f64,
}

unsafe impl Send for InputCapturer {}

impl InputCapturer {
    /// Create a new input capturer
    pub fn new(config: CaptureConfig) -> BridgeResult<Self> {
        let (event_tx, event_rx) = bounded(256); // Buffer for input events

        // Get screen dimensions for coordinate normalization
        let (screen_width, screen_height) = unsafe {
            let display = CGMainDisplayID();
            (
                CGDisplayPixelsWide(display) as f64,
                CGDisplayPixelsHigh(display) as f64,
            )
        };

        Ok(Self {
            config,
            event_tx,
            event_rx,
            is_running: Arc::new(AtomicBool::new(false)),
            tap_thread: None,
            screen_width,
            screen_height,
        })
    }

    /// Start capturing input events
    pub fn start(&mut self) -> BridgeResult<()> {
        if self.is_running.load(Ordering::SeqCst) {
            return Ok(());
        }

        info!("Starting input capture");

        // Check accessibility permission
        if !crate::has_accessibility_permission() {
            return Err(BridgeError::Input(
                "Accessibility permission required. Enable in System Preferences > Security & Privacy > Privacy > Accessibility".into()
            ));
        }

        self.is_running.store(true, Ordering::SeqCst);

        // Start event tap in a separate thread with its own run loop
        let event_tx = self.event_tx.clone();
        let is_running = self.is_running.clone();
        let config = self.config.clone();
        let screen_width = self.screen_width;
        let screen_height = self.screen_height;

        let handle = thread::spawn(move || {
            run_event_tap(
                event_tx,
                is_running,
                config,
                screen_width,
                screen_height,
            );
        });

        self.tap_thread = Some(handle);

        Ok(())
    }

    /// Stop capturing input events
    pub fn stop(&mut self) -> BridgeResult<()> {
        if !self.is_running.load(Ordering::SeqCst) {
            return Ok(());
        }

        info!("Stopping input capture");

        self.is_running.store(false, Ordering::SeqCst);

        // Wait for thread to finish
        if let Some(handle) = self.tap_thread.take() {
            let _ = handle.join();
        }

        Ok(())
    }

    /// Get the next input event (non-blocking)
    pub fn recv_event(&self) -> Option<InputEvent> {
        self.event_rx.try_recv().ok()
    }

    /// Get the next input event (blocking)
    pub fn recv_event_blocking(&self) -> BridgeResult<InputEvent> {
        self.event_rx.recv().map_err(|_| BridgeError::Input("Event channel closed".into()))
    }

    /// Get a receiver for input events
    pub fn event_receiver(&self) -> Receiver<InputEvent> {
        self.event_rx.clone()
    }

    /// Check if capturing is active
    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }
}

impl Drop for InputCapturer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

// Context passed to event tap callback
struct EventTapContext {
    event_tx: Sender<InputEvent>,
    screen_width: f64,
    screen_height: f64,
    last_mouse_pos: Mutex<CGPoint>,
}

fn run_event_tap(
    event_tx: Sender<InputEvent>,
    is_running: Arc<AtomicBool>,
    config: CaptureConfig,
    screen_width: f64,
    screen_height: f64,
) {
    // Build event mask
    let mut event_mask: u64 = 0;

    if config.capture_keyboard {
        event_mask |= cg_event_mask_bit(CGEventType::KeyDown);
        event_mask |= cg_event_mask_bit(CGEventType::KeyUp);
        event_mask |= cg_event_mask_bit(CGEventType::FlagsChanged);
    }

    if config.capture_mouse_move {
        event_mask |= cg_event_mask_bit(CGEventType::MouseMoved);
        event_mask |= cg_event_mask_bit(CGEventType::LeftMouseDragged);
        event_mask |= cg_event_mask_bit(CGEventType::RightMouseDragged);
        event_mask |= cg_event_mask_bit(CGEventType::OtherMouseDragged);
    }

    if config.capture_mouse_buttons {
        event_mask |= cg_event_mask_bit(CGEventType::LeftMouseDown);
        event_mask |= cg_event_mask_bit(CGEventType::LeftMouseUp);
        event_mask |= cg_event_mask_bit(CGEventType::RightMouseDown);
        event_mask |= cg_event_mask_bit(CGEventType::RightMouseUp);
        event_mask |= cg_event_mask_bit(CGEventType::OtherMouseDown);
        event_mask |= cg_event_mask_bit(CGEventType::OtherMouseUp);
    }

    if config.capture_scroll {
        event_mask |= cg_event_mask_bit(CGEventType::ScrollWheel);
    }

    // Create callback context
    let context = Box::new(EventTapContext {
        event_tx,
        screen_width,
        screen_height,
        last_mouse_pos: Mutex::new(CGPoint::default()),
    });
    let context_ptr = Box::into_raw(context);

    unsafe {
        // Create event tap
        let tap_options = if config.listen_only {
            CGEventTapOptions::ListenOnly
        } else {
            CGEventTapOptions::Default
        };

        let tap = CGEventTapCreate(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            tap_options,
            event_mask,
            event_tap_callback,
            context_ptr as *mut c_void,
        );

        if tap.is_null() {
            error!("Failed to create event tap. Check accessibility permissions.");
            let _ = Box::from_raw(context_ptr); // Clean up
            return;
        }

        // Create run loop source
        let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0);
        if source.is_null() {
            error!("Failed to create run loop source");
            CFRelease(tap as CFTypeRef);
            let _ = Box::from_raw(context_ptr);
            return;
        }

        // Add to current run loop
        let run_loop = CFRunLoopGetCurrent();
        CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);

        // Enable the tap
        CGEventTapEnable(tap, true);

        debug!("Event tap started");

        // Run the run loop until stopped
        while is_running.load(Ordering::SeqCst) {
            // Run for a short duration to allow checking is_running
            extern "C" {
                fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source_handled: bool) -> i32;
            }
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, false);
        }

        // Clean up
        CGEventTapEnable(tap, false);
        CFRunLoopRemoveSource(run_loop, source, kCFRunLoopCommonModes);
        CFMachPortInvalidate(tap);
        CFRelease(source as CFTypeRef);
        CFRelease(tap as CFTypeRef);
        let _ = Box::from_raw(context_ptr);
    }

    debug!("Event tap stopped");
}

extern "C" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    if user_info.is_null() || event.is_null() {
        return event;
    }

    let ctx = unsafe { &*(user_info as *const EventTapContext) };
    let timestamp = now_us();

    match event_type {
        CGEventType::KeyDown | CGEventType::KeyUp => {
            let key_code = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16 };
            let flags = unsafe { CGEventGetFlags(event) };
            let is_pressed = event_type == CGEventType::KeyDown;

            let input_event = InputEvent::Keyboard(KeyboardEvent {
                key_code,
                is_pressed,
                modifiers: flags_to_modifiers(flags),
                timestamp_us: timestamp,
            });

            let _ = ctx.event_tx.try_send(input_event);
        }

        CGEventType::FlagsChanged => {
            // Handle modifier key changes
            let flags = unsafe { CGEventGetFlags(event) };
            let key_code = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) as u16 };

            // Determine if it's a press or release based on the flags
            let is_pressed = match key_code {
                0x37 | 0x36 => flags & event_flags::COMMAND != 0, // Command
                0x38 | 0x3C => flags & event_flags::SHIFT != 0,   // Shift
                0x3A | 0x3D => flags & event_flags::ALTERNATE != 0, // Option
                0x3B | 0x3E => flags & event_flags::CONTROL != 0, // Control
                0x39 => flags & event_flags::ALPHA_SHIFT != 0,    // Caps Lock
                0x3F => flags & event_flags::SECONDARY_FN != 0,   // Fn
                _ => true,
            };

            let input_event = InputEvent::Keyboard(KeyboardEvent {
                key_code,
                is_pressed,
                modifiers: flags_to_modifiers(flags),
                timestamp_us: timestamp,
            });

            let _ = ctx.event_tx.try_send(input_event);
        }

        CGEventType::MouseMoved |
        CGEventType::LeftMouseDragged |
        CGEventType::RightMouseDragged |
        CGEventType::OtherMouseDragged => {
            let location = unsafe { CGEventGetLocation(event) };
            let mut last_pos = ctx.last_mouse_pos.lock();

            let delta_x = location.x - last_pos.x;
            let delta_y = location.y - last_pos.y;
            *last_pos = location;

            let input_event = InputEvent::MouseMove(MouseMoveEvent {
                x: location.x / ctx.screen_width,
                y: location.y / ctx.screen_height,
                delta_x,
                delta_y,
                is_absolute: true,
                timestamp_us: timestamp,
            });

            let _ = ctx.event_tx.try_send(input_event);
        }

        CGEventType::LeftMouseDown |
        CGEventType::LeftMouseUp |
        CGEventType::RightMouseDown |
        CGEventType::RightMouseUp |
        CGEventType::OtherMouseDown |
        CGEventType::OtherMouseUp => {
            let button = match event_type {
                CGEventType::LeftMouseDown | CGEventType::LeftMouseUp => 0,
                CGEventType::RightMouseDown | CGEventType::RightMouseUp => 1,
                _ => 2, // Middle/other
            };

            let is_pressed = matches!(
                event_type,
                CGEventType::LeftMouseDown | CGEventType::RightMouseDown | CGEventType::OtherMouseDown
            );

            let click_count = unsafe {
                CGEventGetIntegerValueField(event, K_CG_MOUSE_EVENT_CLICK_STATE) as u8
            };

            let input_event = InputEvent::MouseButton(MouseButtonEvent {
                button,
                is_pressed,
                click_count,
                timestamp_us: timestamp,
            });

            let _ = ctx.event_tx.try_send(input_event);
        }

        CGEventType::ScrollWheel => {
            let delta_y = unsafe {
                CGEventGetDoubleValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1)
            };
            let delta_x = unsafe {
                CGEventGetDoubleValueField(event, K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2)
            };
            let is_continuous = unsafe {
                CGEventGetIntegerValueField(event, K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS) != 0
            };

            let input_event = InputEvent::MouseScroll(MouseScrollEvent {
                delta_x,
                delta_y,
                is_continuous,
                timestamp_us: timestamp,
            });

            let _ = ctx.event_tx.try_send(input_event);
        }

        _ => {}
    }

    event
}

fn flags_to_modifiers(flags: u64) -> ModifierFlags {
    ModifierFlags {
        shift: flags & event_flags::SHIFT != 0,
        control: flags & event_flags::CONTROL != 0,
        option: flags & event_flags::ALTERNATE != 0,
        command: flags & event_flags::COMMAND != 0,
        caps_lock: flags & event_flags::ALPHA_SHIFT != 0,
        fn_key: flags & event_flags::SECONDARY_FN != 0,
    }
}
