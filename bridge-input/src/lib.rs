//! Bridge Input Subsystem
//!
//! Provides low-latency input capture and injection:
//! - Keyboard/Mouse capture via IOHIDManager and CGEventTap
//! - Event injection via CGEventPost (for testing) or DriverKit virtual HID

pub mod capture;
pub mod inject;
mod sys;

pub use capture::*;
pub use inject::*;


/// Check if accessibility permission is granted (required for event capture)
pub fn has_accessibility_permission() -> bool {
    unsafe {
        extern "C" {
            fn AXIsProcessTrusted() -> bool;
        }
        AXIsProcessTrusted()
    }
}

/// Request accessibility permission (opens System Preferences)
pub fn request_accessibility_permission() {
    unsafe {
        extern "C" {
            fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
        }

        // Create options dictionary with prompt = true
        // This would open the System Preferences dialog

        // For now, just check trusted status
        AXIsProcessTrustedWithOptions(std::ptr::null());
    }
}

/// Convert macOS key code to a string representation (for debugging)
pub fn keycode_to_string(keycode: u16) -> &'static str {
    match keycode {
        0x00 => "A",
        0x01 => "S",
        0x02 => "D",
        0x03 => "F",
        0x04 => "H",
        0x05 => "G",
        0x06 => "Z",
        0x07 => "X",
        0x08 => "C",
        0x09 => "V",
        0x0B => "B",
        0x0C => "Q",
        0x0D => "W",
        0x0E => "E",
        0x0F => "R",
        0x10 => "Y",
        0x11 => "T",
        0x12 => "1",
        0x13 => "2",
        0x14 => "3",
        0x15 => "4",
        0x16 => "6",
        0x17 => "5",
        0x18 => "=",
        0x19 => "9",
        0x1A => "7",
        0x1B => "-",
        0x1C => "8",
        0x1D => "0",
        0x1E => "]",
        0x1F => "O",
        0x20 => "U",
        0x21 => "[",
        0x22 => "I",
        0x23 => "P",
        0x24 => "Return",
        0x25 => "L",
        0x26 => "J",
        0x27 => "'",
        0x28 => "K",
        0x29 => ";",
        0x2A => "\\",
        0x2B => ",",
        0x2C => "/",
        0x2D => "N",
        0x2E => "M",
        0x2F => ".",
        0x30 => "Tab",
        0x31 => "Space",
        0x32 => "`",
        0x33 => "Delete",
        0x35 => "Escape",
        0x36 => "RightCommand",
        0x37 => "Command",
        0x38 => "Shift",
        0x39 => "CapsLock",
        0x3A => "Option",
        0x3B => "Control",
        0x3C => "RightShift",
        0x3D => "RightOption",
        0x3E => "RightControl",
        0x3F => "Function",
        0x7A => "F1",
        0x78 => "F2",
        0x63 => "F3",
        0x76 => "F4",
        0x60 => "F5",
        0x61 => "F6",
        0x62 => "F7",
        0x64 => "F8",
        0x65 => "F9",
        0x6D => "F10",
        0x67 => "F11",
        0x6F => "F12",
        0x7B => "LeftArrow",
        0x7C => "RightArrow",
        0x7D => "DownArrow",
        0x7E => "UpArrow",
        _ => "Unknown",
    }
}
