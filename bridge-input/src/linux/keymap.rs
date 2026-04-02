//! macOS virtual keycode to Linux KEY_* mapping
//!
//! The Bridge protocol uses macOS keycodes as the canonical format.
//! This module translates them to Linux input event codes.

/// Convert macOS virtual keycode to Linux input event code
pub fn macos_to_linux_keycode(macos_code: u16) -> u16 {
    match macos_code {
        // Letters
        0x00 => 30,  // A → KEY_A
        0x01 => 31,  // S → KEY_S
        0x02 => 32,  // D → KEY_D
        0x03 => 33,  // F → KEY_F
        0x04 => 35,  // H → KEY_H
        0x05 => 34,  // G → KEY_G
        0x06 => 44,  // Z → KEY_Z
        0x07 => 45,  // X → KEY_X
        0x08 => 46,  // C → KEY_C
        0x09 => 47,  // V → KEY_V
        0x0B => 48,  // B → KEY_B
        0x0C => 16,  // Q → KEY_Q
        0x0D => 17,  // W → KEY_W
        0x0E => 18,  // E → KEY_E
        0x0F => 19,  // R → KEY_R
        0x10 => 21,  // Y → KEY_Y
        0x11 => 20,  // T → KEY_T
        0x1F => 24,  // O → KEY_O
        0x20 => 22,  // U → KEY_U
        0x22 => 23,  // I → KEY_I
        0x23 => 25,  // P → KEY_P
        0x25 => 38,  // L → KEY_L
        0x26 => 36,  // J → KEY_J
        0x28 => 37,  // K → KEY_K
        0x2D => 49,  // N → KEY_N
        0x2E => 50,  // M → KEY_M

        // Numbers
        0x12 => 2,   // 1 → KEY_1
        0x13 => 3,   // 2 → KEY_2
        0x14 => 4,   // 3 → KEY_3
        0x15 => 5,   // 4 → KEY_4
        0x17 => 6,   // 5 → KEY_5
        0x16 => 7,   // 6 → KEY_6
        0x1A => 8,   // 7 → KEY_7
        0x1C => 9,   // 8 → KEY_8
        0x19 => 10,  // 9 → KEY_9
        0x1D => 11,  // 0 → KEY_0

        // Punctuation
        0x18 => 13,  // = → KEY_EQUAL
        0x1B => 12,  // - → KEY_MINUS
        0x1E => 27,  // ] → KEY_RIGHTBRACE
        0x21 => 26,  // [ → KEY_LEFTBRACE
        0x27 => 40,  // ' → KEY_APOSTROPHE
        0x29 => 39,  // ; → KEY_SEMICOLON
        0x2A => 43,  // \ → KEY_BACKSLASH
        0x2B => 51,  // , → KEY_COMMA
        0x2C => 53,  // / → KEY_SLASH
        0x2F => 52,  // . → KEY_DOT
        0x32 => 41,  // ` → KEY_GRAVE

        // Special keys
        0x24 => 28,  // Return → KEY_ENTER
        0x30 => 15,  // Tab → KEY_TAB
        0x31 => 57,  // Space → KEY_SPACE
        0x33 => 14,  // Delete → KEY_BACKSPACE
        0x35 => 1,   // Escape → KEY_ESC

        // Modifiers
        0x37 => 125, // Command → KEY_LEFTMETA
        0x36 => 126, // RightCommand → KEY_RIGHTMETA
        0x38 => 42,  // Shift → KEY_LEFTSHIFT
        0x3C => 54,  // RightShift → KEY_RIGHTSHIFT
        0x39 => 58,  // CapsLock → KEY_CAPSLOCK
        0x3A => 56,  // Option → KEY_LEFTALT
        0x3D => 100, // RightOption → KEY_RIGHTALT
        0x3B => 29,  // Control → KEY_LEFTCTRL
        0x3E => 97,  // RightControl → KEY_RIGHTCTRL
        0x3F => 464, // Function → KEY_FN

        // Function keys
        0x7A => 59,  // F1
        0x78 => 60,  // F2
        0x63 => 61,  // F3
        0x76 => 62,  // F4
        0x60 => 63,  // F5
        0x61 => 64,  // F6
        0x62 => 65,  // F7
        0x64 => 66,  // F8
        0x65 => 67,  // F9
        0x6D => 68,  // F10
        0x67 => 87,  // F11
        0x6F => 88,  // F12

        // Arrow keys
        0x7B => 105, // Left → KEY_LEFT
        0x7C => 106, // Right → KEY_RIGHT
        0x7D => 108, // Down → KEY_DOWN
        0x7E => 103, // Up → KEY_UP

        _ => 0, // KEY_RESERVED (unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_letter_keys() {
        assert_eq!(macos_to_linux_keycode(0x00), 30); // A
        assert_eq!(macos_to_linux_keycode(0x01), 31); // S
        assert_eq!(macos_to_linux_keycode(0x0C), 16); // Q
    }

    #[test]
    fn test_special_keys() {
        assert_eq!(macos_to_linux_keycode(0x24), 28); // Return
        assert_eq!(macos_to_linux_keycode(0x31), 57); // Space
        assert_eq!(macos_to_linux_keycode(0x35), 1);  // Escape
        assert_eq!(macos_to_linux_keycode(0x33), 14); // Backspace
    }

    #[test]
    fn test_modifier_keys() {
        assert_eq!(macos_to_linux_keycode(0x37), 125); // Command → Meta
        assert_eq!(macos_to_linux_keycode(0x38), 42);  // Shift
        assert_eq!(macos_to_linux_keycode(0x3B), 29);  // Control
    }

    #[test]
    fn test_arrow_keys() {
        assert_eq!(macos_to_linux_keycode(0x7B), 105); // Left
        assert_eq!(macos_to_linux_keycode(0x7E), 103); // Up
    }

    #[test]
    fn test_unknown_returns_zero() {
        assert_eq!(macos_to_linux_keycode(0xFF), 0);
    }
}
