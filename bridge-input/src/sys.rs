//! Low-level system bindings for macOS input APIs
//!
//! This module provides FFI bindings to:
//! - CGEventTap (for event capture at the Quartz level)
//! - IOHIDManager (for raw HID device access)
//! - CGEvent (for event creation and posting)
//!
//! Many of these bindings are not yet used because input capture/injection
//! is currently disabled. They will be needed when input is re-enabled.
#![allow(dead_code)]

use std::ffi::c_void;

// Core Foundation types
pub type CFTypeRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFMutableDictionaryRef = *mut c_void;
pub type CFRunLoopRef = *const c_void;
pub type CFRunLoopSourceRef = *mut c_void;
pub type CFMachPortRef = *mut c_void;

// Core Graphics types
pub type CGEventRef = *mut c_void;
pub type CGEventSourceRef = *mut c_void;
pub type CGEventTapProxy = *mut c_void;
pub type CGFloat = f64;

// IOKit/HID types
pub type IOHIDManagerRef = *mut c_void;
pub type IOHIDDeviceRef = *mut c_void;
pub type IOHIDElementRef = *mut c_void;
pub type IOHIDValueRef = *mut c_void;
pub type IOReturn = i32;

/// CGEventType enum values
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CGEventType {
    Null = 0,
    LeftMouseDown = 1,
    LeftMouseUp = 2,
    RightMouseDown = 3,
    RightMouseUp = 4,
    MouseMoved = 5,
    LeftMouseDragged = 6,
    RightMouseDragged = 7,
    KeyDown = 10,
    KeyUp = 11,
    FlagsChanged = 12,
    ScrollWheel = 22,
    TabletPointer = 23,
    TabletProximity = 24,
    OtherMouseDown = 25,
    OtherMouseUp = 26,
    OtherMouseDragged = 27,
    TapDisabledByTimeout = 0xFFFFFFFE,
    TapDisabledByUserInput = 0xFFFFFFFF,
}

/// CGEventTapLocation enum values
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub enum CGEventTapLocation {
    HIDEventTap = 0,
    SessionEventTap = 1,
    AnnotatedSessionEventTap = 2,
}

/// CGEventTapPlacement enum values
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub enum CGEventTapPlacement {
    HeadInsertEventTap = 0,
    TailAppendEventTap = 1,
}

/// CGEventTapOptions enum values
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub enum CGEventTapOptions {
    Default = 0,
    ListenOnly = 1,
}

/// CGEventSourceStateID enum values
#[repr(i32)]
#[derive(Debug, Clone, Copy)]
pub enum CGEventSourceStateID {
    Private = -1,
    CombinedSessionState = 0,
    HIDSystemState = 1,
}

/// CGMouseButton enum values
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub enum CGMouseButton {
    Left = 0,
    Right = 1,
    Center = 2,
}

/// CGScrollEventUnit enum values
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub enum CGScrollEventUnit {
    Pixel = 0,
    Line = 1,
}

// Event field constants
pub const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
pub const K_CG_MOUSE_EVENT_CLICK_STATE: u32 = 1;
pub const K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_1: u32 = 11;
pub const K_CG_SCROLL_WHEEL_EVENT_DELTA_AXIS_2: u32 = 12;
pub const K_CG_SCROLL_WHEEL_EVENT_IS_CONTINUOUS: u32 = 88;

// CGPoint structure
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGPoint {
    pub x: CGFloat,
    pub y: CGFloat,
}

impl CGPoint {
    pub fn new(x: CGFloat, y: CGFloat) -> Self {
        Self { x, y }
    }
}

// Event callback type
pub type CGEventTapCallBack = extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

// HID callback types
pub type IOHIDDeviceCallback = extern "C" fn(
    context: *mut c_void,
    result: IOReturn,
    sender: *mut c_void,
    device: IOHIDDeviceRef,
);

pub type IOHIDValueCallback = extern "C" fn(
    context: *mut c_void,
    result: IOReturn,
    sender: *mut c_void,
    value: IOHIDValueRef,
);

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFAllocatorDefault: CFAllocatorRef;
    pub static kCFRunLoopDefaultMode: CFStringRef;
    pub static kCFRunLoopCommonModes: CFStringRef;

    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopGetMain() -> CFRunLoopRef;
    pub fn CFRunLoopRun();
    pub fn CFRunLoopStop(rl: CFRunLoopRef);
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);

    pub fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;

    pub fn CFMachPortInvalidate(port: CFMachPortRef);
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    // Event tap functions
    pub fn CGEventTapCreate(
        tap: CGEventTapLocation,
        place: CGEventTapPlacement,
        options: CGEventTapOptions,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;

    pub fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);

    // Event source functions
    pub fn CGEventSourceCreate(state_id: CGEventSourceStateID) -> CGEventSourceRef;

    // Event creation functions
    pub fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtual_key: u16,
        key_down: bool,
    ) -> CGEventRef;

    pub fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        mouse_type: CGEventType,
        mouse_cursor_position: CGPoint,
        mouse_button: CGMouseButton,
    ) -> CGEventRef;

    pub fn CGEventCreateScrollWheelEvent(
        source: CGEventSourceRef,
        units: CGScrollEventUnit,
        wheel_count: u32,
        wheel1: i32,
        ...
    ) -> CGEventRef;

    // Event posting
    pub fn CGEventPost(tap: CGEventTapLocation, event: CGEventRef);
    pub fn CGEventPostToPid(pid: i32, event: CGEventRef);

    // Event properties
    pub fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    pub fn CGEventSetLocation(event: CGEventRef, location: CGPoint);
    pub fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    pub fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    pub fn CGEventGetDoubleValueField(event: CGEventRef, field: u32) -> f64;
    pub fn CGEventSetDoubleValueField(event: CGEventRef, field: u32, value: f64);
    pub fn CGEventGetFlags(event: CGEventRef) -> u64;
    pub fn CGEventSetFlags(event: CGEventRef, flags: u64);
    pub fn CGEventGetType(event: CGEventRef) -> CGEventType;

    // Display functions
    pub fn CGMainDisplayID() -> u32;
    pub fn CGDisplayPixelsWide(display: u32) -> usize;
    pub fn CGDisplayPixelsHigh(display: u32) -> usize;

    // Warp cursor
    pub fn CGWarpMouseCursorPosition(new_cursor_position: CGPoint) -> i32;
    pub fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    // IOHIDManager functions
    pub fn IOHIDManagerCreate(
        allocator: CFAllocatorRef,
        options: u32,
    ) -> IOHIDManagerRef;

    pub fn IOHIDManagerSetDeviceMatching(
        manager: IOHIDManagerRef,
        matching: CFDictionaryRef,
    );

    pub fn IOHIDManagerSetDeviceMatchingMultiple(
        manager: IOHIDManagerRef,
        multiple: *const c_void, // CFArrayRef
    );

    pub fn IOHIDManagerRegisterDeviceMatchingCallback(
        manager: IOHIDManagerRef,
        callback: IOHIDDeviceCallback,
        context: *mut c_void,
    );

    pub fn IOHIDManagerRegisterDeviceRemovalCallback(
        manager: IOHIDManagerRef,
        callback: IOHIDDeviceCallback,
        context: *mut c_void,
    );

    pub fn IOHIDManagerRegisterInputValueCallback(
        manager: IOHIDManagerRef,
        callback: IOHIDValueCallback,
        context: *mut c_void,
    );

    pub fn IOHIDManagerScheduleWithRunLoop(
        manager: IOHIDManagerRef,
        run_loop: CFRunLoopRef,
        run_loop_mode: CFStringRef,
    );

    pub fn IOHIDManagerUnscheduleFromRunLoop(
        manager: IOHIDManagerRef,
        run_loop: CFRunLoopRef,
        run_loop_mode: CFStringRef,
    );

    pub fn IOHIDManagerOpen(manager: IOHIDManagerRef, options: u32) -> IOReturn;
    pub fn IOHIDManagerClose(manager: IOHIDManagerRef, options: u32) -> IOReturn;

    // IOHIDDevice functions
    pub fn IOHIDDeviceGetProperty(device: IOHIDDeviceRef, key: CFStringRef) -> CFTypeRef;

    // IOHIDValue functions
    pub fn IOHIDValueGetElement(value: IOHIDValueRef) -> IOHIDElementRef;
    pub fn IOHIDValueGetIntegerValue(value: IOHIDValueRef) -> isize;
    pub fn IOHIDValueGetLength(value: IOHIDValueRef) -> usize;
    pub fn IOHIDValueGetBytePtr(value: IOHIDValueRef) -> *const u8;
    pub fn IOHIDValueGetTimeStamp(value: IOHIDValueRef) -> u64;

    // IOHIDElement functions
    pub fn IOHIDElementGetUsagePage(element: IOHIDElementRef) -> u32;
    pub fn IOHIDElementGetUsage(element: IOHIDElementRef) -> u32;
}

// HID usage pages and usages
pub const K_HID_PAGE_GENERIC_DESKTOP: u32 = 0x01;
pub const K_HID_PAGE_KEYBOARD: u32 = 0x07;
pub const K_HID_PAGE_BUTTON: u32 = 0x09;

pub const K_HID_USAGE_GD_MOUSE: u32 = 0x02;
pub const K_HID_USAGE_GD_KEYBOARD: u32 = 0x06;
pub const K_HID_USAGE_GD_KEYPAD: u32 = 0x07;
pub const K_HID_USAGE_GD_POINTER: u32 = 0x01;
pub const K_HID_USAGE_GD_X: u32 = 0x30;
pub const K_HID_USAGE_GD_Y: u32 = 0x31;
pub const K_HID_USAGE_GD_WHEEL: u32 = 0x38;

// IOReturn codes
pub const K_IO_RETURN_SUCCESS: IOReturn = 0;

/// CGEventFlags bit values
pub mod event_flags {
    pub const ALPHA_SHIFT: u64 = 0x00010000; // Caps Lock
    pub const SHIFT: u64 = 0x00020000;
    pub const CONTROL: u64 = 0x00040000;
    pub const ALTERNATE: u64 = 0x00080000; // Option
    pub const COMMAND: u64 = 0x00100000;
    pub const NUMERIC_PAD: u64 = 0x00200000;
    pub const HELP: u64 = 0x00400000;
    pub const SECONDARY_FN: u64 = 0x00800000; // Fn key
}

/// Create event mask for event types
pub fn cg_event_mask_bit(event_type: CGEventType) -> u64 {
    1 << (event_type as u64)
}

/// Safe wrapper for CFRelease
pub fn cf_release(cf: CFTypeRef) {
    if !cf.is_null() {
        unsafe { CFRelease(cf) };
    }
}
