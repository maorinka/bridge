//! Input injection using CGEventPost and virtual HID
//!
//! CGEventPost provides system-level event injection.
//! For production use with games, a DriverKit virtual HID device
//! would provide lower latency and better compatibility.

use bridge_common::{
    BridgeResult, BridgeError, InputEvent, KeyboardEvent, MouseMoveEvent,
    MouseButtonEvent, MouseScrollEvent, ModifierFlags,
};
use tracing::{debug, trace, warn};

use crate::sys::*;

/// Input injector for posting events to the system
pub struct InputInjector {
    event_source: CGEventSourceRef,
    screen_width: f64,
    screen_height: f64,
    current_modifiers: ModifierFlags,
}

unsafe impl Send for InputInjector {}

impl InputInjector {
    /// Create a new input injector
    pub fn new() -> BridgeResult<Self> {
        let event_source = unsafe {
            CGEventSourceCreate(CGEventSourceStateID::HIDSystemState)
        };

        if event_source.is_null() {
            return Err(BridgeError::Input("Failed to create event source".into()));
        }

        // Get screen dimensions
        let (screen_width, screen_height) = unsafe {
            let display = CGMainDisplayID();
            (
                CGDisplayPixelsWide(display) as f64,
                CGDisplayPixelsHigh(display) as f64,
            )
        };

        Ok(Self {
            event_source,
            screen_width,
            screen_height,
            current_modifiers: ModifierFlags::default(),
        })
    }

    /// Inject an input event
    pub fn inject(&mut self, event: &InputEvent) -> BridgeResult<()> {
        match event {
            InputEvent::Keyboard(e) => self.inject_keyboard(e),
            InputEvent::MouseMove(e) => self.inject_mouse_move(e),
            InputEvent::MouseButton(e) => self.inject_mouse_button(e),
            InputEvent::MouseScroll(e) => self.inject_scroll(e),
            InputEvent::RawHid(_) => {
                // Raw HID requires DriverKit virtual device
                warn!("Raw HID injection not implemented - requires DriverKit");
                Ok(())
            }
        }
    }

    fn inject_keyboard(&mut self, event: &KeyboardEvent) -> BridgeResult<()> {
        unsafe {
            let cg_event = CGEventCreateKeyboardEvent(
                self.event_source,
                event.key_code,
                event.is_pressed,
            );

            if cg_event.is_null() {
                return Err(BridgeError::Input("Failed to create keyboard event".into()));
            }

            // Set modifier flags
            let flags = modifiers_to_flags(&event.modifiers);
            CGEventSetFlags(cg_event, flags);

            // Post the event
            CGEventPost(CGEventTapLocation::HIDEventTap, cg_event);

            CFRelease(cg_event as CFTypeRef);
        }

        self.current_modifiers = event.modifiers;
        trace!("Injected keyboard event: key={}, pressed={}", event.key_code, event.is_pressed);

        Ok(())
    }

    fn inject_mouse_move(&mut self, event: &MouseMoveEvent) -> BridgeResult<()> {
        let position = if event.is_absolute {
            CGPoint::new(
                event.x * self.screen_width,
                event.y * self.screen_height,
            )
        } else {
            // For relative movement, we need to get current position and add delta
            // This is a simplified implementation
            unsafe {
                extern "C" {
                    fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
                }
                let temp_event = CGEventCreate(self.event_source);
                let current = CGEventGetLocation(temp_event);
                CFRelease(temp_event as CFTypeRef);

                CGPoint::new(
                    current.x + event.delta_x,
                    current.y + event.delta_y,
                )
            }
        };

        unsafe {
            let cg_event = CGEventCreateMouseEvent(
                self.event_source,
                CGEventType::MouseMoved,
                position,
                CGMouseButton::Left, // Not used for move events
            );

            if cg_event.is_null() {
                return Err(BridgeError::Input("Failed to create mouse move event".into()));
            }

            CGEventPost(CGEventTapLocation::HIDEventTap, cg_event);
            CFRelease(cg_event as CFTypeRef);
        }

        trace!("Injected mouse move: x={:.2}, y={:.2}", position.x, position.y);

        Ok(())
    }

    fn inject_mouse_button(&mut self, event: &MouseButtonEvent) -> BridgeResult<()> {
        // Get current mouse position for the event
        let position = unsafe {
            extern "C" {
                fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
            }
            let temp_event = CGEventCreate(self.event_source);
            let pos = CGEventGetLocation(temp_event);
            CFRelease(temp_event as CFTypeRef);
            pos
        };

        let (event_type, button) = match (event.button, event.is_pressed) {
            (0, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
            (0, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
            (1, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
            (1, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
            (_, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (_, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        };

        unsafe {
            let cg_event = CGEventCreateMouseEvent(
                self.event_source,
                event_type,
                position,
                button,
            );

            if cg_event.is_null() {
                return Err(BridgeError::Input("Failed to create mouse button event".into()));
            }

            // Set click count
            CGEventSetIntegerValueField(
                cg_event,
                K_CG_MOUSE_EVENT_CLICK_STATE,
                event.click_count as i64,
            );

            CGEventPost(CGEventTapLocation::HIDEventTap, cg_event);
            CFRelease(cg_event as CFTypeRef);
        }

        trace!("Injected mouse button: button={}, pressed={}", event.button, event.is_pressed);

        Ok(())
    }

    fn inject_scroll(&mut self, event: &MouseScrollEvent) -> BridgeResult<()> {
        unsafe {
            let units = if event.is_continuous {
                CGScrollEventUnit::Pixel
            } else {
                CGScrollEventUnit::Line
            };

            // Create scroll event with both axes
            let cg_event = CGEventCreateScrollWheelEvent(
                self.event_source,
                units,
                2, // wheel count
                event.delta_y as i32,
                event.delta_x as i32,
            );

            if cg_event.is_null() {
                return Err(BridgeError::Input("Failed to create scroll event".into()));
            }

            CGEventPost(CGEventTapLocation::HIDEventTap, cg_event);
            CFRelease(cg_event as CFTypeRef);
        }

        trace!("Injected scroll: dx={:.2}, dy={:.2}", event.delta_x, event.delta_y);

        Ok(())
    }

    /// Move mouse cursor to absolute position
    pub fn warp_cursor(&self, x: f64, y: f64) -> BridgeResult<()> {
        let position = CGPoint::new(
            x * self.screen_width,
            y * self.screen_height,
        );

        unsafe {
            let result = CGWarpMouseCursorPosition(position);
            if result != 0 {
                return Err(BridgeError::Input(format!("Failed to warp cursor: {}", result)));
            }
        }

        Ok(())
    }

    /// Capture/release mouse cursor (for relative mouse mode in games)
    pub fn set_cursor_captured(&self, captured: bool) -> BridgeResult<()> {
        unsafe {
            // When captured, disassociate mouse movement from cursor position
            // This allows games to use raw mouse delta
            CGAssociateMouseAndMouseCursorPosition(!captured);
        }

        debug!("Cursor capture: {}", captured);
        Ok(())
    }
}

impl Drop for InputInjector {
    fn drop(&mut self) {
        if !self.event_source.is_null() {
            unsafe {
                CFRelease(self.event_source as CFTypeRef);
            }
        }
    }
}

fn modifiers_to_flags(mods: &ModifierFlags) -> u64 {
    let mut flags = 0u64;

    if mods.shift {
        flags |= event_flags::SHIFT;
    }
    if mods.control {
        flags |= event_flags::CONTROL;
    }
    if mods.option {
        flags |= event_flags::ALTERNATE;
    }
    if mods.command {
        flags |= event_flags::COMMAND;
    }
    if mods.caps_lock {
        flags |= event_flags::ALPHA_SHIFT;
    }
    if mods.fn_key {
        flags |= event_flags::SECONDARY_FN;
    }

    flags
}

/// Placeholder for DriverKit virtual HID device integration
/// In production, this would communicate with a DriverKit extension
pub struct VirtualHidDevice {
    // Would contain connection to IOKit user client
}

impl VirtualHidDevice {
    /// Create connection to DriverKit virtual HID device
    pub fn new() -> BridgeResult<Self> {
        // In production:
        // 1. Look for the DriverKit extension's user client
        // 2. Open a connection via IOKit
        // 3. Set up shared memory for low-latency communication

        Err(BridgeError::Input(
            "DriverKit virtual HID not implemented. \
            Requires DriverKit extension development with Apple Developer account.".into()
        ))
    }

    /// Send a raw HID report to the virtual device
    pub fn send_report(&self, _report: &[u8]) -> BridgeResult<()> {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modifiers_to_flags() {
        let mods = ModifierFlags {
            shift: true,
            control: false,
            option: true,
            command: false,
            caps_lock: false,
            fn_key: false,
        };

        let flags = modifiers_to_flags(&mods);
        assert!(flags & event_flags::SHIFT != 0);
        assert!(flags & event_flags::ALTERNATE != 0);
        assert!(flags & event_flags::CONTROL == 0);
        assert!(flags & event_flags::COMMAND == 0);
    }
}
