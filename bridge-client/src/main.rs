//! Bridge Client
//!
//! The client receives video from the server and displays it full-screen,
//! captures keyboard/mouse input and sends to the server,
//! and receives audio from the server and plays it.
//!
//! Currently macOS-only (Metal + Cocoa). Cross-platform client (wgpu) planned.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("bridge-client currently requires macOS (Metal + Cocoa window system).");
    eprintln!("Cross-platform client with wgpu is planned for a future release.");
    eprintln!("On Linux, use bridge-server to stream, and connect from a Mac client.");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
mod macos_client {

use anyhow::Result;
use bridge_common::{
    ControlMessage, LatencyReport, VideoFrameHeader,
};
use bridge_transport::{
    BridgeConnection, ServiceBrowser, TransportConfig, DEFAULT_CONTROL_PORT,
    is_thunderbolt_connection,
};
use bridge_video::{MetalDisplay, VideoDecoder, DecodedFrame};
// TODO: Input disabled - focus on video first
#[allow(unused_imports)]
use bridge_input::{CaptureConfig, InputCapturer};
use bridge_audio::{AudioPlayer, AudioPacket};
use clap::Parser;
use anyhow::anyhow;
use std::collections::HashMap;
use std::net::SocketAddr;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::FmtSubscriber;

// macOS imports for window creation
use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_foundation::{MainThreadMarker, NSString, NSPoint, NSSize, NSRect};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSWindow, NSWindowStyleMask,
    NSBackingStoreType, NSView, NSEventMask,
};
use objc2_quartz_core::CAMetalLayer;
use metal::foreign_types::ForeignType;
use metal::MetalLayer;

/// Reassembles fragmented video frames
struct FrameReassembler {
    /// In-progress frames: frame_number -> (expected_fragment_count, received_fragments)
    pending_frames: HashMap<u64, PendingFrame>,
    /// Last completed frame number (for ordering)
    last_completed_frame: u64,
    /// Maximum age of pending frames before they're dropped (in microseconds)
    max_age_us: u64,
}

struct PendingFrame {
    header: VideoFrameHeader,
    fragments: Vec<Option<Vec<u8>>>,
    first_received_us: u64,
    received_count: u16,
}

impl FrameReassembler {
    fn new() -> Self {
        Self {
            pending_frames: HashMap::new(),
            last_completed_frame: 0,
            max_age_us: 200_000, // 200ms timeout for incomplete frames
        }
    }

    /// Add a fragment, returns complete frame data if all fragments received
    fn add_fragment(&mut self, header: VideoFrameHeader, data: Vec<u8>) -> Option<(VideoFrameHeader, Vec<u8>)> {
        let now_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;

        // Clean up old pending frames
        self.cleanup_old_frames(now_us);

        // Skip frames older than what we've already displayed
        // Note: use < not <= because frame 0 is valid when last_completed_frame starts at 0
        if header.frame_number < self.last_completed_frame {
            return None;
        }

        // Single fragment frame - return immediately
        if header.fragment_count == 1 {
            self.last_completed_frame = header.frame_number;
            return Some((header, data));
        }

        // Multi-fragment frame - add to pending
        let pending = self.pending_frames
            .entry(header.frame_number)
            .or_insert_with(|| PendingFrame {
                header,
                fragments: vec![None; header.fragment_count as usize],
                first_received_us: now_us,
                received_count: 0,
            });

        // Store this fragment
        let idx = header.fragment_index as usize;
        if idx < pending.fragments.len() && pending.fragments[idx].is_none() {
            pending.fragments[idx] = Some(data);
            pending.received_count += 1;

            // All fragments carry the is_keyframe flag now
            if header.is_keyframe {
                pending.header.is_keyframe = true;
            }
        }

        // Check if complete
        if pending.received_count == pending.header.fragment_count {
            let frame_number = pending.header.frame_number;
            let completed_header = pending.header;
            debug!("Frame {} complete ({} fragments, {} bytes, keyframe={})",
                   frame_number, completed_header.fragment_count,
                   completed_header.frame_size, completed_header.is_keyframe);

            // Assemble all fragments
            let total_size = pending.header.frame_size as usize;
            let mut complete_data = Vec::with_capacity(total_size);

            for fragment in &pending.fragments {
                if let Some(frag_data) = fragment {
                    complete_data.extend_from_slice(frag_data);
                }
            }

            // Remove from pending
            self.pending_frames.remove(&frame_number);
            self.last_completed_frame = frame_number;

            return Some((completed_header, complete_data));
        }

        None
    }

    fn cleanup_old_frames(&mut self, now_us: u64) {
        self.pending_frames.retain(|_, pending| {
            now_us - pending.first_received_us < self.max_age_us
        });
    }

    /// Get statistics about pending frames
    fn pending_count(&self) -> usize {
        self.pending_frames.len()
    }
}

#[derive(Parser, Debug)]
#[command(name = "bridge-client")]
#[command(about = "Bridge client application for iMac", long_about = None)]
struct Args {
    /// Server address (IP:port) - if not specified, uses discovery
    #[arg(short, long)]
    server: Option<String>,

    /// Client name
    #[arg(short, long, default_value = "Bridge Client")]
    name: String,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Don't capture input (view-only mode)
    #[arg(long)]
    view_only: bool,

    /// Windowed mode (not fullscreen)
    #[arg(long)]
    windowed: bool,
}

/// Create an NSWindow with a CAMetalLayer for video display
fn create_window(mtm: MainThreadMarker, width: u32, height: u32, fullscreen: bool) -> (Retained<NSWindow>, MetalLayer) {
    // Get screen frame for fullscreen
    let screen_frame = if fullscreen {
        if let Some(screen) = NSScreen::mainScreen(mtm) {
            screen.frame()
        } else {
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width as f64, height as f64))
        }
    } else {
        // On Retina displays, window dimensions are in points (not pixels).
        // Divide by screen backing scale to get a window that's exactly
        // width x height pixels.
        let scale = NSScreen::mainScreen(mtm)
            .map(|s| s.backingScaleFactor())
            .unwrap_or(1.0);
        let pt_w = width as f64 / scale;
        let pt_h = height as f64 / scale;
        NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(pt_w, pt_h))
    };

    // Create window
    let style = if fullscreen {
        NSWindowStyleMask::Borderless
    } else {
        NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::Miniaturizable
    };

    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            screen_frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };

    // Set window properties
    let title = NSString::from_str("Bridge");
    window.setTitle(&title);
    window.setAcceptsMouseMovedEvents(true);

    if fullscreen {
        window.setCollectionBehavior(
            objc2_app_kit::NSWindowCollectionBehavior::FullScreenPrimary
        );
    }

    // Create a content view with layer backing
    let content_view = {
        let view = NSView::initWithFrame(NSView::alloc(mtm), screen_frame);
        view.setWantsLayer(true);
        view
    };

    // Create CAMetalLayer
    let ca_layer = CAMetalLayer::new();

    // Wrap in metal-rs MetalLayer
    let metal_layer = unsafe {
        MetalLayer::from_ptr(Retained::into_raw(ca_layer) as *mut _)
    };

    // Set the layer on the view
    unsafe {
        // Get the raw CAMetalLayer pointer from MetalLayer
        let layer_ptr = metal_layer.as_ptr();
        let layer: &CAMetalLayer = &*(layer_ptr as *const CAMetalLayer);
        content_view.setLayer(Some(layer));
    }

    window.setContentView(Some(&content_view));

    // Show window
    window.makeKeyAndOrderFront(None);
    window.center();

    (window, metal_layer)
}

// Import NSScreen
use objc2_app_kit::NSScreen;

/// Maximum number of reconnection attempts before giving up
const MAX_RECONNECT_ATTEMPTS: u32 = 10;
/// Delay between reconnection attempts
const RECONNECT_DELAY_MS: u64 = 2000;

pub fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let level = if args.verbose { Level::DEBUG } else { Level::INFO };
    let subscriber = FmtSubscriber::builder()
        .with_max_level(level)
        .with_target(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("Bridge Client starting...");
    info!("  Name: {}", args.name);
    if let Some(server) = &args.server {
        info!("  Server: {}", server);
    } else {
        info!("  Server: auto-discover");
    }

    // Check permissions
    if !args.view_only && !bridge_input::has_accessibility_permission() {
        warn!("Accessibility permission not granted - input capture will fail");
        warn!("Enable in System Preferences > Security & Privacy > Privacy > Accessibility");
    }

    // Get main thread marker - required for macOS GUI operations
    let mtm = MainThreadMarker::new().expect("Must run on main thread");

    // Initialize NSApplication
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);

    // Build and run the tokio runtime for async operations
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // Run with auto-reconnect (only for transport/connection errors, not startup failures)
    runtime.block_on(async {
        let mut attempt = 0u32;
        loop {
            match async_main(&args, mtm).await {
                Ok(()) => {
                    info!("Session ended cleanly");
                    break Ok(());
                }
                Err(e) => {
                    // Check if this is a transient transport error worth retrying.
                    // Don't retry permanent failures like bad args, init errors, etc.
                    let msg = e.to_string();
                    let is_transient = msg.contains("channel failed")
                        || msg.contains("channel lost")
                        || msg.contains("connection")
                        || msg.contains("Connection")
                        || msg.contains("timeout")
                        || msg.contains("Timeout")
                        || msg.contains("reset")
                        || msg.contains("broken pipe");
                    if !is_transient {
                        error!("Fatal error (not retrying): {}", e);
                        break Err(e);
                    }
                    attempt += 1;
                    if attempt >= MAX_RECONNECT_ATTEMPTS {
                        error!("Max reconnect attempts ({}) reached, giving up: {}", MAX_RECONNECT_ATTEMPTS, e);
                        break Err(e);
                    }
                    warn!("Session failed (attempt {}/{}): {}. Reconnecting in {}ms...",
                          attempt, MAX_RECONNECT_ATTEMPTS, e, RECONNECT_DELAY_MS);
                    tokio::time::sleep(std::time::Duration::from_millis(RECONNECT_DELAY_MS)).await;
                }
            }
        }
    })
}

async fn async_main(args: &Args, mtm: MainThreadMarker) -> Result<()> {
    // Determine server address
    let server_addr = if let Some(ref server) = args.server {
        match server.parse::<SocketAddr>() {
            Ok(addr) => addr,
            Err(_) => {
                // Try adding default port
                format!("{}:{}", server, DEFAULT_CONTROL_PORT)
                    .parse()
                    .map_err(|_| anyhow!("Invalid server address: {}", server))?
            }
        }
    } else {
        // Use service discovery
        info!("Searching for Bridge servers...");
        discover_server().await?
    };

    info!("Connecting to server at {}", server_addr);

    // Detect local display resolution to request matching capture from server
    let displays = bridge_video::get_displays();
    let main_display = displays.iter().find(|d| d.is_main).or_else(|| displays.first());
    let (display_width, display_height) = match main_display {
        Some(d) => {
            info!("Detected local display: {}x{} (native pixels)", d.width, d.height);
            (d.width, d.height)
        }
        None => {
            warn!("Could not detect display, defaulting to 1920x1080");
            (1920, 1080)
        }
    };

    // Detect if connecting via Thunderbolt
    let is_thunderbolt = bridge_transport::is_thunderbolt_connection(&server_addr);
    if is_thunderbolt {
        info!("Connecting via Thunderbolt to {}", server_addr);
    }

    // Create video config with client's display resolution
    let requested_video_config = bridge_common::VideoConfig {
        width: display_width,
        height: display_height,
        fps: 60,
        codec: bridge_common::VideoCodec::H265,
        bitrate: if is_thunderbolt {
            500_000_000 // 500 Mbps - near-lossless for Thunderbolt
        } else {
            60_000_000 // 60 Mbps - good quality for 4K60 H.265
        },
        pixel_format: bridge_common::PixelFormat::Bgra8,
    };

    // Connect to server — use larger packets and buffers for Thunderbolt
    let mut transport_config = TransportConfig::default();
    if is_thunderbolt {
        transport_config.send_buffer_size = 64 * 1024 * 1024; // 64MB
        transport_config.recv_buffer_size = 64 * 1024 * 1024;
        info!("Thunderbolt: using MTU-safe packet size with 64MB socket buffers");
    }
    let mut conn = BridgeConnection::new(server_addr, transport_config);

    let welcome = conn.connect_with_config(
        &args.name,
        requested_video_config,
        bridge_common::AudioConfig::default(),
    ).await?;
    info!("Connected to server: {}", welcome.server_name);
    info!("  Video: {}x{} @ {}fps",
        welcome.video_config.width,
        welcome.video_config.height,
        welcome.video_config.fps
    );

    let video_config = welcome.video_config;
    let _audio_config = welcome.audio_config;

    // Send ping packets on UDP channels so server learns our address
    // Retry multiple times to handle race condition where server may not be ready yet
    info!("Sending UDP channel pings...");
    for attempt in 0..10 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if let Some(video_ch) = conn.video_channel() {
            if let Err(e) = video_ch.send(bridge_common::PacketType::Video, b"ping").await {
                error!("Failed to send video ping: {}", e);
            }
        }
        // Audio ping skipped — audio disabled
        // TODO: Input disabled - focus on video first
        // if let Some(input_ch) = conn.input_channel() {
        //     if let Err(e) = input_ch.send(bridge_common::PacketType::Input, b"ping").await {
        //         error!("Failed to send input ping: {}", e);
        //     }
        // }
    }
    debug!("UDP pings sent (10 attempts over 900ms)");

    // Create window — use received config dimensions (server may have negotiated different resolution)
    let fullscreen = !args.windowed;
    let window_w = video_config.width;
    let window_h = video_config.height;
    let (_window, metal_layer) = create_window(mtm, window_w, window_h, fullscreen);
    info!("Window created: {}x{}, fullscreen={}", window_w, window_h, fullscreen);

    // Initialize display with negotiated video resolution
    let mut display = MetalDisplay::new(video_config.width, video_config.height)?;
    display.set_layer(metal_layer);
    info!("Metal display initialized with layer");

    // Decoder will be created lazily when we receive first frame with actual dimensions
    let mut decoder: Option<VideoDecoder> = None;
    let video_codec = video_config.codec;
    let mut _actual_frame_width: Option<u32> = None;
    let mut _actual_frame_height: Option<u32> = None;

    // Audio disabled — focusing on video first
    let mut player: Option<AudioPlayer> = None;

    // TODO: Input capture disabled - focus on video first
    let _input_capturer: Option<InputCapturer> = None;
    // let mut input_capturer = if !args.view_only {
    //     let capture_config = CaptureConfig {
    //         listen_only: false,
    //         ..Default::default()
    //     };
    //     let mut capturer = InputCapturer::new(capture_config)?;
    //     capturer.start()?;
    //     Some(capturer)
    // } else {
    //     None
    // };

    info!("Client components initialized");

    // Spawn a dedicated task to read control messages.
    // recv_control() is NOT cancel-safe (two separate read_exact calls),
    // so we can't use it in tokio::select!. Instead, take the recv stream
    // and run a continuous reader that feeds an mpsc channel.
    let recv_stream = conn.control()
        .and_then(|ctrl| ctrl.take_recv_stream())
        .ok_or_else(|| anyhow!("No control channel"))?;
    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel::<ControlMessage>();
    tokio::spawn(async move {
        let mut stream = recv_stream;
        loop {
            match bridge_transport::QuicConnection::recv_control_from_stream(&mut stream).await {
                Ok(msg) => {
                    if control_tx.send(msg).is_err() {
                        break; // main loop dropped receiver
                    }
                }
                Err(e) => {
                    warn!("Control channel read error: {}", e);
                    break;
                }
            }
        }
    });

    // Request stream start (send_control still works — only recv was taken)
    conn.send_control(ControlMessage::StartStream).await?;
    info!("Stream started");

    // Request a keyframe now that we're ready to receive
    // This handles the case where the first keyframe was sent before we were ready
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    conn.send_control(ControlMessage::RequestKeyframe).await?;
    debug!("Keyframe requested");

    // Frame reassembler for handling fragmented video frames
    let mut frame_reassembler = FrameReassembler::new();

    // Latency tracking - only track what we can actually measure locally
    let mut decode_latency_samples: Vec<u64> = Vec::with_capacity(60);
    let mut last_latency_report = tokio::time::Instant::now();
    let latency_report_interval = tokio::time::Duration::from_secs(2); // Report less frequently

    // Packet loss tracking
    let mut expected_frame: u64 = 0;
    let mut received_frames: u64 = 0;
    let mut dropped_frames: u64 = 0;
    let mut last_keyframe_request = tokio::time::Instant::now();

    // Set up signal handlers for graceful shutdown
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("Failed to set up SIGTERM handler");
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .expect("Failed to set up SIGINT handler");

    // Main client loop — event-driven with tokio::select!
    loop {
        // Process macOS events first (non-blocking) to keep the window responsive
        unsafe {
            let app = NSApplication::sharedApplication(mtm);
            loop {
                let event = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    None, // Don't wait
                    objc2_foundation::NSDefaultRunLoopMode,
                    true,
                );
                match event {
                    Some(e) => app.sendEvent(&e),
                    None => break,
                }
            }
            app.updateWindows();
        }

        // Wait for video data, control messages, or signals
        // Use select! to avoid busy-polling — wakes on first available event.
        // control_rx.recv() is cancel-safe (mpsc channel), unlike raw recv_control().
        tokio::select! {
            biased;

            _ = sigterm.recv() => {
                info!("Received SIGTERM, gracefully disconnecting...");
                break;
            }
            _ = sigint.recv() => {
                info!("Received SIGINT, gracefully disconnecting...");
                break;
            }
            msg = control_rx.recv() => {
                match msg {
                    Some(ControlMessage::Disconnect) => {
                        info!("Server sent Disconnect, ending session");
                        break;
                    }
                    Some(other) => {
                        debug!("Control message from server: {:?}", other);
                    }
                    None => {
                        // Reader task exited — control channel lost
                        warn!("Control channel closed, reconnecting...");
                        return Err(anyhow!("Control channel lost"));
                    }
                }
            }
            // Wake every 8ms to pump macOS events even when no data arrives
            // (60fps = 16.7ms per frame, so 8ms gives responsive UI + catches frames quickly)
            _ = tokio::time::sleep(std::time::Duration::from_millis(8)) => {}
        }

        // Receive video frames — drain all available packets, decode all, render only latest
        if let Some(video_ch) = conn.video_channel() {
            let mut packets_this_round = 0u32;
            let mut complete_frames: Vec<(VideoFrameHeader, Vec<u8>)> = Vec::new();

            // Phase 1: Drain all available packets and collect complete frames
            loop {
                // Short timeout: we already waited in the select! above,
                // so just drain what's available without blocking long
                let recv_result = tokio::time::timeout(
                    std::time::Duration::from_millis(if packets_this_round == 0 { 2 } else { 0 }),
                    video_ch.recv()
                ).await;

                match recv_result {
                    Ok(Ok((header, data))) => {
                        if header.packet_type != bridge_common::PacketType::Video {
                            continue;
                        }
                        packets_this_round += 1;

                        // Parse the VideoFrameHeader from the data
                        let video_header: VideoFrameHeader = match bincode::deserialize(&data[..VideoFrameHeader::SIZE.min(data.len())]) {
                            Ok(h) => h,
                            Err(e) => {
                                warn!("Failed to parse video frame header: {}", e);
                                continue;
                            }
                        };

                        if video_header.fragment_index == 0 {
                            debug!("Receiving frame {} ({} fragments, keyframe={})",
                                   video_header.frame_number, video_header.fragment_count, video_header.is_keyframe);
                        }

                        let fragment_data = data[VideoFrameHeader::SIZE.min(data.len())..].to_vec();

                        if let Some((complete_header, complete_data)) = frame_reassembler.add_fragment(video_header, fragment_data) {
                            if complete_header.frame_number > expected_frame {
                                dropped_frames += complete_header.frame_number - expected_frame;
                            }
                            expected_frame = complete_header.frame_number + 1;
                            received_frames += 1;
                            complete_frames.push((complete_header, complete_data));
                        }
                    }
                    Ok(Err(e)) => {
                        error!("Video channel error: {}", e);
                        return Err(anyhow!("Video channel failed: {}", e));
                    }
                    Err(_) => break,
                }
            }

            // Phase 2: Decode ALL frames (H.265 needs reference chain), render only the last
            let frame_count = complete_frames.len();
            for (i, (complete_header, complete_data)) in complete_frames.into_iter().enumerate() {
                let is_last = i == frame_count - 1;

                if is_last {
                    info!("Frame {} complete ({} bytes, keyframe={}, {}x{}, {} pkts, skipped {})",
                          complete_header.frame_number, complete_data.len(),
                          complete_header.is_keyframe,
                          complete_header.width, complete_header.height,
                          packets_this_round, frame_count - 1);
                }

                // Create decoder lazily
                if decoder.is_none() && video_codec != bridge_common::VideoCodec::Raw {
                    info!("Creating decoder for {}x{} frames",
                          complete_header.width, complete_header.height);
                    _actual_frame_width = Some(complete_header.width);
                    _actual_frame_height = Some(complete_header.height);
                    decoder = Some(VideoDecoder::new(
                        complete_header.width,
                        complete_header.height,
                        video_codec,
                    )?);
                }

                // Decode every frame to maintain reference chain
                let decode_start = std::time::Instant::now();
                let decoded = if let Some(ref mut dec) = decoder {
                    let encoded = bridge_video::EncodedFrame {
                        data: complete_data.into(),
                        pts_us: complete_header.pts_us,
                        dts_us: complete_header.pts_us,
                        is_keyframe: complete_header.is_keyframe,
                        frame_number: complete_header.frame_number,
                    };
                    match dec.decode(&encoded) {
                        Ok(()) => {
                            // Only retrieve decoded output for the last frame (saves work)
                            if is_last { dec.recv_frame() } else { let _ = dec.recv_frame(); None }
                        }
                        Err(e) => {
                            warn!("Decode error on frame {}: {} — requesting keyframe",
                                  complete_header.frame_number, e);
                            if last_keyframe_request.elapsed() > std::time::Duration::from_secs(1) {
                                let _ = conn.send_control(ControlMessage::RequestKeyframe).await;
                                last_keyframe_request = tokio::time::Instant::now();
                            }
                            // Don't call recv_frame after decode error — would get stale output
                            None
                        }
                    }
                } else if is_last {
                    Some(DecodedFrame {
                        data: complete_data,
                        width: complete_header.width,
                        height: complete_header.height,
                        bytes_per_row: complete_header.width * 4,
                        pts_us: complete_header.pts_us,
                        io_surface: None,
                        cv_pixel_buffer: None,
                    })
                } else {
                    None
                };
                let decode_time_us = decode_start.elapsed().as_micros() as u64;

                // Request keyframe if decoder needs one
                if let Some(ref dec) = decoder {
                    if dec.needs_keyframe() && last_keyframe_request.elapsed() > std::time::Duration::from_secs(1) {
                        info!("Decoder needs keyframe, requesting from server");
                        conn.send_control(ControlMessage::RequestKeyframe).await?;
                        last_keyframe_request = tokio::time::Instant::now();
                    }
                }

                // Only render the latest frame
                if let Some(frame) = decoded {
                    debug!("Decoded frame: {}x{}, rendering...", frame.width, frame.height);
                    display.render(&frame)?;

                    decode_latency_samples.push(decode_time_us);
                    if decode_latency_samples.len() > 60 {
                        decode_latency_samples.remove(0);
                    }
                }
            }
        }

        // Receive audio
        if let (Some(ref mut p), Some(audio_ch)) = (&mut player, conn.audio_channel()) {
            match tokio::time::timeout(
                std::time::Duration::from_millis(1),
                audio_ch.recv()
            ).await {
                Ok(Ok((header, data))) => {
                    if header.packet_type == bridge_common::PacketType::Audio {
                        let packet = AudioPacket {
                            data,
                            pts_us: header.timestamp_us,
                            sample_count: header.payload_len / 8,
                            sequence: header.sequence,
                        };
                        let _ = p.play(&packet);
                    }
                }
                Ok(Err(_)) => {}
                Err(_) => {}
            }
        }

        // TODO: Input disabled - focus on video first
        // if let Some(ref capturer) = input_capturer {
        //     while let Some(event) = capturer.recv_event() {
        //         if let Some(input_ch) = conn.input_channel() {
        //             let data = event.to_bytes()?;
        //             let _ = input_ch.send(bridge_common::PacketType::Input, &data).await;
        //         }
        //     }
        // }

        // Send periodic latency reports
        // Note: We can't measure true network latency without clock sync (NTP/PTP)
        // So we only report what we can actually measure: decode time and packet loss
        if last_latency_report.elapsed() > latency_report_interval {
            let avg_decode: u64 = if !decode_latency_samples.is_empty() {
                decode_latency_samples.iter().sum::<u64>() / decode_latency_samples.len() as u64
            } else {
                0
            };

            // Calculate jitter (variance in decode time as a proxy)
            let jitter = if decode_latency_samples.len() > 1 {
                let mean = avg_decode as f64;
                let variance: f64 = decode_latency_samples.iter()
                    .map(|&x| {
                        let diff = x as f64 - mean;
                        diff * diff
                    })
                    .sum::<f64>() / decode_latency_samples.len() as f64;
                variance.sqrt() as u64
            } else {
                0
            };

            // Calculate packet loss
            let total_frames = received_frames + dropped_frames;
            let packet_loss = if total_frames > 0 {
                dropped_frames as f32 / total_frames as f32
            } else {
                0.0
            };

            // Report with placeholder RTT - we can't measure this without clock sync
            // Use a reasonable estimate for WiFi (5-15ms typical)
            let estimated_rtt_us = 10_000u64; // 10ms estimate

            let report = LatencyReport {
                rtt_us: estimated_rtt_us,
                decode_latency_us: avg_decode,
                display_latency_us: 2000, // ~2ms for Metal rendering
                packet_loss,
                jitter_us: jitter,
            };

            debug!("Stats: decode={}us ({:.1}ms), jitter={}us, loss={:.1}%, pending={}",
                avg_decode, avg_decode as f64 / 1000.0,
                jitter,
                packet_loss * 100.0,
                frame_reassembler.pending_count());

            conn.send_control(ControlMessage::LatencyReport(report)).await?;
            last_latency_report = tokio::time::Instant::now();

            // Reset packet loss counters for next interval
            received_frames = 0;
            dropped_frames = 0;
        }

    }

    // Clean up
    // TODO: Input disabled - focus on video first
    // if let Some(ref mut capturer) = input_capturer {
    //     capturer.stop()?;
    // }
    if let Some(ref mut p) = player {
        let _ = p.stop();
    }
    conn.disconnect().await?;

    info!("Client exiting");
    Ok(())
}

async fn discover_server() -> Result<SocketAddr> {
    let browser = ServiceBrowser::new().await?;

    let timeout = tokio::time::Duration::from_secs(10);
    let start = tokio::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!("No Bridge servers found"));
        }

        let servers = browser.servers().await;
        if !servers.is_empty() {
            // Prefer servers reachable via Thunderbolt (169.254.x.x)
            if let Some(tb_server) = servers.iter().find(|s| is_thunderbolt_connection(&s.address)) {
                info!("Found server via Thunderbolt: {} at {}", tb_server.name, tb_server.address);
                return Ok(tb_server.address);
            }

            // Fall back to first available server
            let server = &servers[0];
            info!("Found server via network: {} at {}", server.name, server.address);
            return Ok(server.address);
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
}

} // mod macos_client

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    macos_client::main()
}
