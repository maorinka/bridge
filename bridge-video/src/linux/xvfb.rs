//! Virtual display via Xvfb for headless Linux

use bridge_common::{BridgeResult, BridgeError};
use std::process::{Command, Child};
use tracing::info;

/// A running Xvfb virtual display.
///
/// The Xvfb process is killed and reaped when this value is dropped.
pub struct VirtualDisplay {
    child: Option<Child>,
    display_num: u32,
    width: u32,
    height: u32,
}

impl VirtualDisplay {
    /// Start an Xvfb virtual display and set the `DISPLAY` environment variable.
    ///
    /// `refresh_rate` is accepted for API symmetry but is not forwarded to
    /// Xvfb (which doesn't use a refresh-rate argument).
    pub fn new(width: u32, height: u32, _refresh_rate: u32) -> BridgeResult<Self> {
        let display_num = 99u32;
        let display_str = format!(":{}", display_num);
        let resolution = format!("{}x{}x24", width, height);

        info!("Starting Xvfb on {} at {}", display_str, resolution);

        let child = Command::new("Xvfb")
            .arg(&display_str)
            .arg("-screen")
            .arg("0")
            .arg(&resolution)
            .arg("-ac") // disable access control (allow all connections)
            .spawn()
            .map_err(|e| {
                BridgeError::Video(format!(
                    "Failed to start Xvfb: {}. Install with: sudo apt install xvfb",
                    e
                ))
            })?;

        // Give Xvfb a moment to initialise before accepting connections
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Expose the virtual display to child processes
        // SAFETY: single-threaded at this point; acceptable for CLI/daemon usage.
        #[allow(deprecated)]
        std::env::set_var("DISPLAY", &display_str);

        Ok(Self {
            child: Some(child),
            display_num,
            width,
            height,
        })
    }

    /// Return the X display number (e.g. `99` for `:99`).
    pub fn display_id(&self) -> u32 {
        self.display_num
    }

    /// Return the configured display width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Return the configured display height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Returns `true` if `Xvfb` is available on `$PATH`.
pub fn is_virtual_display_supported() -> bool {
    Command::new("which")
        .arg("Xvfb")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_virtual_display_supported_does_not_panic() {
        // We do not assert a specific result because the test environment may
        // or may not have Xvfb installed.
        let _ = is_virtual_display_supported();
    }
}
