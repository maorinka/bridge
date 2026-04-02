//! Bridge Diagnostic Tool
//!
//! Quick diagnostic to test if this Mac can capture screens,
//! create virtual displays, and encode video.

use bridge_common::VideoCodec;
use bridge_video::{
    CaptureConfig, ScreenCapturer, VideoEncoder, EncoderConfig,
    get_displays, is_capture_supported,
    virtual_display::{VirtualDisplay, is_virtual_display_supported},
};

// ANSI color codes
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

fn pass(label: &str) {
    println!("  {GREEN}{BOLD}PASS{RESET}  {label}");
}

fn fail(label: &str) {
    println!("  {RED}{BOLD}FAIL{RESET}  {label}");
}

fn warn(label: &str) {
    println!("  {YELLOW}{BOLD}WARN{RESET}  {label}");
}

fn info(label: &str) {
    println!("  {DIM}{label}{RESET}");
}

fn header(title: &str) {
    println!();
    println!("{CYAN}{BOLD}=== {title} ==={RESET}");
}

struct DiagResults {
    permission: bool,
    displays_found: bool,
    virtual_display: Option<bool>, // None = not supported, Some(ok)
    capture: Option<bool>,         // None = skipped
    capture_frames: u64,
    encode: Option<bool>,          // None = skipped
    encoded_frames: u64,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_target(false)
        .init();

    println!();
    println!("{BOLD}Bridge Diagnostic Tool{RESET}");
    println!("{DIM}Testing screen capture, virtual display, and video encoding{RESET}");

    let mut results = DiagResults {
        permission: false,
        displays_found: false,
        virtual_display: None,
        capture: None,
        capture_frames: 0,
        encode: None,
        encoded_frames: 0,
    };

    // Hold virtual display alive across sections so capture test can use it
    let mut vd_holder: Option<VirtualDisplay> = None;

    // -----------------------------------------------------------------------
    // 1. Screen recording permission
    // -----------------------------------------------------------------------
    header("Screen Recording Permission");

    let has_permission = ScreenCapturer::has_permission();
    if has_permission {
        pass("Screen recording permission granted");
        results.permission = true;
    } else {
        // Try requesting
        println!("  Requesting screen recording permission...");
        let granted = ScreenCapturer::request_permission();
        if granted {
            pass("Screen recording permission granted (after request)");
            results.permission = true;
        } else {
            fail("Screen recording permission denied");
            info("Go to System Settings > Privacy & Security > Screen Recording");
            info("and enable permission for this terminal/app.");
        }
    }

    // Also check is_capture_supported() which wraps both check + request
    let supported = is_capture_supported();
    info(&format!("is_capture_supported() = {supported}"));

    // -----------------------------------------------------------------------
    // 2. List displays
    // -----------------------------------------------------------------------
    header("Display Enumeration");

    let displays = get_displays();
    if displays.is_empty() {
        fail("No displays found (headless with no virtual display?)");
    } else {
        pass(&format!("Found {} display(s)", displays.len()));
        results.displays_found = true;
        for d in &displays {
            let main_tag = if d.is_main { " [MAIN]" } else { "" };
            info(&format!(
                "Display 0x{:08X} ({}): {}x{}{main_tag}",
                d.id, d.id, d.width, d.height
            ));
        }
    }

    // -----------------------------------------------------------------------
    // 3. Virtual display creation
    // -----------------------------------------------------------------------
    header("Virtual Display");

    if !is_virtual_display_supported() {
        #[cfg(target_os = "macos")]
        warn("CGVirtualDisplay API not available (requires macOS 14+)");
        #[cfg(target_os = "linux")]
        warn("Xvfb not found. Install: sudo apt install xvfb");
        results.virtual_display = None;
    } else {
        #[cfg(target_os = "macos")]
        info("CGVirtualDisplay API is available");
        #[cfg(target_os = "linux")]
        info("Xvfb is available");
        info("Attempting to create 3840x2160 @ 60Hz virtual display...");

        match VirtualDisplay::new(3840, 2160, 60) {
            Ok(vd) => {
                pass(&format!(
                    "Virtual display created: ID=0x{:08X} ({}), {}x{}",
                    vd.display_id(), vd.display_id(), vd.width(), vd.height()
                ));
                results.virtual_display = Some(true);

                // Verify it shows up in display list
                let displays_after = get_displays();
                let found = displays_after.iter().any(|d| d.id == vd.display_id());
                if found {
                    pass("Virtual display appears in display list");
                    let vd_info = displays_after.iter().find(|d| d.id == vd.display_id()).unwrap();
                    info(&format!(
                        "Confirmed resolution: {}x{}",
                        vd_info.width, vd_info.height
                    ));
                } else {
                    warn("Virtual display NOT found in display list (may not be fully registered)");
                }

                // Check resolution details (platform-specific)
                #[cfg(target_os = "macos")]
                {
                    let (logical_w, logical_h, pixel_w, pixel_h) = unsafe {
                        extern "C" {
                            fn CGDisplayPixelsWide(display: u32) -> usize;
                            fn CGDisplayPixelsHigh(display: u32) -> usize;
                            fn CGDisplayCopyDisplayMode(display: u32) -> *const std::ffi::c_void;
                            fn CGDisplayModeGetPixelWidth(mode: *const std::ffi::c_void) -> usize;
                            fn CGDisplayModeGetPixelHeight(mode: *const std::ffi::c_void) -> usize;
                            fn CGDisplayModeRelease(mode: *const std::ffi::c_void);
                        }
                        let lw = CGDisplayPixelsWide(vd.display_id());
                        let lh = CGDisplayPixelsHigh(vd.display_id());
                        let mode = CGDisplayCopyDisplayMode(vd.display_id());
                        let (pw, ph) = if !mode.is_null() {
                            let w = CGDisplayModeGetPixelWidth(mode);
                            let h = CGDisplayModeGetPixelHeight(mode);
                            CGDisplayModeRelease(mode);
                            (w, h)
                        } else {
                            (lw, lh)
                        };
                        (lw, lh, pw, ph)
                    };
                    info(&format!("Logical resolution:  {}x{}", logical_w, logical_h));
                    info(&format!("Backing pixels:      {}x{}", pixel_w, pixel_h));
                    if pixel_w > logical_w {
                        pass(&format!("HiDPI active: {}x scale", pixel_w / logical_w));
                    } else if pixel_w == logical_w && logical_w as u32 == vd.width() {
                        pass("Native resolution matches requested (1x scale)");
                    } else {
                        warn(&format!("Resolution mismatch: requested {}x{} but got {}x{} backing pixels",
                            vd.width(), vd.height(), pixel_w, pixel_h));
                    }
                }
                #[cfg(target_os = "linux")]
                {
                    info(&format!("Virtual display: {}x{} (Xvfb)", vd.width(), vd.height()));
                }

                // Keep virtual display alive for capture test
                vd_holder = Some(vd);
            }
            Err(e) => {
                fail(&format!("Virtual display creation failed: {e}"));
                results.virtual_display = Some(false);
            }
        }
    }

    // -----------------------------------------------------------------------
    // 4. Screen capture test (captures virtual display if available)
    // -----------------------------------------------------------------------
    header("Screen Capture");

    if !results.permission {
        warn("Skipping capture test (no screen recording permission)");
        results.capture = None;
    } else {
        // Prefer capturing the virtual display to test actual streaming resolution
        let displays = get_displays();
        let capture_display = if let Some(ref vd) = vd_holder {
            let vd_disp = displays.iter().find(|d| d.id == vd.display_id());
            if vd_disp.is_some() {
                info("Capturing VIRTUAL display (simulates server streaming scenario)");
            }
            vd_disp.or_else(|| displays.iter().find(|d| d.is_main)).or(displays.first())
        } else {
            displays.iter().find(|d| d.is_main).or(displays.first())
        };

        if let Some(display) = capture_display {
            // If capturing virtual display, use explicit 3840x2160 (server does this too)
            let is_vd = vd_holder.as_ref().map_or(false, |vd| vd.display_id() == display.id);
            let (cap_w, cap_h) = if is_vd {
                (3840u32, 2160u32)
            } else {
                (0, 0) // native
            };
            info(&format!(
                "Capturing display 0x{:08X}: {}x{} (config: {})",
                display.id, display.width, display.height,
                if is_vd { format!("explicit {}x{}", cap_w, cap_h) } else { "native".into() }
            ));

            let config = CaptureConfig {
                width: cap_w,
                height: cap_h,
                fps: 60,
                show_cursor: true,
                capture_audio: false,
                display_id: Some(display.id),
            };

            match ScreenCapturer::new(config) {
                Ok(mut capturer) => {
                    match capturer.start().await {
                        Ok(()) => {
                            pass("Screen capture started");

                            // Wait 1.5 seconds for frames
                            info("Waiting 1.5 seconds for frames...");
                            tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

                            let stats = capturer.stats();
                            results.capture_frames = stats.frames_captured;

                            if stats.frames_captured > 0 {
                                pass(&format!("Received {} frames in 1.5s ({:.1} fps)",
                                    stats.frames_captured,
                                    stats.frames_captured as f64 / 1.5
                                ));
                                results.capture = Some(true);

                                // Show a sample frame's info
                                if let Some(frame) = capturer.recv_frame() {
                                    #[cfg(target_os = "macos")]
                                    let surface_info = format!("io_surface={}", frame.io_surface.is_some());
                                    #[cfg(target_os = "linux")]
                                    let surface_info = format!("dma_buf={}", frame.dma_buf_fd.is_some());
                                    info(&format!(
                                        "Sample frame: {}x{}, bytes_per_row={}, {}",
                                        frame.width, frame.height, frame.bytes_per_row,
                                        surface_info
                                    ));
                                }
                            } else {
                                fail("No frames received in 1.5s");
                                results.capture = Some(false);
                                if capturer.is_stopped() {
                                    info("Capture stream was stopped externally (display disconnected?)");
                                }
                            }

                            let _ = capturer.stop();
                            info("Capture stopped");
                        }
                        Err(e) => {
                            fail(&format!("Failed to start capture: {e}"));
                            results.capture = Some(false);
                        }
                    }
                }
                Err(e) => {
                    fail(&format!("Failed to create capturer: {e}"));
                    results.capture = Some(false);
                }
            }
        } else {
            fail("No display available to capture");
            results.capture = Some(false);
        }
    }

    // -----------------------------------------------------------------------
    // 5. Video encoding test
    // -----------------------------------------------------------------------
    header("Video Encoding (H.265)");

    if results.capture != Some(true) {
        warn("Skipping encode test (capture did not succeed)");
        results.encode = None;
    } else {
        let displays = get_displays();
        let capture_display = displays.iter().find(|d| d.is_main).or(displays.first());

        if let Some(display) = capture_display {
            let encoder_config = EncoderConfig {
                width: display.width,
                height: display.height,
                fps: 60,
                bitrate: 100_000_000, // 100 Mbps
                codec: VideoCodec::H265,
                keyframe_interval: 60,
                low_latency: true,
                realtime: true,
                max_quality: false,
            };

            info(&format!(
                "Creating H.265 encoder: {}x{} @ 60fps, 100 Mbps",
                encoder_config.width, encoder_config.height
            ));

            match VideoEncoder::new(encoder_config) {
                Ok(mut encoder) => {
                    #[cfg(target_os = "macos")]
                    pass("VideoToolbox encoder created");
                    #[cfg(target_os = "linux")]
                    pass("V4L2 encoder created");

                    // Capture a few frames and encode them
                    let cap_config = CaptureConfig {
                        width: 0,
                        height: 0,
                        fps: 60,
                        show_cursor: true,
                        capture_audio: false,
                        display_id: Some(display.id),
                    };

                    match ScreenCapturer::new(cap_config) {
                        Ok(mut capturer) => {
                            if let Ok(()) = capturer.start().await {
                                // Wait for frames to arrive
                                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                                let mut encoded_count = 0u64;
                                let mut encoded_sizes = Vec::new();
                                let target_frames = 5;

                                for _ in 0..target_frames {
                                    if let Some(mut frame) = capturer.recv_frame() {
                                        if let Err(e) = encoder.encode(&mut frame) {
                                            fail(&format!("Encode error: {e}"));
                                            break;
                                        }
                                        // Give the encoder a moment to produce output
                                        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                                    } else {
                                        // Wait a bit for more frames
                                        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                                    }
                                }

                                // Flush and collect encoded frames
                                #[cfg(target_os = "macos")]
                                let _ = encoder.flush();
                                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                                while let Some(ef) = encoder.recv_frame() {
                                    encoded_count += 1;
                                    let kf = if ef.is_keyframe { " [keyframe]" } else { "" };
                                    info(&format!(
                                        "Encoded frame {}: {} bytes{kf}",
                                        ef.frame_number,
                                        ef.data.len()
                                    ));
                                    encoded_sizes.push(ef.data.len());
                                }

                                results.encoded_frames = encoded_count;
                                if encoded_count > 0 {
                                    let total_bytes: usize = encoded_sizes.iter().sum();
                                    let avg_bytes = total_bytes / encoded_count as usize;
                                    pass(&format!(
                                        "Encoded {} frames, avg size {} bytes ({:.1} KB)",
                                        encoded_count, avg_bytes, avg_bytes as f64 / 1024.0
                                    ));
                                    results.encode = Some(true);
                                } else {
                                    fail("No encoded frames produced");
                                    results.encode = Some(false);
                                }

                                let _ = capturer.stop();
                            } else {
                                fail("Could not start capture for encoding test");
                                results.encode = Some(false);
                            }
                        }
                        Err(e) => {
                            fail(&format!("Could not create capturer for encoding test: {e}"));
                            results.encode = Some(false);
                        }
                    }
                }
                Err(e) => {
                    fail(&format!("Failed to create encoder: {e}"));
                    results.encode = Some(false);
                }
            }
        } else {
            fail("No display available for encoding test");
            results.encode = Some(false);
        }
    }

    // -----------------------------------------------------------------------
    // Summary
    // -----------------------------------------------------------------------
    header("Summary");

    let mut all_pass = true;

    let perm_status = if results.permission { format!("{GREEN}OK{RESET}") } else { all_pass = false; format!("{RED}FAIL{RESET}") };
    let disp_status = if results.displays_found { format!("{GREEN}OK{RESET}") } else { all_pass = false; format!("{RED}FAIL{RESET}") };
    let vd_status = match results.virtual_display {
        Some(true) => format!("{GREEN}OK{RESET}"),
        Some(false) => { all_pass = false; format!("{RED}FAIL{RESET}") },
        None => format!("{YELLOW}N/A{RESET}"),
    };
    let cap_status = match results.capture {
        Some(true) => format!("{GREEN}OK{RESET} ({} frames)", results.capture_frames),
        Some(false) => { all_pass = false; format!("{RED}FAIL{RESET}") },
        None => format!("{YELLOW}SKIP{RESET}"),
    };
    let enc_status = match results.encode {
        Some(true) => format!("{GREEN}OK{RESET} ({} frames)", results.encoded_frames),
        Some(false) => { all_pass = false; format!("{RED}FAIL{RESET}") },
        None => format!("{YELLOW}SKIP{RESET}"),
    };

    println!("  Screen recording permission:  {perm_status}");
    println!("  Display enumeration:          {disp_status}");
    println!("  Virtual display creation:     {vd_status}");
    println!("  Screen capture:               {cap_status}");
    println!("  H.265 encoding:               {enc_status}");
    println!();

    if all_pass {
        println!("  {GREEN}{BOLD}All tests passed. This machine is ready for Bridge server.{RESET}");
    } else {
        println!("  {RED}{BOLD}Some tests failed. See details above.{RESET}");

        if !results.permission {
            println!();
            println!("  {YELLOW}Tip:{RESET} Grant screen recording permission in System Settings");
            println!("       > Privacy & Security > Screen Recording.");
        }
        if results.virtual_display == Some(false) {
            println!();
            println!("  {YELLOW}Tip:{RESET} Virtual display may fail on Mac Mini via SSH.");
            println!("       Try running from a GUI terminal or as a LaunchAgent.");
            println!("       A dummy HDMI dongle is an alternative solution.");
        }
        if results.capture == Some(false) && results.permission {
            println!();
            println!("  {YELLOW}Tip:{RESET} Capture failed despite having permission.");
            println!("       This may happen on headless setups without any display.");
        }
    }

    println!();
}
