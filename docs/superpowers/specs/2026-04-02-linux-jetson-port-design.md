# Linux/Jetson Nano Server Port — Design Spec

**Date**: 2026-04-02
**Status**: Approved
**Priority**: Latency-first

## Overview

Port the Bridge server to run on NVIDIA Jetson Nano (Linux/aarch64), enabling full desktop capture and streaming to any-platform clients. The existing Mac server and client remain untouched. Platform abstraction traits provide compile-time selection of backends.

## Target Hardware

- **NVIDIA Jetson Nano** (Maxwell GPU, dedicated HW encoder via V4L2 Multimedia API)
- Supports physical display (HDMI) and headless (Xvfb) operation
- Clients: Mac (existing Metal renderer) + Linux/cross-platform (new wgpu renderer)

## Architecture: Platform Abstraction Traits

Each platform-specific component gets a trait. Implementations selected via `#[cfg(target_os)]`.

### Traits

```rust
// bridge-video
pub trait ScreenCapture: Send {
    fn start(&mut self) -> BridgeResult<()>;
    fn next_frame(&mut self) -> BridgeResult<CapturedFrame>;
    fn stop(&mut self);
}

pub trait VideoEncoder: Send {
    fn encode(&mut self, frame: &CapturedFrame) -> BridgeResult<EncodedFrame>;
    fn request_keyframe(&mut self);
    fn update_bitrate(&mut self, bitrate_bps: u32);
}

pub trait VideoDecoder: Send {
    fn decode(&mut self, frame: &EncodedFrame) -> BridgeResult<DecodedFrame>;
}

// bridge-input
pub trait InputInjector: Send {
    fn inject(&mut self, event: &InputEvent) -> BridgeResult<()>;
}

// bridge-transport
pub trait ServiceDiscovery: Send {
    async fn advertise(&self, name: &str, port: u16) -> BridgeResult<()>;
    async fn discover(&self) -> BridgeResult<Vec<ServerInfo>>;
}
```

### Factory Functions

```rust
pub fn create_capturer(config: &CaptureConfig) -> BridgeResult<Box<dyn ScreenCapture>> {
    #[cfg(target_os = "macos")]
    { Ok(Box::new(macos::SckCapturer::new(config)?)) }
    #[cfg(target_os = "linux")]
    { Ok(Box::new(linux::X11Capturer::new(config)?)) }
}
```

Same pattern for encoder, decoder, injector, discovery.

## Component Designs

### 1. Screen Capture (Linux)

**Physical display**: X11 capture via `xcb` crate + XShm extension.
- Connect to X display, attach shared memory segment
- `XShmGetImage` per frame — sub-millisecond, near-zero-copy
- Frame rate governed by timer at configured FPS
- Pixel format: BGRA (matches existing `CapturedFrame`)

**Headless**: Xvfb at desired resolution. Capturer auto-detects or user sets `DISPLAY`.

**Resolution**: Native X display resolution. Configurable via Xvfb for virtual displays.

### 2. Video Encoding (Jetson Nano)

**API**: V4L2 memory-to-memory via `/dev/video*` (NVIDIA Multimedia API).
- Set output format: `V4L2_PIX_FMT_NV12`
- Set capture format: `V4L2_PIX_FMT_H264` (default) or `V4L2_PIX_FMT_H265`
- Queue raw frames, dequeue encoded bitstream

**Color conversion**: BGRA → NV12 required.
- Primary: GPU-accelerated via OpenGL ES compute or CUDA
- Fallback: CPU conversion via `libyuv` or manual

**Codec**: H.264 default (Nano handles 1080p30 comfortably). H.265 supported but lower throughput.

**Bitrate adaptation**: Same algorithm as Mac — packet loss monitoring. Adjusted via `V4L2_CID_MPEG_VIDEO_BITRATE`. Keyframes via `V4L2_CID_MPEG_VIDEO_FORCE_KEY_FRAME`.

**Performance target**: 1080p30 H.264 (4K not realistic on Nano).

### 3. Video Decoding (Client-side Linux)

V4L2 M2M decoder or FFmpeg `h264`/`hevc` software decode. For cross-platform client, use FFmpeg via `ffmpeg-next` crate as the common decoder, keeping VideoToolbox for Mac.

### 4. Display Rendering (Cross-platform Client)

**Mac**: Existing Metal + CAMetalLayer (unchanged).
**Linux/cross-platform**: `wgpu` crate — provides Vulkan backend on Linux, Metal on Mac, DX12 on Windows. Single renderer implementation covers all non-Mac clients.

### 5. Input Injection (Linux)

**API**: `/dev/uinput` kernel module.
- Create virtual keyboard + mouse device
- Write `input_event` structs for key, mouse, scroll events

**Mapping**:
- `KeyboardEvent` → `EV_KEY` + Linux keycodes
- `MouseMoveEvent` → `EV_ABS` (absolute positioning)
- `MouseButtonEvent` → `EV_KEY` + `BTN_LEFT/RIGHT/MIDDLE`
- `MouseScrollEvent` → `EV_REL` + `REL_WHEEL/REL_HWHEEL`

**Keycode table**: Protocol-neutral keycodes → Linux `KEY_*` constants.

**Permissions**: Requires `/dev/uinput` write access (root or `input` group).

### 6. Service Discovery (Linux)

**API**: Avahi via `libavahi-client` (Bonjour/mDNS compatible).
- Advertise: `avahi_entry_group_add_service()`
- Browse: `avahi_service_browser_new()`
- Same service type `_bridge._tcp` — Mac and Linux discover each other.

### 7. Audio Capture (Linux)

**API**: `cpal` crate already supports ALSA. Minimal platform-specific code needed.
- Capture system audio via PulseAudio monitor source or ALSA loopback
- Same Opus encoding path as Mac

### 8. Transport (No Changes)

QUIC (`quinn`) and UDP are fully cross-platform. No changes needed.
Thunderbolt detection not applicable to Jetson — skip on Linux.

## CapturedFrame / DecodedFrame Changes

Add Linux-native zero-copy field alongside existing Mac fields:

```rust
pub struct CapturedFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub bytes_per_row: u32,
    pub pts_us: u64,
    pub frame_number: u64,
    #[cfg(target_os = "macos")]
    pub io_surface: Option<IOSurfaceRef>,
    #[cfg(target_os = "linux")]
    pub dma_buf_fd: Option<i32>,
}
```

## Cargo Workspace Changes

- Platform-specific dependencies gated by `#[cfg]` in each crate's `Cargo.toml`
- New Linux dependencies: `xcb`, `x11rb` (or raw xcb), `nix` (for V4L2/uinput ioctls), `v4l2-sys`
- New cross-platform dep: `wgpu` (for client renderer)
- Existing Mac deps unchanged, gated behind `[target.'cfg(target_os = "macos")'.dependencies]`

## File Structure (New)

```
bridge-video/src/
  capture.rs          → trait ScreenCapture + CapturedFrame
  macos/
    sck_capture.rs    → existing ScreenCaptureKit impl (moved)
    codec.rs          → existing VideoToolbox impl (moved)
    display.rs        → existing Metal renderer (moved)
    virtual_display.rs → existing CGVirtualDisplay (moved)
    sys.rs            → existing FFI bindings (moved)
  linux/
    x11_capture.rs    → XShm capture impl
    v4l2_encoder.rs   → V4L2 M2M encoder
    v4l2_decoder.rs   → V4L2 M2M decoder (optional)
    color_convert.rs  → BGRA→NV12 conversion
    xvfb.rs           → Virtual display management
  cross/
    wgpu_display.rs   → wgpu-based renderer

bridge-input/src/
  macos/
    inject.rs         → existing CGEventPost (moved)
    capture.rs        → existing IOHIDManager (moved)
    sys.rs            → existing FFI (moved)
  linux/
    uinput.rs         → uinput injection

bridge-transport/src/
  macos/
    discovery.rs      → existing Bonjour (moved)
  linux/
    discovery.rs      → Avahi discovery
```

## Build & Cross-Compilation

- Primary: build natively on the Jetson (`cargo build --release`)
- Alternative: cross-compile from Mac/Linux x86 using `cross` tool with aarch64 target
- CI: separate Linux/aarch64 build job

## Testing Strategy

- Unit tests for color conversion, keycode mapping, protocol compatibility
- Integration tests for V4L2 encoder (requires Jetson hardware or mock)
- Cross-platform protocol tests ensure Mac ↔ Linux interop
- Capture tests with Xvfb (can run in CI without hardware)

## Non-Goals

- 4K streaming on Jetson Nano (hardware can't handle it)
- Windows server support (only client via wgpu)
- Jetson-specific CUDA optimizations (keep it generic Linux + V4L2 first)
