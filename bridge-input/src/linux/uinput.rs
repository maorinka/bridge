//! Input injection via Linux uinput kernel module
//!
//! Creates a virtual keyboard + mouse device via /dev/uinput
//! and writes input_event structs to inject key/mouse/scroll events.

use bridge_common::{BridgeResult, BridgeError, InputEvent, KeyboardEvent, MouseMoveEvent, MouseButtonEvent, MouseScrollEvent};
use super::keymap::macos_to_linux_keycode;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
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
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_WHEEL: u16 = 0x08;
const REL_HWHEEL: u16 = 0x06;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;

// uinput ioctl numbers (from linux/uinput.h)
const UI_SET_EVBIT: libc::c_ulong = 0x40045564;   // _IOW('U', 100, int)
const UI_SET_KEYBIT: libc::c_ulong = 0x40045565;  // _IOW('U', 101, int)
const UI_SET_RELBIT: libc::c_ulong = 0x40045566;  // _IOW('U', 102, int)
const UI_SET_ABSBIT: libc::c_ulong = 0x40045567;  // _IOW('U', 103, int)
const UI_DEV_CREATE: libc::c_ulong = 0x5501;      // _IO('U', 1)
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;     // _IO('U', 2)

// Key range
const KEY_MAX: u16 = 0x2FF;

/// uinput_user_dev struct (from linux/uinput.h)
/// name[80] + id(bustype u16, vendor u16, product u16, version u16) + ff_effects_max u32
/// + absmax[64] i32 + absmin[64] i32 + absfuzz[64] i32 + absflat[64] i32
#[repr(C)]
struct UinputUserDev {
    name: [u8; 80],
    id_bustype: u16,
    id_vendor: u16,
    id_product: u16,
    id_version: u16,
    ff_effects_max: u32,
    absmax: [i32; 64],
    absmin: [i32; 64],
    absfuzz: [i32; 64],
    absflat: [i32; 64],
}

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

        let fd = file.as_raw_fd();

        unsafe {
            // Enable event types
            ioctl_check(libc::ioctl(fd, UI_SET_EVBIT, EV_KEY as libc::c_int), "UI_SET_EVBIT EV_KEY")?;
            ioctl_check(libc::ioctl(fd, UI_SET_EVBIT, EV_REL as libc::c_int), "UI_SET_EVBIT EV_REL")?;
            ioctl_check(libc::ioctl(fd, UI_SET_EVBIT, EV_ABS as libc::c_int), "UI_SET_EVBIT EV_ABS")?;
            ioctl_check(libc::ioctl(fd, UI_SET_EVBIT, EV_SYN as libc::c_int), "UI_SET_EVBIT EV_SYN")?;

            // Enable all standard keys (0..KEY_MAX)
            for key in 0..=KEY_MAX {
                let _ = libc::ioctl(fd, UI_SET_KEYBIT, key as libc::c_int);
            }

            // Enable mouse buttons
            ioctl_check(libc::ioctl(fd, UI_SET_KEYBIT, BTN_LEFT as libc::c_int), "BTN_LEFT")?;
            ioctl_check(libc::ioctl(fd, UI_SET_KEYBIT, BTN_RIGHT as libc::c_int), "BTN_RIGHT")?;
            ioctl_check(libc::ioctl(fd, UI_SET_KEYBIT, BTN_MIDDLE as libc::c_int), "BTN_MIDDLE")?;

            // Enable relative axes (mouse movement + scroll)
            ioctl_check(libc::ioctl(fd, UI_SET_RELBIT, REL_X as libc::c_int), "REL_X")?;
            ioctl_check(libc::ioctl(fd, UI_SET_RELBIT, REL_Y as libc::c_int), "REL_Y")?;
            ioctl_check(libc::ioctl(fd, UI_SET_RELBIT, REL_WHEEL as libc::c_int), "REL_WHEEL")?;
            ioctl_check(libc::ioctl(fd, UI_SET_RELBIT, REL_HWHEEL as libc::c_int), "REL_HWHEEL")?;

            // Enable absolute axes (for absolute mouse positioning)
            ioctl_check(libc::ioctl(fd, UI_SET_ABSBIT, ABS_X as libc::c_int), "ABS_X")?;
            ioctl_check(libc::ioctl(fd, UI_SET_ABSBIT, ABS_Y as libc::c_int), "ABS_Y")?;

            // Write uinput_user_dev struct
            let mut dev: UinputUserDev = std::mem::zeroed();
            let name = b"Bridge Virtual Input";
            dev.name[..name.len()].copy_from_slice(name);
            dev.id_bustype = 0x03; // BUS_USB
            dev.id_vendor = 0x1234;
            dev.id_product = 0x5678;
            dev.id_version = 1;

            // Absolute axis ranges (for absolute mouse positioning)
            dev.absmax[ABS_X as usize] = 3840; // screen width
            dev.absmax[ABS_Y as usize] = 2160; // screen height
            dev.absmin[ABS_X as usize] = 0;
            dev.absmin[ABS_Y as usize] = 0;

            let dev_bytes = std::slice::from_raw_parts(
                &dev as *const UinputUserDev as *const u8,
                std::mem::size_of::<UinputUserDev>(),
            );

            let mut f = &file;
            f.write_all(dev_bytes)
                .map_err(|e| BridgeError::Input(format!("Write uinput_user_dev: {}", e)))?;

            // Create the device
            ioctl_check(libc::ioctl(fd, UI_DEV_CREATE), "UI_DEV_CREATE")?;
        }

        // Small delay for device to register
        std::thread::sleep(std::time::Duration::from_millis(200));

        info!("uinput virtual input device created");

        Ok(Self {
            file,
            screen_width: 3840.0,
            screen_height: 2160.0,
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
            let x = (event.x * self.screen_width) as i32;
            let y = (event.y * self.screen_height) as i32;
            self.write_event(EV_ABS, ABS_X, x)?;
            self.write_event(EV_ABS, ABS_Y, y)?;
        } else {
            self.write_event(EV_REL, REL_X, event.delta_x as i32)?;
            self.write_event(EV_REL, REL_Y, event.delta_y as i32)?;
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

impl Drop for InputInjector {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY);
        }
        info!("uinput device destroyed");
    }
}

unsafe fn ioctl_check(ret: libc::c_int, name: &str) -> BridgeResult<()> {
    if ret < 0 {
        Err(BridgeError::Input(format!("ioctl {} failed: {}", name, std::io::Error::last_os_error())))
    } else {
        Ok(())
    }
}
