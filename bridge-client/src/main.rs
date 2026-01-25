//! Bridge Client - iMac application
//!
//! The client runs on the iMac and:
//! - Receives video from the server and displays it full-screen
//! - Captures keyboard/mouse input and sends to the server
//! - Receives audio from the server and plays it

use anyhow::Result;
use bridge_common::{
    ControlMessage, LatencyReport, VideoFrameHeader, elapsed_us,
};
use bridge_transport::{
    BridgeConnection, ServiceBrowser, TransportConfig, DEFAULT_CONTROL_PORT,
};
use bridge_video::{MetalDisplay, VideoDecoder, DecodedFrame};
use bridge_input::{CaptureConfig, InputCapturer};
use bridge_audio::{PlaybackConfig, AudioPlayer, AudioPacket};
use clap::Parser;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use parking_lot::Mutex;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::FmtSubscriber;

// macOS imports for window creation
use objc2::rc::Retained;
use objc2::{ClassType, MainThreadOnly};
use objc2_foundation::{MainThreadMarker, NSString, NSPoint, NSSize, NSRect};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSWindow, NSWindowStyleMask,
    NSBackingStoreType, NSView,
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
            max_age_us: 100_000, // 100ms timeout for incomplete frames
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
        if header.frame_number <= self.last_completed_frame {
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
        }

        // Check if complete
        if pending.received_count == pending.header.fragment_count {
            let frame_number = pending.header.frame_number;
            let completed_header = pending.header;

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
        unsafe {
            if let Some(screen) = NSScreen::mainScreen(mtm) {
                screen.frame()
            } else {
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width as f64, height as f64))
            }
        }
    } else {
        NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(width as f64, height as f64))
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
    unsafe {
        let title = NSString::from_str("Bridge");
        window.setTitle(&title);
        window.setAcceptsMouseMovedEvents(true);

        if fullscreen {
            window.setLevel(1000); // Above other windows
            window.setCollectionBehavior(
                objc2_app_kit::NSWindowCollectionBehavior::FullScreenPrimary
            );
        }
    }

    // Create a content view with layer backing
    let content_view = unsafe {
        let view = NSView::initWithFrame(NSView::alloc(mtm), screen_frame);
        view.setWantsLayer(true);
        view
    };

    // Create CAMetalLayer
    let ca_layer = unsafe { CAMetalLayer::new() };

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

        window.setContentView(Some(&content_view));
    }

    // Show window
    unsafe {
        window.makeKeyAndOrderFront(None);
        window.center();
    }

    (window, metal_layer)
}

// Import NSScreen
use objc2_app_kit::NSScreen;

fn main() -> Result<()> {
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
    unsafe {
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        app.activateIgnoringOtherApps(true);
    }

    // Build and run the tokio runtime for async operations
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // Run the async main logic
    let result = runtime.block_on(async_main(args, mtm));

    result
}

async fn async_main(args: Args, mtm: MainThreadMarker) -> Result<()> {
    // Determine server address
    let server_addr = if let Some(server) = args.server {
        server.parse::<SocketAddr>()
            .unwrap_or_else(|_| {
                // Try adding default port
                format!("{}:{}", server, DEFAULT_CONTROL_PORT)
                    .parse()
                    .expect("Invalid server address")
            })
    } else {
        // Use service discovery
        info!("Searching for Bridge servers...");
        discover_server().await?
    };

    info!("Connecting to server at {}", server_addr);

    // Detect local display resolution to request from server
    let displays = bridge_video::get_displays();
    let main_display = displays.iter().find(|d| d.is_main).or_else(|| displays.first());
    let (display_width, display_height) = match main_display {
        Some(d) => {
            info!("Detected display: {}x{}", d.width, d.height);
            (d.width, d.height)
        }
        None => {
            warn!("Could not detect display, using 1920x1080");
            (1920, 1080)
        }
    };

    // Create video config with client's display resolution
    let requested_video_config = bridge_common::VideoConfig {
        width: display_width,
        height: display_height,
        fps: 60,
        codec: bridge_common::VideoCodec::H265,
        bitrate: 50_000_000,
        pixel_format: bridge_common::PixelFormat::Bgra8,
    };

    // Connect to server
    let transport_config = TransportConfig::default();
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
    let audio_config = welcome.audio_config;

    // Create window with Metal layer
    let fullscreen = !args.windowed;
    let (window, metal_layer) = create_window(mtm, video_config.width, video_config.height, fullscreen);
    info!("Window created: {}x{}, fullscreen={}", video_config.width, video_config.height, fullscreen);

    // Initialize display and set the layer
    let mut display = MetalDisplay::new(video_config.width, video_config.height)?;
    display.set_layer(metal_layer);
    info!("Metal display initialized with layer");

    let mut decoder = if video_config.codec != bridge_common::VideoCodec::Raw {
        Some(VideoDecoder::new(
            video_config.width,
            video_config.height,
            video_config.codec,
        )?)
    } else {
        None
    };

    let playback_config = PlaybackConfig::from(&audio_config);
    let mut player = AudioPlayer::new(playback_config)?;
    player.start()?;

    let mut input_capturer = if !args.view_only {
        let capture_config = CaptureConfig {
            listen_only: false,
            ..Default::default()
        };
        let mut capturer = InputCapturer::new(capture_config)?;
        capturer.start()?;
        Some(capturer)
    } else {
        None
    };

    info!("Client components initialized");

    // Send ping packets on UDP channels so server learns our address
    info!("Sending UDP channel pings...");
    if let Some(video_ch) = conn.video_channel() {
        let _ = video_ch.send(bridge_common::PacketType::Video, b"ping").await;
    }
    if let Some(audio_ch) = conn.audio_channel() {
        let _ = audio_ch.send(bridge_common::PacketType::Audio, b"ping").await;
    }
    if let Some(input_ch) = conn.input_channel() {
        let _ = input_ch.send(bridge_common::PacketType::Input, b"ping").await;
    }
    // Give server time to receive pings
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Request stream start
    conn.send_control(ControlMessage::StartStream).await?;
    info!("Stream started");

    // Frame reassembler for handling fragmented video frames
    let mut frame_reassembler = FrameReassembler::new();

    // Latency tracking
    let mut latency_samples: Vec<u64> = Vec::with_capacity(60);
    let mut decode_latency_samples: Vec<u64> = Vec::with_capacity(60);
    let mut last_latency_report = tokio::time::Instant::now();
    let latency_report_interval = tokio::time::Duration::from_secs(1);

    // Packet loss tracking
    let mut expected_frame: u64 = 0;
    let mut received_frames: u64 = 0;
    let mut dropped_frames: u64 = 0;

    // Set up signal handlers for graceful shutdown
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("Failed to set up SIGTERM handler");
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .expect("Failed to set up SIGINT handler");

    let mut shutdown_requested = false;

    // Main client loop
    loop {
        // Check for shutdown signals (non-blocking)
        tokio::select! {
            biased;

            _ = sigterm.recv() => {
                info!("Received SIGTERM, gracefully disconnecting...");
                shutdown_requested = true;
            }
            _ = sigint.recv() => {
                info!("Received SIGINT, gracefully disconnecting...");
                shutdown_requested = true;
            }
            _ = tokio::time::sleep(std::time::Duration::from_micros(1)) => {
                // Continue with normal processing
            }
        }

        if shutdown_requested {
            break;
        }

        // Receive video frames
        if let Some(video_ch) = conn.video_channel() {
            match tokio::time::timeout(
                std::time::Duration::from_millis(10),
                video_ch.recv()
            ).await {
                Ok(Ok((header, data))) => {
                    if header.packet_type == bridge_common::PacketType::Video {
                        // Parse the VideoFrameHeader from the data
                        let video_header: VideoFrameHeader = match bincode::deserialize(&data[..VideoFrameHeader::SIZE.min(data.len())]) {
                            Ok(h) => h,
                            Err(e) => {
                                warn!("Failed to parse video frame header: {}", e);
                                continue;
                            }
                        };

                        // Extract fragment data (after header)
                        let fragment_data = data[VideoFrameHeader::SIZE.min(data.len())..].to_vec();

                        // Add fragment to reassembler
                        if let Some((complete_header, complete_data)) = frame_reassembler.add_fragment(video_header, fragment_data) {
                            // Track frame ordering for packet loss calculation
                            if complete_header.frame_number > expected_frame {
                                dropped_frames += complete_header.frame_number - expected_frame;
                            }
                            expected_frame = complete_header.frame_number + 1;
                            received_frames += 1;

                            // Decode frame
                            let decode_start = std::time::Instant::now();
                            let decoded = if let Some(ref mut dec) = decoder {
                                let encoded = bridge_video::EncodedFrame {
                                    data: complete_data.into(),
                                    pts_us: complete_header.pts_us,
                                    dts_us: complete_header.pts_us,
                                    is_keyframe: complete_header.is_keyframe,
                                    frame_number: complete_header.frame_number,
                                };

                                dec.decode(&encoded)?;
                                dec.recv_frame()
                            } else {
                                Some(DecodedFrame {
                                    data: complete_data,
                                    width: complete_header.width,
                                    height: complete_header.height,
                                    bytes_per_row: complete_header.width * 4,
                                    pts_us: complete_header.pts_us,
                                    io_surface: None,
                                })
                            };
                            let decode_time_us = decode_start.elapsed().as_micros() as u64;

                            if let Some(frame) = decoded {
                                display.render(&frame)?;

                                // Track latencies
                                let network_latency = elapsed_us(complete_header.pts_us);
                                latency_samples.push(network_latency);
                                decode_latency_samples.push(decode_time_us);

                                if latency_samples.len() > 60 {
                                    latency_samples.remove(0);
                                }
                                if decode_latency_samples.len() > 60 {
                                    decode_latency_samples.remove(0);
                                }
                            }
                        }
                    }
                }
                Ok(Err(e)) => {
                    error!("Video receive error: {}", e);
                    break;
                }
                Err(_) => {} // Timeout, continue
            }
        }

        // Receive audio
        if let Some(audio_ch) = conn.audio_channel() {
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
                        let _ = player.play(&packet);
                    }
                }
                Ok(Err(_)) => {}
                Err(_) => {}
            }
        }

        // Capture and send input events
        if let Some(ref capturer) = input_capturer {
            while let Some(event) = capturer.recv_event() {
                if let Some(input_ch) = conn.input_channel() {
                    let data = event.to_bytes()?;
                    let _ = input_ch.send(bridge_common::PacketType::Input, &data).await;
                }
            }
        }

        // Send periodic latency reports
        if last_latency_report.elapsed() > latency_report_interval && !latency_samples.is_empty() {
            let avg_latency: u64 = latency_samples.iter().sum::<u64>()
                / latency_samples.len() as u64;

            let avg_decode: u64 = if !decode_latency_samples.is_empty() {
                decode_latency_samples.iter().sum::<u64>() / decode_latency_samples.len() as u64
            } else {
                0
            };

            // Calculate jitter (variance in latency)
            let jitter = if latency_samples.len() > 1 {
                let mean = avg_latency as f64;
                let variance: f64 = latency_samples.iter()
                    .map(|&x| {
                        let diff = x as f64 - mean;
                        diff * diff
                    })
                    .sum::<f64>() / latency_samples.len() as f64;
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

            let report = LatencyReport {
                rtt_us: avg_latency * 2,
                decode_latency_us: avg_decode,
                display_latency_us: 2000, // ~2ms for Metal rendering
                packet_loss,
                jitter_us: jitter,
            };

            debug!("Latency: avg={}us ({:.1}ms), decode={}us, jitter={}us, loss={:.1}%, pending={}",
                avg_latency, avg_latency as f64 / 1000.0,
                avg_decode, jitter,
                packet_loss * 100.0,
                frame_reassembler.pending_count());

            conn.send_control(ControlMessage::LatencyReport(report)).await?;
            last_latency_report = tokio::time::Instant::now();

            // Reset packet loss counters for next interval
            received_frames = 0;
            dropped_frames = 0;
        }

        // Small yield
        tokio::task::yield_now().await;
    }

    // Clean up
    if let Some(ref mut capturer) = input_capturer {
        capturer.stop()?;
    }
    player.stop()?;
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
        if let Some(server) = servers.first() {
            info!("Found server: {} at {}", server.name, server.address);
            return Ok(server.address);
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
}
