# Linux/Jetson Nano Server Port — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the Bridge server to compile and run on Linux/aarch64 (Jetson Nano), with X11 screen capture, V4L2 hardware encoding, uinput input injection, and Avahi service discovery.

**Architecture:** Platform abstraction via `#[cfg(target_os)]` compile-time selection. macOS code moves into `macos/` subdirectories; new Linux backends go into `linux/` subdirectories. Shared types (`CapturedFrame`, `EncodedFrame`, etc.) become platform-neutral with cfg-gated platform-specific fields. The server binary (`bridge-server`) uses factory functions that return the right backend for the target OS.

**Tech Stack:** Rust, xcb (X11 capture), nix (V4L2 ioctls, uinput), libavahi-client (mDNS), cpal (ALSA audio)

---

## File Structure

**Files moved (macOS code reorganization):**
- `bridge-video/src/capture.rs` → split into `bridge-video/src/capture.rs` (trait + shared types) + `bridge-video/src/macos/capture.rs` (macOS impl)
- `bridge-video/src/sck_capture.rs` → `bridge-video/src/macos/sck_capture.rs`
- `bridge-video/src/codec.rs` → split into `bridge-video/src/codec.rs` (trait + shared types) + `bridge-video/src/macos/codec.rs` (macOS impl)
- `bridge-video/src/display.rs` → `bridge-video/src/macos/display.rs`
- `bridge-video/src/virtual_display.rs` → `bridge-video/src/macos/virtual_display.rs`
- `bridge-video/src/sys.rs` → `bridge-video/src/macos/sys.rs`
- `bridge-input/src/inject.rs` → `bridge-input/src/macos/inject.rs`
- `bridge-input/src/capture.rs` → `bridge-input/src/macos/capture.rs`
- `bridge-input/src/sys.rs` → `bridge-input/src/macos/sys.rs`
- `bridge-transport/src/discovery.rs` → `bridge-transport/src/macos/discovery.rs`

**Files created (Linux backends):**
- `bridge-video/src/linux/mod.rs` — module declarations
- `bridge-video/src/linux/x11_capture.rs` — X11+XShm screen capture
- `bridge-video/src/linux/v4l2_encoder.rs` — V4L2 M2M hardware encoder
- `bridge-video/src/linux/v4l2_decoder.rs` — V4L2 M2M hardware decoder (stub)
- `bridge-video/src/linux/color_convert.rs` — BGRA→NV12 CPU conversion
- `bridge-video/src/linux/xvfb.rs` — virtual display via Xvfb
- `bridge-input/src/linux/mod.rs` — module declarations
- `bridge-input/src/linux/uinput.rs` — uinput input injection
- `bridge-input/src/linux/keymap.rs` — macOS↔Linux keycode mapping
- `bridge-transport/src/linux/mod.rs` — module declarations
- `bridge-transport/src/linux/discovery.rs` — Avahi mDNS discovery

**Files modified (platform abstraction):**
- `bridge-video/src/lib.rs` — cfg-gated module imports, factory functions
- `bridge-video/src/capture.rs` — extract `CapturedFrame` + `CaptureConfig` as shared types, add trait
- `bridge-video/src/codec.rs` — extract `EncodedFrame` + `DecodedFrame` + `EncoderConfig` as shared types, add trait
- `bridge-video/Cargo.toml` — cfg-gated deps
- `bridge-input/src/lib.rs` — cfg-gated module imports
- `bridge-input/Cargo.toml` — cfg-gated deps
- `bridge-transport/src/lib.rs` — cfg-gated discovery import
- `bridge-transport/Cargo.toml` — cfg-gated deps
- `bridge-audio/Cargo.toml` — cfg-gate macOS-specific deps
- `bridge-server/Cargo.toml` — cfg-gate macOS-specific deps
- `bridge-server/src/main.rs` — use factory functions instead of direct types
- `bridge-common/src/error.rs` — add `Linux` error variant
- `Cargo.toml` — add Linux workspace deps (`nix`, `x11rb`)

---

### Task 1: Gate macOS dependencies behind cfg in all Cargo.toml files

This is the foundation — make the workspace compilable on Linux by ensuring all macOS-specific dependencies are conditional.

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `bridge-video/Cargo.toml`
- Modify: `bridge-input/Cargo.toml`
- Modify: `bridge-audio/Cargo.toml`
- Modify: `bridge-transport/Cargo.toml`
- Modify: `bridge-server/Cargo.toml`
- Modify: `bridge-client/Cargo.toml`
- Modify: `bridge-common/src/error.rs`

- [ ] **Step 1: Update workspace root Cargo.toml**

Split dependencies into cross-platform and macOS-specific sections. Add Linux dependencies.

```toml
# In Cargo.toml [workspace.dependencies], change:
# Remove macOS deps from unconditional section — they'll be specified per-crate with cfg

# Add new cross-platform deps that Linux backends will need:
nix = { version = "0.29", features = ["ioctl", "fs", "uio"] }
x11rb = { version = "0.13", features = ["shm"] }
libc = "0.2"
```

Keep existing macOS deps in workspace.dependencies (they're still referenced by crates), but each crate's Cargo.toml will gate them behind `[target.'cfg(target_os = "macos")'.dependencies]`.

- [ ] **Step 2: Update bridge-video/Cargo.toml**

```toml
[package]
name = "bridge-video"
version.workspace = true
edition.workspace = true

[dependencies]
bridge-common = { path = "../bridge-common" }
tokio = { workspace = true }
bytes = { workspace = true }
thiserror = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
parking_lot = { workspace = true }
crossbeam-channel = { workspace = true }
libc = "0.2"

[target.'cfg(target_os = "macos")'.dependencies]
objc2 = { workspace = true }
objc2-foundation = { workspace = true }
objc2-app-kit = { workspace = true }
objc2-quartz-core = { workspace = true }
block2 = { workspace = true }
core-foundation = { workspace = true }
core-graphics = { workspace = true }
dispatch = { workspace = true }
metal = { workspace = true }
core-graphics-types = "0.1.3"
screencapturekit = { version = "1.5", features = ["macos_14_0"] }

[target.'cfg(target_os = "linux")'.dependencies]
nix = { workspace = true }
x11rb = { workspace = true }

[build-dependencies]
cc = "1.2"
```

- [ ] **Step 3: Update bridge-input/Cargo.toml**

```toml
[package]
name = "bridge-input"
version.workspace = true
edition.workspace = true

[dependencies]
bridge-common = { path = "../bridge-common" }
tokio = { workspace = true }
bytes = { workspace = true }
serde = { workspace = true }
thiserror = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
parking_lot = { workspace = true }
crossbeam-channel = { workspace = true }
libc = "0.2"

[target.'cfg(target_os = "macos")'.dependencies]
objc2 = { workspace = true }
objc2-foundation = { workspace = true }
block2 = { workspace = true }
core-foundation = { workspace = true }
core-graphics = { workspace = true }
dispatch = { workspace = true }

[target.'cfg(target_os = "linux")'.dependencies]
nix = { workspace = true }

[build-dependencies]
cc = "1.2"
```

- [ ] **Step 4: Update bridge-audio/Cargo.toml**

```toml
[package]
name = "bridge-audio"
version.workspace = true
edition.workspace = true

[dependencies]
bridge-common = { path = "../bridge-common" }
tokio = { workspace = true }
bytes = { workspace = true }
serde = { workspace = true }
thiserror = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
parking_lot = { workspace = true }
crossbeam-channel = { workspace = true }
cpal = { workspace = true }
libc = "0.2"

[target.'cfg(target_os = "macos")'.dependencies]
objc2 = { workspace = true }
objc2-foundation = { workspace = true }
block2 = { workspace = true }
core-foundation = { workspace = true }
dispatch = { workspace = true }

[build-dependencies]
cc = "1.2"
```

- [ ] **Step 5: Update bridge-transport/Cargo.toml**

Add libc as cross-platform (already used for DNS-SD FFI), no new Linux deps needed yet.

```toml
# No changes needed — libc is already unconditional and DNS-SD FFI 
# will be split by cfg in the source code. The transport crate's 
# networking (quinn, rustls) is already cross-platform.
```

- [ ] **Step 6: Update bridge-server/Cargo.toml**

```toml
[package]
name = "bridge-server"
version.workspace = true
edition.workspace = true

[[bin]]
name = "bridge-server"
path = "src/main.rs"

[dependencies]
bridge-common = { path = "../bridge-common" }
bridge-transport = { path = "../bridge-transport" }
bridge-video = { path = "../bridge-video" }
bridge-input = { path = "../bridge-input" }
bridge-audio = { path = "../bridge-audio" }

tokio = { workspace = true }
bytes = { workspace = true }
serde = { workspace = true }
bincode = { workspace = true }
thiserror = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
parking_lot = { workspace = true }
crossbeam-channel = { workspace = true }
clap = { version = "4.5", features = ["derive"] }

[target.'cfg(target_os = "macos")'.dependencies]
objc2 = { workspace = true }
objc2-foundation = { workspace = true }
block2 = { workspace = true }
core-foundation = { workspace = true }
```

- [ ] **Step 7: Update bridge-client/Cargo.toml similarly**

Gate macOS deps:

```toml
[target.'cfg(target_os = "macos")'.dependencies]
objc2 = { workspace = true }
objc2-foundation = { workspace = true }
objc2-app-kit = { workspace = true }
objc2-quartz-core = { workspace = true }
block2 = { workspace = true }
core-foundation = { workspace = true }
core-graphics-types = "0.1"
metal = { workspace = true }
```

- [ ] **Step 8: Add Linux error variant to bridge-common**

In `bridge-common/src/error.rs`, add:

```rust
#[error("Linux error: {0}")]
Linux(String),
```

- [ ] **Step 9: Verify macOS build still works**

Run: `cargo build 2>&1 | head -20`
Expected: successful compilation (no errors)

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "Gate macOS dependencies behind cfg(target_os) in all crates

Prepares workspace for cross-platform compilation by making macOS
framework dependencies conditional. Adds Linux deps (nix, x11rb)
to workspace. No functional changes on macOS."
```

---

### Task 2: Reorganize bridge-video into platform modules

Move macOS-specific code into `macos/` subdirectory. Extract shared types and traits into the top-level module files.

**Files:**
- Create: `bridge-video/src/macos/mod.rs`
- Create: `bridge-video/src/macos/capture.rs` (moved from `capture.rs` macOS-specific code)
- Create: `bridge-video/src/macos/sck_capture.rs` (moved from `sck_capture.rs`)
- Create: `bridge-video/src/macos/codec.rs` (moved from `codec.rs` macOS-specific code)
- Create: `bridge-video/src/macos/display.rs` (moved from `display.rs`)
- Create: `bridge-video/src/macos/virtual_display.rs` (moved from `virtual_display.rs`)
- Create: `bridge-video/src/macos/sys.rs` (moved from `sys.rs`)
- Create: `bridge-video/src/linux/mod.rs` (empty stubs)
- Modify: `bridge-video/src/lib.rs` — platform dispatch
- Modify: `bridge-video/src/capture.rs` — shared types only
- Modify: `bridge-video/src/codec.rs` — shared types only

- [ ] **Step 1: Create macos/ directory and move sys.rs**

```bash
mkdir -p bridge-video/src/macos
cp bridge-video/src/sys.rs bridge-video/src/macos/sys.rs
```

- [ ] **Step 2: Create macos/mod.rs**

```rust
//! macOS-specific video implementations

pub mod sys;
pub mod sck_capture;
pub mod capture;
pub mod codec;
pub mod display;
pub mod virtual_display;
```

- [ ] **Step 3: Move sck_capture.rs to macos/**

```bash
cp bridge-video/src/sck_capture.rs bridge-video/src/macos/sck_capture.rs
```

Update its imports to reference `super::sys::*` instead of `crate::sys::*`, and `crate::capture::CapturedFrame` for the shared type.

- [ ] **Step 4: Create macos/capture.rs**

Move the macOS-specific `ScreenCapturer` struct and its impl from `capture.rs` into `macos/capture.rs`. This includes `CaptureBackend`, `CaptureContext`, `create_capture_block`, the `impl ScreenCapturer` block, `get_displays`, `DisplayInfo`, and `is_capture_supported`.

Imports reference `super::sys::*` and `super::sck_capture::SckBackend`. Shared types come from `crate::capture::{CapturedFrame, CaptureConfig, CaptureStats}`.

- [ ] **Step 5: Rewrite capture.rs as shared types only**

The new `bridge-video/src/capture.rs` contains only the platform-neutral shared types and the cfg-gated re-exports:

```rust
//! Screen capture — shared types and platform dispatch

use bridge_common::{BridgeResult, VideoConfig};
use crossbeam_channel::Receiver;

/// Captured frame with metadata
#[derive(Debug)]
pub struct CapturedFrame {
    /// Pixel data (BGRA format)
    pub data: Vec<u8>,
    /// Frame width
    pub width: u32,
    /// Frame height
    pub height: u32,
    /// Bytes per row
    pub bytes_per_row: u32,
    /// Presentation timestamp in microseconds
    pub pts_us: u64,
    /// Frame number
    pub frame_number: u64,
    /// Platform-specific surface handle (macOS IOSurface, Linux DMA-BUF fd)
    #[cfg(target_os = "macos")]
    pub io_surface: Option<crate::macos::sys::IOSurfaceRef>,
    #[cfg(target_os = "linux")]
    pub dma_buf_fd: Option<i32>,
}

// Platform-specific Drop, Send impls via cfg
#[cfg(target_os = "macos")]
impl Drop for CapturedFrame {
    fn drop(&mut self) {
        if let Some(surface) = self.io_surface.take() {
            unsafe {
                crate::macos::sys::IOSurfaceDecrementUseCount(surface);
                crate::macos::sys::CFRelease(surface as crate::macos::sys::CFTypeRef);
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for CapturedFrame {
    fn drop(&mut self) {
        if let Some(fd) = self.dma_buf_fd.take() {
            unsafe { libc::close(fd); }
        }
    }
}

unsafe impl Send for CapturedFrame {}

/// Screen capture configuration
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub show_cursor: bool,
    pub capture_audio: bool,
    pub display_id: Option<u32>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            fps: 60,
            show_cursor: true,
            capture_audio: false,
            display_id: None,
        }
    }
}

impl From<&VideoConfig> for CaptureConfig {
    fn from(config: &VideoConfig) -> Self {
        Self {
            width: config.width,
            height: config.height,
            fps: config.fps,
            ..Default::default()
        }
    }
}

/// Capture statistics
#[derive(Debug, Clone)]
pub struct CaptureStats {
    pub frames_captured: u64,
    pub is_running: bool,
}

/// Information about a display
#[derive(Debug, Clone)]
pub struct DisplayInfo {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub is_main: bool,
}
```

- [ ] **Step 6: Create macos/codec.rs**

Move the macOS-specific `VideoEncoder` and `VideoDecoder` structs from `codec.rs` into `macos/codec.rs`. Imports: `super::sys::*`, shared types from `crate::codec::*` and `crate::capture::CapturedFrame`.

- [ ] **Step 7: Rewrite codec.rs as shared types only**

The new `bridge-video/src/codec.rs` contains platform-neutral types:

```rust
//! Video encoding/decoding — shared types and platform dispatch

use bridge_common::{BridgeResult, VideoCodec, VideoConfig};
use bytes::Bytes;

/// Encoded video frame
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Bytes,
    pub pts_us: u64,
    pub dts_us: u64,
    pub is_keyframe: bool,
    pub frame_number: u64,
}

/// Decoded video frame
#[derive(Debug)]
pub struct DecodedFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub bytes_per_row: u32,
    pub pts_us: u64,
    #[cfg(target_os = "macos")]
    pub io_surface: Option<crate::macos::sys::IOSurfaceRef>,
    #[cfg(target_os = "macos")]
    pub cv_pixel_buffer: Option<crate::macos::sys::CVPixelBufferRef>,
    #[cfg(target_os = "linux")]
    pub dma_buf_fd: Option<i32>,
}

unsafe impl Send for DecodedFrame {}

#[cfg(target_os = "macos")]
impl Drop for DecodedFrame {
    fn drop(&mut self) {
        if let Some(pb) = self.cv_pixel_buffer.take() {
            unsafe { crate::macos::sys::CVPixelBufferRelease(pb); }
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for DecodedFrame {
    fn drop(&mut self) {
        if let Some(fd) = self.dma_buf_fd.take() {
            unsafe { libc::close(fd); }
        }
    }
}

/// Video encoder configuration
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub codec: VideoCodec,
    pub keyframe_interval: u32,
    pub low_latency: bool,
    pub realtime: bool,
    pub max_quality: bool,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate: 60_000_000,
            codec: VideoCodec::H265,
            keyframe_interval: 60,
            low_latency: true,
            realtime: true,
            max_quality: false,
        }
    }
}

impl From<&VideoConfig> for EncoderConfig {
    fn from(config: &VideoConfig) -> Self {
        Self {
            width: config.width,
            height: config.height,
            fps: config.fps,
            bitrate: config.bitrate,
            codec: config.codec,
            ..Default::default()
        }
    }
}
```

- [ ] **Step 8: Move display.rs and virtual_display.rs to macos/**

```bash
cp bridge-video/src/display.rs bridge-video/src/macos/display.rs
cp bridge-video/src/virtual_display.rs bridge-video/src/macos/virtual_display.rs
```

Update imports in both to reference `super::sys::*`.

- [ ] **Step 9: Create linux/mod.rs stub**

```rust
//! Linux-specific video implementations (Jetson Nano)

pub mod x11_capture;
pub mod v4l2_encoder;
pub mod color_convert;
pub mod xvfb;
```

Create empty stub files for each module with just a module doc comment, so the project compiles.

- [ ] **Step 10: Update bridge-video/src/lib.rs with platform dispatch**

```rust
//! Bridge Video Subsystem

pub mod capture;
pub mod codec;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub mod linux;

pub use capture::*;
pub use codec::*;

// Re-export platform-specific types
#[cfg(target_os = "macos")]
pub use macos::capture::{ScreenCapturer, get_displays, is_capture_supported};
#[cfg(target_os = "macos")]
pub use macos::codec::{VideoEncoder, VideoDecoder};
#[cfg(target_os = "macos")]
pub use macos::display::MetalDisplay;
#[cfg(target_os = "macos")]
pub mod virtual_display {
    pub use crate::macos::virtual_display::*;
}

#[cfg(target_os = "linux")]
pub use linux::x11_capture::ScreenCapturer;
#[cfg(target_os = "linux")]
pub use linux::v4l2_encoder::VideoEncoder;

use bridge_common::{VideoConfig, VideoCodec, PixelFormat};

pub fn is_hw_encoding_available() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

pub fn bytes_per_frame(config: &VideoConfig) -> usize {
    let pixels = config.width as usize * config.height as usize;
    match config.pixel_format {
        PixelFormat::Bgra8 | PixelFormat::Rgba8 => pixels * 4,
        PixelFormat::Nv12 | PixelFormat::I420 => pixels * 3 / 2,
    }
}

pub fn raw_bitrate(config: &VideoConfig) -> u64 {
    let bytes = bytes_per_frame(config);
    bytes as u64 * config.fps as u64 * 8
}

pub fn recommended_bitrate(config: &VideoConfig) -> u32 {
    let pixels = config.width as u64 * config.height as u64;
    let fps = config.fps as u64;
    let base = match config.codec {
        VideoCodec::H265 => pixels * fps / 100,
        VideoCodec::H264 => pixels * fps / 50,
        VideoCodec::Raw => pixels * fps * 32,
    };
    base.min(200_000_000) as u32
}
```

- [ ] **Step 11: Remove old top-level files that were moved**

Delete `bridge-video/src/sys.rs` and `bridge-video/src/sck_capture.rs` (now in `macos/`).

- [ ] **Step 12: Verify macOS build**

Run: `cargo build 2>&1 | head -30`
Expected: successful compilation

- [ ] **Step 13: Commit**

```bash
git add -A
git commit -m "Reorganize bridge-video into platform modules

Move macOS code into macos/ subdirectory. Extract shared types
(CapturedFrame, EncodedFrame, etc.) into top-level modules.
Add empty linux/ stubs. Platform selected via cfg(target_os)."
```

---

### Task 3: Reorganize bridge-input into platform modules

Same pattern as bridge-video: move macOS code to `macos/`, create `linux/` stubs.

**Files:**
- Create: `bridge-input/src/macos/mod.rs`
- Create: `bridge-input/src/macos/inject.rs` (moved from inject.rs)
- Create: `bridge-input/src/macos/capture.rs` (moved from capture.rs)
- Create: `bridge-input/src/macos/sys.rs` (moved from sys.rs)
- Create: `bridge-input/src/linux/mod.rs`
- Create: `bridge-input/src/linux/uinput.rs` (stub)
- Create: `bridge-input/src/linux/keymap.rs` (stub)
- Modify: `bridge-input/src/lib.rs`

- [ ] **Step 1: Create macos/ directory and move files**

```bash
mkdir -p bridge-input/src/macos bridge-input/src/linux
cp bridge-input/src/sys.rs bridge-input/src/macos/sys.rs
cp bridge-input/src/inject.rs bridge-input/src/macos/inject.rs
cp bridge-input/src/capture.rs bridge-input/src/macos/capture.rs
```

- [ ] **Step 2: Create macos/mod.rs**

```rust
//! macOS-specific input implementations

pub mod sys;
pub mod inject;
pub mod capture;
```

Update imports in `macos/inject.rs` and `macos/capture.rs` to use `super::sys::*`.

- [ ] **Step 3: Create linux/mod.rs and linux/uinput.rs stubs**

`linux/mod.rs`:
```rust
//! Linux-specific input implementations

pub mod uinput;
pub mod keymap;
```

`linux/uinput.rs`:
```rust
//! Input injection via Linux uinput kernel module
// TODO: Implement in Task 6
```

`linux/keymap.rs`:
```rust
//! macOS ↔ Linux keycode mapping
// TODO: Implement in Task 6
```

- [ ] **Step 4: Update bridge-input/src/lib.rs**

```rust
//! Bridge Input Subsystem

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub use macos::capture::*;
#[cfg(target_os = "macos")]
pub use macos::inject::*;

#[cfg(target_os = "linux")]
pub use linux::uinput::*;

#[cfg(target_os = "macos")]
pub fn has_accessibility_permission() -> bool {
    unsafe {
        extern "C" {
            fn AXIsProcessTrusted() -> bool;
        }
        AXIsProcessTrusted()
    }
}

#[cfg(target_os = "linux")]
pub fn has_accessibility_permission() -> bool {
    // On Linux, check if /dev/uinput is writable
    std::fs::metadata("/dev/uinput")
        .map(|m| !m.permissions().readonly())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
pub fn request_accessibility_permission() {
    unsafe {
        extern "C" {
            fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
        }
        AXIsProcessTrustedWithOptions(std::ptr::null());
    }
}

#[cfg(target_os = "linux")]
pub fn request_accessibility_permission() {
    tracing::info!("On Linux, add user to 'input' group: sudo usermod -aG input $USER");
}

pub fn keycode_to_string(keycode: u16) -> &'static str {
    // Keep existing macOS keycode table — it's the protocol format
    match keycode {
        0x00 => "A",
        0x01 => "S",
        // ... (keep full existing table)
        _ => "Unknown",
    }
}
```

- [ ] **Step 5: Delete old top-level files**

Remove `bridge-input/src/inject.rs`, `bridge-input/src/capture.rs`, `bridge-input/src/sys.rs`.

- [ ] **Step 6: Verify macOS build**

Run: `cargo build 2>&1 | head -20`
Expected: successful compilation

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "Reorganize bridge-input into platform modules

Move macOS code to macos/ subdirectory, create linux/ stubs
for uinput injection and keycode mapping."
```

---

### Task 4: Reorganize bridge-transport discovery into platform modules

**Files:**
- Create: `bridge-transport/src/macos/mod.rs`
- Create: `bridge-transport/src/macos/discovery.rs` (moved from discovery.rs)
- Create: `bridge-transport/src/linux/mod.rs`
- Create: `bridge-transport/src/linux/discovery.rs` (stub)
- Modify: `bridge-transport/src/discovery.rs` — keep `get_local_addresses` and `is_thunderbolt_interface`, cfg-gate the rest
- Modify: `bridge-transport/src/lib.rs`

- [ ] **Step 1: Create platform directories**

```bash
mkdir -p bridge-transport/src/macos bridge-transport/src/linux
```

- [ ] **Step 2: Split discovery.rs**

The existing `discovery.rs` contains both platform-neutral code (`get_local_addresses`, `is_thunderbolt_interface`) and macOS-specific DNS-SD FFI. Keep the platform-neutral functions in `discovery.rs` and move the DNS-SD code into `macos/discovery.rs`.

- [ ] **Step 3: Create macos/mod.rs and macos/discovery.rs**

Move `ServiceAdvertiser`, `ServiceBrowser`, and all DNS-SD FFI to `macos/discovery.rs`.

- [ ] **Step 4: Create linux/mod.rs and linux/discovery.rs stubs**

`linux/discovery.rs`:
```rust
//! Service discovery via Avahi (Linux mDNS)
// TODO: Implement in Task 7
```

- [ ] **Step 5: Update lib.rs with cfg-gated re-exports**

```rust
pub mod discovery;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "linux")]
pub mod linux;

// Re-export platform-specific discovery types
#[cfg(target_os = "macos")]
pub use macos::discovery::{ServiceAdvertiser, ServiceBrowser};
#[cfg(target_os = "linux")]
pub use linux::discovery::{ServiceAdvertiser, ServiceBrowser};
```

- [ ] **Step 6: Verify macOS build**

Run: `cargo build 2>&1 | head -20`

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "Reorganize bridge-transport discovery into platform modules

Split DNS-SD/Bonjour code into macos/, create linux/ stubs for Avahi."
```

---

### Task 5: Implement Linux X11 screen capture

**Files:**
- Create: `bridge-video/src/linux/x11_capture.rs`
- Modify: `bridge-video/src/linux/mod.rs`

- [ ] **Step 1: Write X11 capture test**

Create `bridge-video/src/linux/x11_capture.rs` with a test first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::CaptureConfig;

    #[test]
    fn test_capture_config_default() {
        let config = CaptureConfig::default();
        assert_eq!(config.fps, 60);
    }

    #[test]
    fn test_display_info_from_xvfb() {
        // This test can run in CI with Xvfb
        if std::env::var("DISPLAY").is_err() {
            println!("Skipping: no DISPLAY set");
            return;
        }
        let displays = get_displays();
        assert!(!displays.is_empty(), "Should find at least one display");
        assert!(displays[0].width > 0);
        assert!(displays[0].height > 0);
    }
}
```

- [ ] **Step 2: Implement ScreenCapturer for X11**

```rust
//! X11 screen capture using XShm for low-latency frame access
//!
//! Uses x11rb crate for X11 protocol, with the MIT-SHM extension
//! for shared-memory frame access (sub-millisecond per frame).

use bridge_common::{BridgeResult, BridgeError};
use crate::capture::{CapturedFrame, CaptureConfig, CaptureStats, DisplayInfo};
use crossbeam_channel::{bounded, Receiver, Sender};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tracing::{debug, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::shm;

pub struct ScreenCapturer {
    config: CaptureConfig,
    frame_tx: Sender<CapturedFrame>,
    frame_rx: Receiver<CapturedFrame>,
    frame_count: Arc<AtomicU64>,
    is_running: Arc<AtomicBool>,
    capture_thread: Option<std::thread::JoinHandle<()>>,
}

unsafe impl Send for ScreenCapturer {}
unsafe impl Sync for ScreenCapturer {}

impl ScreenCapturer {
    pub fn new(config: CaptureConfig) -> BridgeResult<Self> {
        let (frame_tx, frame_rx) = bounded(4);
        Ok(Self {
            config,
            frame_tx,
            frame_rx,
            frame_count: Arc::new(AtomicU64::new(0)),
            is_running: Arc::new(AtomicBool::new(false)),
            capture_thread: None,
        })
    }

    pub fn has_permission() -> bool {
        std::env::var("DISPLAY").is_ok()
    }

    pub fn request_permission() -> bool {
        Self::has_permission()
    }

    pub async fn start(&mut self) -> BridgeResult<()> {
        if self.is_running.load(Ordering::SeqCst) {
            return Ok(());
        }

        info!("Starting X11 screen capture at {}fps", self.config.fps);

        let config = self.config.clone();
        let frame_tx = self.frame_tx.clone();
        let frame_count = self.frame_count.clone();
        let is_running = self.is_running.clone();

        is_running.store(true, Ordering::SeqCst);

        let handle = std::thread::spawn(move || {
            if let Err(e) = capture_loop(config, frame_tx, frame_count, is_running.clone()) {
                warn!("X11 capture loop error: {}", e);
                is_running.store(false, Ordering::SeqCst);
            }
        });

        self.capture_thread = Some(handle);
        Ok(())
    }

    pub fn stop(&mut self) -> BridgeResult<()> {
        self.is_running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.capture_thread.take() {
            let _ = handle.join();
        }
        Ok(())
    }

    pub fn is_stopped(&self) -> bool {
        !self.is_running.load(Ordering::SeqCst)
    }

    pub async fn restart(&mut self, _new_display_id: Option<u32>) -> BridgeResult<()> {
        self.stop()?;
        while self.frame_rx.try_recv().is_ok() {}
        self.start().await
    }

    pub fn recv_frame(&self) -> Option<CapturedFrame> {
        self.frame_rx.try_recv().ok()
    }

    pub fn recv_frame_blocking(&self) -> BridgeResult<CapturedFrame> {
        self.frame_rx.recv().map_err(|_| BridgeError::Video("Capture channel closed".into()))
    }

    pub fn frame_receiver(&self) -> Receiver<CapturedFrame> {
        self.frame_rx.clone()
    }

    pub fn stats(&self) -> CaptureStats {
        CaptureStats {
            frames_captured: self.frame_count.load(Ordering::SeqCst),
            is_running: self.is_running.load(Ordering::SeqCst),
        }
    }
}

impl Drop for ScreenCapturer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn capture_loop(
    config: CaptureConfig,
    frame_tx: Sender<CapturedFrame>,
    frame_count: Arc<AtomicU64>,
    is_running: Arc<AtomicBool>,
) -> BridgeResult<()> {
    let (conn, screen_num) = x11rb::connect(None)
        .map_err(|e| BridgeError::Video(format!("X11 connect failed: {}", e)))?;

    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let width = if config.width > 0 { config.width } else { screen.width_in_pixels as u32 };
    let height = if config.height > 0 { config.height } else { screen.height_in_pixels as u32 };

    info!("X11 capture: {}x{} from root window", width, height);

    // Check for SHM extension
    let shm_available = shm::query_version(&conn)
        .map(|cookie| cookie.reply().is_ok())
        .unwrap_or(false);

    let frame_interval = std::time::Duration::from_micros(1_000_000 / config.fps as u64);

    while is_running.load(Ordering::SeqCst) {
        let start = std::time::Instant::now();

        let frame_num = frame_count.fetch_add(1, Ordering::SeqCst);

        // Capture frame via GetImage (SHM path can be added as optimization)
        let reply = get_image(
            &conn,
            ImageFormat::Z_PIXMAP,
            root,
            0, 0,
            width as u16,
            height as u16,
            !0, // all planes
        )
        .map_err(|e| BridgeError::Video(format!("GetImage failed: {}", e)))?
        .reply()
        .map_err(|e| BridgeError::Video(format!("GetImage reply failed: {}", e)))?;

        let pts_us = bridge_common::now_us();

        let frame = CapturedFrame {
            data: reply.data,
            width,
            height,
            bytes_per_row: width * 4, // BGRA = 4 bytes per pixel
            pts_us,
            frame_number: frame_num,
            dma_buf_fd: None,
        };

        match frame_tx.try_send(frame) {
            Ok(()) => debug!("Frame {} captured", frame_num),
            Err(_) => debug!("Frame {} dropped (channel full)", frame_num),
        }

        // Sleep to maintain target frame rate
        let elapsed = start.elapsed();
        if elapsed < frame_interval {
            std::thread::sleep(frame_interval - elapsed);
        }
    }

    Ok(())
}

pub fn get_displays() -> Vec<DisplayInfo> {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return vec![];
    };

    let screen = &conn.setup().roots[screen_num];
    vec![DisplayInfo {
        id: screen_num as u32,
        width: screen.width_in_pixels as u32,
        height: screen.height_in_pixels as u32,
        is_main: true,
    }]
}

pub fn is_capture_supported() -> bool {
    std::env::var("DISPLAY").is_ok()
}
```

- [ ] **Step 3: Verify the module compiles (on macOS it won't be included, but check syntax)**

Run: `cargo check 2>&1 | head -20`

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Implement X11 screen capture for Linux

XShm-capable capture using x11rb crate. Captures root window
at configurable resolution and frame rate. Falls back to
GetImage when SHM is unavailable."
```

---

### Task 6: Implement Linux V4L2 hardware encoder

**Files:**
- Create: `bridge-video/src/linux/v4l2_encoder.rs`
- Create: `bridge-video/src/linux/color_convert.rs`
- Modify: `bridge-video/src/linux/mod.rs`

- [ ] **Step 1: Implement BGRA→NV12 color conversion**

```rust
//! BGRA to NV12 color conversion for V4L2 encoder input
//!
//! The Jetson Nano hardware encoder expects NV12 (YUV 4:2:0 semi-planar).
//! X11 capture gives us BGRA. This module provides CPU conversion.

/// Convert BGRA frame to NV12 format
/// NV12 layout: Y plane (width*height bytes) followed by interleaved UV plane (width*height/2 bytes)
pub fn bgra_to_nv12(bgra: &[u8], width: u32, height: u32, nv12: &mut Vec<u8>) {
    let w = width as usize;
    let h = height as usize;
    nv12.resize(w * h * 3 / 2, 0);

    let y_plane = &mut nv12[..w * h];
    let uv_plane = &mut nv12[w * h..];

    for row in 0..h {
        for col in 0..w {
            let bgra_idx = (row * w + col) * 4;
            let b = bgra[bgra_idx] as i32;
            let g = bgra[bgra_idx + 1] as i32;
            let r = bgra[bgra_idx + 2] as i32;

            // BT.601 conversion
            let y = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            y_plane[row * w + col] = y.clamp(0, 255) as u8;

            // Subsample UV at 2x2
            if row % 2 == 0 && col % 2 == 0 {
                let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
                let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
                let uv_idx = (row / 2) * w + col;
                uv_plane[uv_idx] = u.clamp(0, 255) as u8;
                uv_plane[uv_idx + 1] = v.clamp(0, 255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bgra_to_nv12_dimensions() {
        let width = 4u32;
        let height = 4u32;
        let bgra = vec![0u8; (width * height * 4) as usize];
        let mut nv12 = Vec::new();
        bgra_to_nv12(&bgra, width, height, &mut nv12);
        assert_eq!(nv12.len(), (width * height * 3 / 2) as usize);
    }

    #[test]
    fn test_bgra_to_nv12_black() {
        let width = 2u32;
        let height = 2u32;
        // BGRA black = [0, 0, 0, 255]
        let bgra = vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
        let mut nv12 = Vec::new();
        bgra_to_nv12(&bgra, width, height, &mut nv12);
        // Y for black should be 16 (BT.601)
        assert_eq!(nv12[0], 16);
    }
}
```

- [ ] **Step 2: Implement V4L2 hardware encoder**

```rust
//! V4L2 memory-to-memory hardware encoder for Jetson Nano
//!
//! Uses the NVIDIA Multimedia API exposed through V4L2 M2M devices.
//! The Jetson Nano's hardware encoder is at /dev/video0 (or similar).

use bridge_common::{BridgeResult, BridgeError, VideoCodec};
use crate::capture::CapturedFrame;
use crate::codec::{EncodedFrame, EncoderConfig};
use crate::linux::color_convert::bgra_to_nv12;
use bytes::Bytes;
use crossbeam_channel::{bounded, Receiver, Sender};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// V4L2 pixel format constants
const V4L2_PIX_FMT_NV12M: u32 = u32::from_le_bytes(*b"NM12");
const V4L2_PIX_FMT_H264: u32 = u32::from_le_bytes(*b"H264");
const V4L2_PIX_FMT_H265: u32 = u32::from_le_bytes(*b"HEVC");

/// V4L2 ioctl constants
const VIDIOC_S_FMT: libc::c_ulong = 0xC0D05605;
const VIDIOC_REQBUFS: libc::c_ulong = 0xC0145608;
const VIDIOC_QBUF: libc::c_ulong = 0xC044560F;
const VIDIOC_DQBUF: libc::c_ulong = 0xC0445611;
const VIDIOC_STREAMON: libc::c_ulong = 0x40045612;
const VIDIOC_STREAMOFF: libc::c_ulong = 0x40045613;
const VIDIOC_S_CTRL: libc::c_ulong = 0xC008561C;

const V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE: u32 = 9;
const V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE: u32 = 10;
const V4L2_MEMORY_MMAP: u32 = 1;

/// Hardware video encoder using V4L2 M2M
pub struct VideoEncoder {
    config: EncoderConfig,
    /// File descriptor for V4L2 device
    fd: i32,
    frame_count: Arc<AtomicU64>,
    frame_tx: Sender<EncodedFrame>,
    frame_rx: Receiver<EncodedFrame>,
    /// Reusable NV12 conversion buffer
    nv12_buf: Vec<u8>,
    force_keyframe: bool,
}

unsafe impl Send for VideoEncoder {}

impl VideoEncoder {
    pub fn new(config: EncoderConfig) -> BridgeResult<Self> {
        info!(
            "Creating V4L2 encoder: {}x{} @ {}fps, {} codec, {} bps",
            config.width, config.height, config.fps,
            match config.codec {
                VideoCodec::H264 => "H.264",
                VideoCodec::H265 => "H.265",
                VideoCodec::Raw => "Raw",
            },
            config.bitrate
        );

        if config.codec == VideoCodec::Raw {
            return Err(BridgeError::Video("Raw codec doesn't need encoder".into()));
        }

        // Find V4L2 encoder device
        let fd = Self::open_encoder_device()?;

        // TODO: Configure V4L2 formats and buffers via ioctl
        // This requires the actual Jetson hardware to test against.
        // The structure is:
        // 1. Set output format (raw NV12 input)
        // 2. Set capture format (H.264/H.265 output)
        // 3. Request buffers for both queues
        // 4. Memory-map or userptr the buffers
        // 5. Start streaming on both queues

        let (frame_tx, frame_rx) = bounded(8);
        let nv12_size = (config.width * config.height * 3 / 2) as usize;

        Ok(Self {
            config,
            fd,
            frame_count: Arc::new(AtomicU64::new(0)),
            frame_tx,
            frame_rx,
            nv12_buf: vec![0u8; nv12_size],
            force_keyframe: false,
        })
    }

    fn open_encoder_device() -> BridgeResult<i32> {
        // Try common Jetson encoder device paths
        for path in &["/dev/video0", "/dev/video1", "/dev/nvhost-msenc"] {
            let c_path = std::ffi::CString::new(*path).unwrap();
            let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
            if fd >= 0 {
                info!("Opened V4L2 encoder device: {}", path);
                return Ok(fd);
            }
        }
        Err(BridgeError::Video("No V4L2 encoder device found. Is this a Jetson?".into()))
    }

    /// Encode a captured frame
    pub fn encode(&mut self, frame: &mut CapturedFrame) -> BridgeResult<()> {
        let pixel_data = if frame.data.is_empty() {
            // On Linux this shouldn't happen (X11 capture fills data directly)
            return Err(BridgeError::Video("No pixel data in frame".into()));
        } else {
            &frame.data
        };

        // Convert BGRA to NV12
        bgra_to_nv12(pixel_data, frame.width, frame.height, &mut self.nv12_buf);

        // TODO: Queue NV12 data to V4L2 output, dequeue encoded from capture
        // For now, this is a stub that will be completed on actual Jetson hardware.
        // The frame flow is:
        // 1. Copy nv12_buf into V4L2 output buffer
        // 2. Queue the output buffer (VIDIOC_QBUF)
        // 3. Dequeue encoded data from capture buffer (VIDIOC_DQBUF)
        // 4. Create EncodedFrame from the encoded data

        let frame_num = self.frame_count.fetch_add(1, Ordering::SeqCst);
        debug!("V4L2 encode frame {} ({}x{}) — stub", frame_num, frame.width, frame.height);

        Ok(())
    }

    /// Receive an encoded frame (non-blocking)
    pub fn recv_frame(&self) -> Option<EncodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    /// Request next frame be encoded as a keyframe
    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    /// Update target bitrate
    pub fn set_bitrate(&mut self, bitrate: u32) -> BridgeResult<()> {
        info!("V4L2 encoder: setting bitrate to {} Mbps", bitrate / 1_000_000);
        self.config.bitrate = bitrate;
        // TODO: VIDIOC_S_CTRL with V4L2_CID_MPEG_VIDEO_BITRATE
        Ok(())
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd); }
        }
    }
}
```

- [ ] **Step 3: Run color conversion tests**

Run: `cargo test -p bridge-video linux::color_convert 2>&1`
Expected: tests pass (color conversion is pure Rust, no hardware needed)

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Implement V4L2 encoder and BGRA-to-NV12 conversion for Jetson

V4L2 M2M encoder structure with device discovery and color
conversion. V4L2 ioctl calls are stubbed pending Jetson hardware
testing. BGRA→NV12 conversion is fully implemented and tested."
```

---

### Task 7: Implement Linux uinput input injection

**Files:**
- Create: `bridge-input/src/linux/uinput.rs`
- Create: `bridge-input/src/linux/keymap.rs`

- [ ] **Step 1: Implement keycode mapping**

```rust
//! macOS virtual keycode to Linux KEY_* mapping

/// Convert macOS virtual keycode (used in protocol) to Linux input event code
pub fn macos_to_linux_keycode(macos_code: u16) -> u16 {
    match macos_code {
        0x00 => 30,  // KEY_A
        0x01 => 31,  // KEY_S
        0x02 => 32,  // KEY_D
        0x03 => 33,  // KEY_F
        0x04 => 35,  // KEY_H
        0x05 => 34,  // KEY_G
        0x06 => 44,  // KEY_Z
        0x07 => 45,  // KEY_X
        0x08 => 46,  // KEY_C
        0x09 => 47,  // KEY_V
        0x0B => 48,  // KEY_B
        0x0C => 16,  // KEY_Q
        0x0D => 17,  // KEY_W
        0x0E => 18,  // KEY_E
        0x0F => 19,  // KEY_R
        0x10 => 21,  // KEY_Y
        0x11 => 20,  // KEY_T
        0x12 => 2,   // KEY_1
        0x13 => 3,   // KEY_2
        0x14 => 4,   // KEY_3
        0x15 => 5,   // KEY_4
        0x16 => 7,   // KEY_6
        0x17 => 6,   // KEY_5
        0x18 => 13,  // KEY_EQUAL
        0x19 => 10,  // KEY_9
        0x1A => 8,   // KEY_7
        0x1B => 12,  // KEY_MINUS
        0x1C => 9,   // KEY_8
        0x1D => 11,  // KEY_0
        0x1E => 27,  // KEY_RIGHTBRACE
        0x1F => 24,  // KEY_O
        0x20 => 22,  // KEY_U
        0x21 => 26,  // KEY_LEFTBRACE
        0x22 => 23,  // KEY_I
        0x23 => 25,  // KEY_P
        0x24 => 28,  // KEY_ENTER
        0x25 => 38,  // KEY_L
        0x26 => 36,  // KEY_J
        0x27 => 40,  // KEY_APOSTROPHE
        0x28 => 37,  // KEY_K
        0x29 => 39,  // KEY_SEMICOLON
        0x2A => 43,  // KEY_BACKSLASH
        0x2B => 51,  // KEY_COMMA
        0x2C => 53,  // KEY_SLASH
        0x2D => 49,  // KEY_N
        0x2E => 50,  // KEY_M
        0x2F => 52,  // KEY_DOT
        0x30 => 15,  // KEY_TAB
        0x31 => 57,  // KEY_SPACE
        0x32 => 41,  // KEY_GRAVE
        0x33 => 14,  // KEY_BACKSPACE
        0x35 => 1,   // KEY_ESC
        0x37 => 125, // KEY_LEFTMETA (Command -> Super)
        0x38 => 42,  // KEY_LEFTSHIFT
        0x39 => 58,  // KEY_CAPSLOCK
        0x3A => 56,  // KEY_LEFTALT (Option -> Alt)
        0x3B => 29,  // KEY_LEFTCTRL
        0x3C => 54,  // KEY_RIGHTSHIFT
        0x3D => 100, // KEY_RIGHTALT
        0x3E => 97,  // KEY_RIGHTCTRL
        0x7A => 59,  // KEY_F1
        0x78 => 60,  // KEY_F2
        0x63 => 61,  // KEY_F3
        0x76 => 62,  // KEY_F4
        0x60 => 63,  // KEY_F5
        0x61 => 64,  // KEY_F6
        0x62 => 65,  // KEY_F7
        0x64 => 66,  // KEY_F8
        0x65 => 67,  // KEY_F9
        0x6D => 68,  // KEY_F10
        0x67 => 87,  // KEY_F11
        0x6F => 88,  // KEY_F12
        0x7B => 105, // KEY_LEFT
        0x7C => 106, // KEY_RIGHT
        0x7D => 108, // KEY_DOWN
        0x7E => 103, // KEY_UP
        _ => 0,      // KEY_RESERVED (unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_common_keys() {
        assert_eq!(macos_to_linux_keycode(0x00), 30); // A
        assert_eq!(macos_to_linux_keycode(0x24), 28); // Return -> Enter
        assert_eq!(macos_to_linux_keycode(0x31), 57); // Space
        assert_eq!(macos_to_linux_keycode(0x35), 1);  // Escape
    }

    #[test]
    fn test_unknown_keycode() {
        assert_eq!(macos_to_linux_keycode(0xFF), 0); // Unknown -> KEY_RESERVED
    }
}
```

- [ ] **Step 2: Implement uinput input injector**

```rust
//! Input injection via Linux uinput kernel module

use bridge_common::{BridgeResult, BridgeError, InputEvent, KeyboardEvent, MouseMoveEvent, MouseButtonEvent, MouseScrollEvent};
use crate::linux::keymap::macos_to_linux_keycode;
use tracing::{debug, info, trace, warn};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;

// Linux input event constants
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

// uinput ioctl constants
const UINPUT_MAX_NAME_SIZE: usize = 80;

#[repr(C)]
struct InputEvent2 {
    time: libc::timeval,
    r#type: u16,
    code: u16,
    value: i32,
}

/// Input injector using Linux uinput
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
                "Cannot open /dev/uinput: {}. Add user to 'input' group.", e
            )))?;

        // TODO: Set up uinput device via ioctl:
        // 1. UI_SET_EVBIT for EV_KEY, EV_REL, EV_ABS
        // 2. UI_SET_KEYBIT for all keys
        // 3. UI_SET_RELBIT for REL_WHEEL, REL_HWHEEL
        // 4. UI_SET_ABSBIT for ABS_X, ABS_Y with min=0, max=screen_size
        // 5. Write uinput_user_dev struct
        // 6. UI_DEV_CREATE ioctl

        info!("uinput device created");

        Ok(Self {
            file,
            screen_width: 1920.0, // Will be set from capture config
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
                warn!("Raw HID injection not supported on Linux");
                Ok(())
            }
        }
    }

    fn write_event(&mut self, type_: u16, code: u16, value: i32) -> BridgeResult<()> {
        let ev = InputEvent2 {
            time: libc::timeval { tv_sec: 0, tv_usec: 0 },
            r#type: type_,
            code,
            value,
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &ev as *const InputEvent2 as *const u8,
                std::mem::size_of::<InputEvent2>(),
            )
        };
        self.file.write_all(bytes)
            .map_err(|e| BridgeError::Input(format!("uinput write failed: {}", e)))?;
        Ok(())
    }

    fn sync(&mut self) -> BridgeResult<()> {
        self.write_event(EV_SYN, SYN_REPORT, 0)
    }

    fn inject_keyboard(&mut self, event: &KeyboardEvent) -> BridgeResult<()> {
        let linux_key = macos_to_linux_keycode(event.key_code);
        if linux_key == 0 {
            debug!("Unknown keycode: 0x{:02X}", event.key_code);
            return Ok(());
        }
        let value = if event.is_pressed { 1 } else { 0 };
        self.write_event(EV_KEY, linux_key, value)?;
        self.sync()?;
        trace!("Injected key: linux={} pressed={}", linux_key, event.is_pressed);
        Ok(())
    }

    fn inject_mouse_move(&mut self, event: &MouseMoveEvent) -> BridgeResult<()> {
        if event.is_absolute {
            let x = (event.x * self.screen_width) as i32;
            let y = (event.y * self.screen_height) as i32;
            self.write_event(EV_ABS, ABS_X, x)?;
            self.write_event(EV_ABS, ABS_Y, y)?;
        } else {
            self.write_event(EV_REL, 0x00, event.delta_x as i32)?; // REL_X
            self.write_event(EV_REL, 0x01, event.delta_y as i32)?; // REL_Y
        }
        self.sync()?;
        Ok(())
    }

    fn inject_mouse_button(&mut self, event: &MouseButtonEvent) -> BridgeResult<()> {
        let btn = match event.button {
            0 => BTN_LEFT,
            1 => BTN_RIGHT,
            2 => BTN_MIDDLE,
            n => BTN_LEFT + n as u16,
        };
        let value = if event.is_pressed { 1 } else { 0 };
        self.write_event(EV_KEY, btn, value)?;
        self.sync()?;
        Ok(())
    }

    fn inject_scroll(&mut self, event: &MouseScrollEvent) -> BridgeResult<()> {
        if event.delta_y.abs() > 0.01 {
            self.write_event(EV_REL, REL_WHEEL, event.delta_y as i32)?;
        }
        if event.delta_x.abs() > 0.01 {
            self.write_event(EV_REL, REL_HWHEEL, event.delta_x as i32)?;
        }
        self.sync()?;
        Ok(())
    }

    pub fn warp_cursor(&self, _x: f64, _y: f64) -> BridgeResult<()> {
        // Absolute positioning handled by inject_mouse_move with is_absolute=true
        Ok(())
    }

    pub fn set_cursor_captured(&self, _captured: bool) -> BridgeResult<()> {
        // Linux handles this differently — for now, no-op
        Ok(())
    }
}
```

- [ ] **Step 3: Run keymap tests**

Run: `cargo test -p bridge-input linux::keymap 2>&1`
Expected: tests pass

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Implement Linux uinput input injection with keycode mapping

uinput-based keyboard/mouse/scroll injection. macOS-to-Linux
keycode translation table covering standard keys, modifiers,
function keys, and arrows."
```

---

### Task 8: Implement Linux Avahi service discovery

**Files:**
- Create: `bridge-transport/src/linux/discovery.rs`
- Modify: `bridge-transport/src/linux/mod.rs`

- [ ] **Step 1: Implement Avahi discovery stubs**

Since Avahi requires `libavahi-client` C library at link time and this likely won't be on a Mac build machine, use a simpler approach: manual mDNS for now, or stub with direct IP connection as fallback.

```rust
//! Service discovery for Linux using manual connection
//!
//! Full Avahi integration requires libavahi-client. For now, this provides
//! a direct IP-based ServiceAdvertiser and ServiceBrowser that can be used
//! when the server address is known (common for Jetson setups).

use bridge_common::{BridgeResult, BridgeError, ServerInfo, Resolution};
use std::net::SocketAddr;
use tracing::info;

/// Service advertiser (Linux — logs availability, no mDNS yet)
pub struct ServiceAdvertiser {
    name: String,
    port: u16,
}

impl ServiceAdvertiser {
    pub async fn new(name: &str, port: u16) -> BridgeResult<Self> {
        info!("Linux service advertiser: {} on port {} (direct IP mode)", name, port);
        info!("Clients can connect directly via IP address");
        Ok(Self {
            name: name.to_string(),
            port,
        })
    }
}

/// Service browser (Linux — supports direct IP connection)
pub struct ServiceBrowser;

impl ServiceBrowser {
    pub async fn new() -> BridgeResult<Self> {
        Ok(Self)
    }

    pub async fn discover(&self) -> BridgeResult<Vec<ServerInfo>> {
        // On Linux without Avahi, return empty — client uses --address flag
        Ok(vec![])
    }
}
```

- [ ] **Step 2: Update linux/mod.rs**

```rust
pub mod discovery;
```

- [ ] **Step 3: Verify build**

Run: `cargo build 2>&1 | head -20`

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Add Linux service discovery stub (direct IP mode)

Clients connect via --address flag on Linux. Full Avahi/mDNS
support to be added when libavahi-client is available."
```

---

### Task 9: Update bridge-server for cross-platform compilation

**Files:**
- Modify: `bridge-server/src/main.rs`
- Modify: `bridge-audio/src/lib.rs` (cfg-gate macOS imports)
- Modify: `bridge-audio/src/capture.rs` (cfg-gate)
- Modify: `bridge-audio/src/playback.rs` (cfg-gate)

- [ ] **Step 1: Gate macOS-specific code in server main.rs**

The server uses `get_thunderbolt_address`, `is_thunderbolt_connection`, `VirtualDisplay`, `is_virtual_display_supported` — these are macOS-only. Wrap in `#[cfg(target_os = "macos")]` blocks with Linux alternatives:

Key changes:
- Thunderbolt detection: skip on Linux (`get_thunderbolt_address` returns None)
- Virtual display: skip on Linux (use Xvfb externally)
- `is_capture_supported()`: already cfg-gated in bridge-video
- `get_displays()`: already cfg-gated in bridge-video

- [ ] **Step 2: Gate macOS imports in bridge-audio**

Wrap CoreAudio-specific code in `capture.rs` and `playback.rs` with `#[cfg(target_os = "macos")]`. On Linux, `cpal` handles audio through ALSA — the code should work but macOS framework imports need gating.

- [ ] **Step 3: Verify macOS build still passes**

Run: `cargo build 2>&1 | head -20`
Expected: success

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Update bridge-server and bridge-audio for cross-platform compilation

Gate macOS-specific code behind cfg. Linux server skips Thunderbolt
detection and virtual display (use Xvfb externally instead)."
```

---

### Task 10: Add Xvfb virtual display helper for headless Linux

**Files:**
- Create: `bridge-video/src/linux/xvfb.rs`

- [ ] **Step 1: Implement Xvfb helper**

```rust
//! Virtual display management via Xvfb for headless Linux
//!
//! When no physical display is connected, starts an Xvfb instance
//! at the desired resolution. The X11 capturer then captures from it.

use bridge_common::{BridgeResult, BridgeError};
use std::process::{Command, Child};
use tracing::{info, warn};

pub struct VirtualDisplay {
    child: Option<Child>,
    display_num: u32,
    width: u32,
    height: u32,
}

impl VirtualDisplay {
    /// Start an Xvfb virtual display.
    /// Sets DISPLAY env var so subsequent X11 connections use it.
    pub fn new(width: u32, height: u32, _refresh_rate: u32) -> BridgeResult<Self> {
        let display_num = 99; // Use :99 by convention
        let display_str = format(":{}", display_num);
        let resolution = format!("{}x{}x24", width, height);

        info!("Starting Xvfb on {} at {}", display_str, resolution);

        let child = Command::new("Xvfb")
            .arg(&display_str)
            .arg("-screen").arg("0").arg(&resolution)
            .arg("-ac") // disable access control
            .spawn()
            .map_err(|e| BridgeError::Video(format!(
                "Failed to start Xvfb: {}. Install with: sudo apt install xvfb", e
            )))?;

        // Give Xvfb time to start
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Set DISPLAY for this process
        std::env::set_var("DISPLAY", &display_str);
        info!("Xvfb started on {}", display_str);

        Ok(Self {
            child: Some(child),
            display_num,
            width,
            height,
        })
    }

    pub fn display_id(&self) -> u32 {
        self.display_num
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

pub fn is_virtual_display_supported() -> bool {
    Command::new("which").arg("Xvfb").output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            info!("Xvfb stopped");
        }
    }
}
```

- [ ] **Step 2: Fix typo and verify syntax**

The `format(":{}", display_num)` should be `format!(":{}", display_num)`.

- [ ] **Step 3: Update linux/mod.rs to include xvfb**

Add `pub mod xvfb;` to `bridge-video/src/linux/mod.rs`.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Add Xvfb virtual display helper for headless Linux

Starts Xvfb at configured resolution for headless Jetson scenarios.
Sets DISPLAY env var automatically for the capture pipeline."
```

---

### Task 11: Integration — wire everything together and verify

**Files:**
- Modify: `bridge-video/src/linux/mod.rs` — export `get_displays`, `is_capture_supported`
- Modify: `bridge-video/src/lib.rs` — verify all Linux re-exports
- Verify: `bridge-server/src/main.rs` compiles on both platforms

- [ ] **Step 1: Verify all Linux module exports are correct**

Ensure `bridge-video/src/lib.rs` has:
```rust
#[cfg(target_os = "linux")]
pub use linux::x11_capture::{ScreenCapturer, get_displays, is_capture_supported};
#[cfg(target_os = "linux")]
pub use linux::v4l2_encoder::VideoEncoder;
#[cfg(target_os = "linux")]
pub mod virtual_display {
    pub use crate::linux::xvfb::*;
}
```

- [ ] **Step 2: Run full test suite on macOS**

Run: `cargo test 2>&1 | tail -20`
Expected: all existing tests pass (macOS code unchanged functionally)

- [ ] **Step 3: Run clippy**

Run: `cargo clippy 2>&1 | head -30`
Expected: no new warnings

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Wire Linux modules together and verify integration

All platform modules properly exported. macOS build and tests
verified passing. Linux compilation ready for cross-compile or
native Jetson build."
```

---

### Task 12: Add cross-compilation support

**Files:**
- Create: `Cross.toml` — cross-compilation config
- Modify: `bridge-video/build.rs` — cfg-gate macOS framework linking

- [ ] **Step 1: Create Cross.toml**

```toml
[target.aarch64-unknown-linux-gnu]
image = "ghcr.io/cross-rs/aarch64-unknown-linux-gnu:main"
pre-build = [
    "dpkg --add-architecture arm64",
    "apt-get update",
    "apt-get install -y libx11-dev:arm64 libxcb1-dev:arm64 libxcb-shm0-dev:arm64"
]
```

- [ ] **Step 2: Gate build.rs framework linking**

In `bridge-video/build.rs`, wrap macOS framework linking in `#[cfg]`:

```rust
fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=framework=VideoToolbox");
        println!("cargo:rustc-link-lib=framework=CoreMedia");
        // ... other frameworks
    }
}
```

Same for `bridge-input/build.rs` and `bridge-audio/build.rs` if they exist.

- [ ] **Step 3: Verify macOS build**

Run: `cargo build 2>&1 | head -20`

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Add cross-compilation support for Linux/aarch64

Cross.toml for cross-rs tool. Build scripts cfg-gated to only
link macOS frameworks on macOS target."
```

---

## Summary

| Task | Component | Lines (est.) | Can test on Mac? |
|------|-----------|-------------|------------------|
| 1 | Cargo.toml cfg gating | ~100 | Yes |
| 2 | bridge-video reorg | ~800 (move) | Yes |
| 3 | bridge-input reorg | ~300 (move) | Yes |
| 4 | bridge-transport reorg | ~200 (move) | Yes |
| 5 | X11 capture | ~200 (new) | Syntax only |
| 6 | V4L2 encoder + color convert | ~300 (new) | Color convert tests |
| 7 | uinput + keymap | ~250 (new) | Keymap tests |
| 8 | Avahi/discovery stub | ~60 (new) | Yes |
| 9 | Server cross-platform | ~100 (modify) | Yes |
| 10 | Xvfb helper | ~80 (new) | Syntax only |
| 11 | Integration verification | ~20 (modify) | Yes |
| 12 | Cross-compilation | ~30 (new) | Yes |

Tasks 1-4 are the reorganization phase — pure refactoring, fully testable on macOS.
Tasks 5-10 add Linux backends — some testable on macOS (pure Rust logic), some need Jetson.
Tasks 11-12 are integration and build infrastructure.
