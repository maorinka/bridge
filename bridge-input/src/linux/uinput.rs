//! Input injection via Linux uinput kernel module
//!
//! Creates a virtual keyboard + mouse device via /dev/uinput
//! and writes input_event structs to inject key/mouse/scroll events.

use bridge_common::{BridgeResult, BridgeError, InputEvent, KeyboardEvent, MouseMoveEvent, MouseButtonEvent, MouseScrollEvent};
use super::keymap::macos_to_linux_keycode;
use std::fs::{File, OpenOptions};
use std::io::Write;
use tracing::{debug, info, trace, warn};

// Linux input event type constants
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0x00;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const REL_WHEEL: u16 = 0x08;
const REL_HWHEEL: u16 = 0x06;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;

#[repr(C)]
struct LinuxInputEvent {
    tv_sec: i64,
    tv_usec: i64,
    type_: u16,
    code: u16,
    value: i32,
}

pub struct InputInjector {
    file: File,
    screen_width: f64,
    screen_height: f64,
}

unsafe impl Send for InputInjector {}

impl InputInjector {
    pub fn new() -> BridgeResult<Self> {
        let file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .map_err(|e| BridgeError::Input(format!(
                "Cannot open /dev/uinput: {}. Add user to 'input' group or run as root.", e
            )))?;

        // TODO: Full uinput device setup via ioctl:
        // UI_SET_EVBIT for EV_KEY, EV_REL, EV_ABS
        // UI_SET_KEYBIT for all keys we support
        // UI_SET_RELBIT for REL_WHEEL, REL_HWHEEL
        // UI_SET_ABSBIT for ABS_X, ABS_Y
        // Write uinput_user_dev struct with device name + abs ranges
        // UI_DEV_CREATE ioctl
        // These ioctls require testing on actual Linux hardware.

        info!("uinput injector created (device setup requires Linux)");

        Ok(Self {
            file,
            screen_width: 1920.0,
            screen_height: 1080.0,
        })
    }

    pub fn inject(&mut self, event: &InputEvent) -> BridgeResult<()> {
        match event {
            InputEvent::Keyboard(e) => self.inject_keyboard(e),
            InputEvent::MouseMove(e) => self.inject_mouse_move(e),
            InputEvent::MouseButton(e) => self.inject_mouse_button(e),
            InputEvent::MouseScroll(e) => self.inject_scroll(e),
            InputEvent::RawHid(_) => {
                warn!("Raw HID injection not supported via uinput");
                Ok(())
            }
        }
    }

    fn write_event(&mut self, type_: u16, code: u16, value: i32) -> BridgeResult<()> {
        let ev = LinuxInputEvent {
            tv_sec: 0,
            tv_usec: 0,
            type_,
            code,
            value,
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &ev as *const LinuxInputEvent as *const u8,
                std::mem::size_of::<LinuxInputEvent>(),
            )
        };
        self.file.write_all(bytes)
            .map_err(|e| BridgeError::Input(format!("uinput write: {}", e)))
    }

    fn sync(&mut self) -> BridgeResult<()> {
        self.write_event(EV_SYN, SYN_REPORT, 0)
    }

    fn inject_keyboard(&mut self, event: &KeyboardEvent) -> BridgeResult<()> {
        let linux_key = macos_to_linux_keycode(event.key_code);
        if linux_key == 0 {
            debug!("Unknown macOS keycode: 0x{:02X}", event.key_code);
            return Ok(());
        }
        self.write_event(EV_KEY, linux_key, if event.is_pressed { 1 } else { 0 })?;
        self.sync()?;
        trace!("Key: linux={} pressed={}", linux_key, event.is_pressed);
        Ok(())
    }

    fn inject_mouse_move(&mut self, event: &MouseMoveEvent) -> BridgeResult<()> {
        if event.is_absolute {
            self.write_event(EV_ABS, ABS_X, (event.x * self.screen_width) as i32)?;
            self.write_event(EV_ABS, ABS_Y, (event.y * self.screen_height) as i32)?;
        } else {
            self.write_event(EV_REL, 0x00, event.delta_x as i32)?; // REL_X
            self.write_event(EV_REL, 0x01, event.delta_y as i32)?; // REL_Y
        }
        self.sync()
    }

    fn inject_mouse_button(&mut self, event: &MouseButtonEvent) -> BridgeResult<()> {
        let btn = match event.button {
            0 => BTN_LEFT,
            1 => BTN_RIGHT,
            2 => BTN_MIDDLE,
            n => BTN_LEFT + n as u16,
        };
        self.write_event(EV_KEY, btn, if event.is_pressed { 1 } else { 0 })?;
        self.sync()
    }

    fn inject_scroll(&mut self, event: &MouseScrollEvent) -> BridgeResult<()> {
        if event.delta_y.abs() > 0.01 {
            self.write_event(EV_REL, REL_WHEEL, event.delta_y as i32)?;
        }
        if event.delta_x.abs() > 0.01 {
            self.write_event(EV_REL, REL_HWHEEL, event.delta_x as i32)?;
        }
        self.sync()
    }

    pub fn warp_cursor(&self, _x: f64, _y: f64) -> BridgeResult<()> { Ok(()) }
    pub fn set_cursor_captured(&self, _captured: bool) -> BridgeResult<()> { Ok(()) }
}
