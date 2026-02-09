//! Bridge Server - Mac Mini daemon
//!
//! The server runs on the Mac Mini and:
//! - Captures the screen and streams video to the client
//! - Receives input events from the client and injects them
//! - Captures system audio and streams to the client

use anyhow::Result;
use bridge_common::{
    AudioConfig, ControlMessage, InputEvent, VideoConfig, VideoFrameHeader,
};
use bridge_transport::{
    BridgeConnection, QuicServer, ServiceAdvertiser, TransportConfig,
    DEFAULT_CONTROL_PORT, get_thunderbolt_address, is_thunderbolt_connection,
    discovery::get_local_addresses,
};
use bridge_video::{CaptureConfig, EncoderConfig, ScreenCapturer, VideoEncoder, get_displays, virtual_display::{VirtualDisplay, is_virtual_display_supported}};
use bridge_input::InputInjector;
use bridge_audio::{AudioCaptureConfig, AudioCapturer};
use clap::Parser;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::FmtSubscriber;

#[derive(Parser, Debug)]
#[command(name = "bridge-server")]
#[command(about = "Bridge server daemon for Mac Mini", long_about = None)]
struct Args {
    /// Port for the control channel
    #[arg(short, long, default_value_t = DEFAULT_CONTROL_PORT)]
    port: u16,

    /// Server name for discovery
    #[arg(short, long, default_value = "Bridge Server")]
    name: String,

    /// Video width (0 = native)
    #[arg(long, default_value_t = 0)]
    width: u32,

    /// Video height (0 = native)
    #[arg(long, default_value_t = 0)]
    height: u32,

    /// Target FPS
    #[arg(long, default_value_t = 60)]
    fps: u32,

    /// Video bitrate in Mbps
    #[arg(long, default_value_t = 50)]
    bitrate: u32,

    /// Enable raw/uncompressed video (for Thunderbolt)
    #[arg(long)]
    raw_video: bool,

    /// Disable audio capture
    #[arg(long)]
    no_audio: bool,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let level = if args.verbose { Level::DEBUG } else { Level::INFO };
    let subscriber = FmtSubscriber::builder()
        .with_max_level(level)
        .with_target(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("Bridge Server starting...");
    info!("  Name: {}", args.name);
    info!("  Port: {}", args.port);
    info!("  Resolution: {}x{} @ {}fps", args.width, args.height, args.fps);
    info!("  Bitrate: {} Mbps", args.bitrate);

    // Log local addresses and Thunderbolt detection
    let local_addrs = get_local_addresses();
    info!("  Local addresses: {:?}", local_addrs);
    match get_thunderbolt_address() {
        Some(tb_addr) => info!("  Thunderbolt interface detected: {}", tb_addr.ip()),
        None => info!("  No Thunderbolt interface detected"),
    }

    // Check permissions
    if !bridge_video::is_capture_supported() {
        error!("Screen capture not supported on this system");
        return Ok(());
    }

    // Set up video configuration
    let video_config = VideoConfig {
        width: if args.width > 0 { args.width } else { 3840 },
        height: if args.height > 0 { args.height } else { 2160 },
        fps: args.fps,
        codec: if args.raw_video {
            bridge_common::VideoCodec::Raw
        } else {
            bridge_common::VideoCodec::H265
        },
        bitrate: args.bitrate * 1_000_000,
        pixel_format: bridge_common::PixelFormat::Bgra8,
    };

    let audio_config = AudioConfig::default();

    // Start QUIC server
    let transport_config = TransportConfig {
        control_port: args.port,
        ..Default::default()
    };

    let server = QuicServer::bind(args.port).await?;
    info!("Listening on port {}", args.port);

    // Start service advertisement
    let _advertiser = ServiceAdvertiser::new(&args.name, args.port).await?;
    info!("Advertising service via Bonjour");

    // Set up signal handlers for graceful shutdown
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("Failed to set up SIGTERM handler");
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .expect("Failed to set up SIGINT handler");

    // Accept connections with graceful shutdown support
    loop {
        info!("Waiting for client connection...");

        tokio::select! {
            accept_result = server.accept() => {
                match accept_result {
                    Ok(control_conn) => {
                        let server_name = args.name.clone();
                        let transport_config = transport_config.clone();
                        let video_config = video_config.clone();
                        let audio_config = audio_config.clone();
                        let no_audio = args.no_audio;

                        tokio::spawn(async move {
                            if let Err(e) = handle_client(
                                control_conn,
                                &server_name,
                                transport_config,
                                video_config,
                                audio_config,
                                no_audio,
                            ).await {
                                error!("Client session error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        error!("Failed to accept connection: {}", e);
                    }
                }
            }
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down gracefully...");
                break;
            }
            _ = sigint.recv() => {
                info!("Received SIGINT, shutting down gracefully...");
                break;
            }
        }
    }

    info!("Server shutdown complete");
    Ok(())
}

async fn handle_client(
    control_conn: bridge_transport::QuicConnection,
    server_name: &str,
    config: TransportConfig,
    _video_config: VideoConfig,
    audio_config: AudioConfig,
    no_audio: bool,
) -> Result<()> {
    let remote_addr = control_conn.remote_addr();
    info!("Client connected from {}", remote_addr);

    // Detect if this is a Thunderbolt connection
    let is_thunderbolt = is_thunderbolt_connection(&remote_addr);
    if is_thunderbolt {
        info!("Client connected via Thunderbolt ({})", remote_addr);
    } else {
        info!("Client connected via WiFi/network ({})", remote_addr);
    }

    // Get server's native display resolution BEFORE accepting
    let displays = get_displays();
    let main_display = displays.iter().find(|d| d.is_main).or(displays.first());
    let has_physical_display = main_display.is_some();
    let (native_width, native_height) = main_display
        .map(|d| (d.width, d.height))
        .unwrap_or((0, 0));

    if has_physical_display {
        info!("Server native display: {}x{}", native_width, native_height);
    } else {
        info!("No physical display detected (headless mode)");
    }

    // For Thunderbolt: use larger packets and socket buffers
    let mut config = config;
    if is_thunderbolt {
        config.max_packet_size = bridge_common::MAX_UDP_PACKET_SIZE_THUNDERBOLT;
        config.send_buffer_size = 32 * 1024 * 1024; // 32MB
        config.recv_buffer_size = 32 * 1024 * 1024;
        info!("Thunderbolt: packet_size=65507, socket_buffers=32MB");
    }
    let max_packet_size = config.max_packet_size;

    // Accept with config negotiation
    let (mut conn, hello) = BridgeConnection::accept_with_negotiation(
        control_conn,
        config,
        server_name,
        |hello| {
            let mut cfg = hello.video_config.clone();
            // For Thunderbolt connections, override codec to Raw
            if is_thunderbolt {
                cfg.codec = bridge_common::VideoCodec::Raw;
                info!("Thunderbolt: overriding codec to Raw (uncompressed)");
            }
            cfg
        }
    ).await?;

    info!("Handshake complete with client: {}", hello.client_name);

    // Get the negotiated video config
    let video_config = conn.video_config().cloned().unwrap();

    // Determine capture resolution and display ID
    let mut _virtual_display: Option<VirtualDisplay> = None;
    let (capture_width, capture_height, capture_display_id) = if !has_physical_display {
        // Headless: must create virtual display
        let vd_width = if video_config.width > 0 { video_config.width } else { 3840 };
        let vd_height = if video_config.height > 0 { video_config.height } else { 2160 };

        if is_virtual_display_supported() {
            match VirtualDisplay::new(vd_width, vd_height, video_config.fps) {
                Ok(vd) => {
                    let id = vd.display_id();
                    let w = vd.width();
                    let h = vd.height();
                    info!("Virtual display created: {}x{} (ID={})", w, h, id);
                    _virtual_display = Some(vd);
                    (w, h, Some(id))
                }
                Err(e) => {
                    error!("Failed to create virtual display: {}. No display available!", e);
                    return Err(anyhow::anyhow!("No display available: headless and virtual display failed: {}", e));
                }
            }
        } else {
            error!("Headless mode requires macOS 14+ for virtual display support");
            return Err(anyhow::anyhow!("No display available: headless and virtual display not supported"));
        }
    } else if video_config.width > native_width || video_config.height > native_height {
        // Client wants higher res than native — try virtual display
        if is_virtual_display_supported() {
            match VirtualDisplay::new(video_config.width, video_config.height, video_config.fps) {
                Ok(vd) => {
                    let id = vd.display_id();
                    let w = vd.width();
                    let h = vd.height();
                    info!("Virtual display created for upscale: {}x{} (ID={})", w, h, id);
                    _virtual_display = Some(vd);
                    (w, h, Some(id))
                }
                Err(e) => {
                    warn!("Virtual display failed ({}), falling back to native {}x{}", e, native_width, native_height);
                    (native_width, native_height, None)
                }
            }
        } else {
            info!("Client requested {}x{}, using native {}x{} (virtual display not supported)",
                  video_config.width, video_config.height, native_width, native_height);
            (native_width, native_height, None)
        }
    } else {
        // Native display is fine
        (native_width, native_height, None)
    };

    info!("Capture resolution: {}x{} @ {}fps, codec: {:?}", capture_width, capture_height, video_config.fps, video_config.codec);

    // Create capture config with actual capture resolution
    let capture_config = CaptureConfig {
        width: capture_width,
        height: capture_height,
        fps: video_config.fps,
        show_cursor: true,
        capture_audio: false,
        display_id: capture_display_id,
    };

    // Create video config for encoder
    let capture_video_config = VideoConfig {
        width: capture_width,
        height: capture_height,
        fps: video_config.fps,
        codec: video_config.codec,
        bitrate: video_config.bitrate,
        pixel_format: video_config.pixel_format,
    };
    let mut capturer = ScreenCapturer::new(capture_config)?;

    let is_raw_mode = capture_video_config.codec == bridge_common::VideoCodec::Raw;
    let mut encoder = if !is_raw_mode {
        let encoder_config = EncoderConfig::from(&capture_video_config);
        Some(VideoEncoder::new(encoder_config)?)
    } else {
        info!("Raw mode: skipping encoder (frames sent uncompressed)");
        None
    };

    let mut audio_capturer: Option<AudioCapturer> = if no_audio {
        info!("Audio capture disabled");
        None
    } else {
        let audio_capture_config = AudioCaptureConfig::from(&audio_config);
        match AudioCapturer::new(audio_capture_config) {
            Ok(capturer) => Some(capturer),
            Err(e) => {
                warn!("Audio capture unavailable: {}. Continuing without audio.", e);
                None
            }
        }
    };

    let mut input_injector = InputInjector::new()?;

    let mut is_streaming = false;
    let mut video_frame_number: u64 = 0;

    // Calculate fragment size based on MTU (leave room for headers)
    let fragment_size = max_packet_size - bridge_common::PacketHeader::SIZE - VideoFrameHeader::SIZE - 64;

    // Adaptive bitrate control state (based on packet loss only)
    // Only relevant for encoded (non-raw) modes
    let initial_bitrate = video_config.bitrate;
    let min_bitrate = 15_000_000u32;  // 15 Mbps minimum (below this 4K is unwatchable)
    let max_bitrate = if is_thunderbolt {
        4_000_000_000u32  // 4 Gbps for Thunderbolt (H.265 fallback only)
    } else {
        80_000_000u32   // 80 Mbps for WiFi (802.11ac can handle this)
    };
    let mut current_bitrate = if is_thunderbolt {
        initial_bitrate.max(200_000_000) // Start high for Thunderbolt
    } else {
        initial_bitrate
    };
    let mut last_bitrate_adjustment = tokio::time::Instant::now();
    let bitrate_adjustment_interval = tokio::time::Duration::from_secs(2);

    info!("Server components initialized");

    // Wait for UDP ping packets from client to learn their address
    // Wait for client UDP pings using tokio polling
    info!("Waiting for client UDP pings...");

    // Poll for video ping with timeout
    if let Some(video_ch) = conn.video_channel() {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match tokio::time::timeout(
                std::time::Duration::from_millis(50),
                video_ch.recv()
            ).await {
                Ok(Ok((header, _))) => {
                    debug!("Received video ping from client (type: {:?})", header.packet_type);
                    break;
                }
                Ok(Err(e)) => {
                    warn!("Error receiving video ping: {}", e);
                    break;
                }
                Err(_) => {
                    // Timeout - check if we've exceeded the deadline
                    if tokio::time::Instant::now() > deadline {
                        warn!("Timeout waiting for video ping");
                        break;
                    }
                    // Otherwise keep polling
                }
            }
        }
    }

    // Poll for audio ping
    if let Some(audio_ch) = conn.audio_channel() {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match tokio::time::timeout(
                std::time::Duration::from_millis(50),
                audio_ch.recv()
            ).await {
                Ok(Ok((header, _))) => {
                    debug!("Received audio ping from client (type: {:?})", header.packet_type);
                    break;
                }
                Ok(Err(e)) => {
                    warn!("Error receiving audio ping: {}", e);
                    break;
                }
                Err(_) => {
                    if tokio::time::Instant::now() > deadline {
                        warn!("Timeout waiting for audio ping");
                        break;
                    }
                }
            }
        }
    }

    // Poll for input ping
    if let Some(input_ch) = conn.input_channel() {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match tokio::time::timeout(
                std::time::Duration::from_millis(50),
                input_ch.recv()
            ).await {
                Ok(Ok((header, _))) => {
                    debug!("Received input ping from client (type: {:?})", header.packet_type);
                    break;
                }
                Ok(Err(e)) => {
                    warn!("Error receiving input ping: {}", e);
                    break;
                }
                Err(_) => {
                    if tokio::time::Instant::now() > deadline {
                        warn!("Timeout waiting for input ping");
                        break;
                    }
                }
            }
        }
    }

    info!("UDP channels initialized");

    // Main server loop
    loop {
        // Handle control messages
        tokio::select! {
            control_result = conn.recv_control() => {
                match control_result {
                    Ok(msg) => {
                        match msg {
                            ControlMessage::StartStream => {
                                info!("Client requested stream start");
                                capturer.start().await?;
                                if let Some(ref mut ac) = audio_capturer {
                                    if let Err(e) = ac.start() {
                                        warn!("Audio capture start failed: {}. Continuing without audio.", e);
                                        audio_capturer = None;
                                    }
                                }
                                is_streaming = true;
                            }
                            ControlMessage::StopStream => {
                                info!("Client requested stream stop");
                                capturer.stop()?;
                                if let Some(ref mut ac) = audio_capturer {
                                    let _ = ac.stop();
                                }
                                is_streaming = false;
                            }
                            ControlMessage::ConfigureVideo(cfg) => {
                                info!("Client requested video config: {:?}", cfg);
                                // Would reconfigure if streaming
                            }
                            ControlMessage::ConfigureAudio(cfg) => {
                                info!("Client requested audio config: {:?}", cfg);
                            }
                            ControlMessage::LatencyReport(report) => {
                                debug!("Client stats: decode={}us, loss={:.1}%, jitter={}us",
                                    report.decode_latency_us,
                                    report.packet_loss * 100.0, report.jitter_us);

                                // Skip adaptive bitrate for raw mode (no encoder to adjust)
                                if !is_raw_mode {
                                if let Some(ref mut enc) = encoder {
                                    if last_bitrate_adjustment.elapsed() > bitrate_adjustment_interval {
                                        let new_bitrate = if report.packet_loss > 0.10 {
                                            // Significant packet loss (>10%) - reduce bitrate by 20%
                                            ((current_bitrate as f64) * 0.80) as u32
                                        } else if report.packet_loss > 0.03 {
                                            // Moderate packet loss (>3%) - reduce bitrate by 10%
                                            ((current_bitrate as f64) * 0.90) as u32
                                        } else if report.packet_loss < 0.01 {
                                            // Low packet loss (<1%) - increase bitrate by 15%
                                            ((current_bitrate as f64) * 1.15) as u32
                                        } else {
                                            current_bitrate
                                        };

                                        let new_bitrate = new_bitrate.clamp(min_bitrate, max_bitrate);
                                        if new_bitrate != current_bitrate {
                                            if let Err(e) = enc.set_bitrate(new_bitrate) {
                                                warn!("Failed to adjust bitrate: {}", e);
                                            } else {
                                                info!("Bitrate: {} -> {} Mbps (loss: {:.1}%)",
                                                    current_bitrate / 1_000_000,
                                                    new_bitrate / 1_000_000,
                                                    report.packet_loss * 100.0);
                                                current_bitrate = new_bitrate;
                                            }
                                        }
                                        last_bitrate_adjustment = tokio::time::Instant::now();
                                    }
                                }
                                }
                            }
                            ControlMessage::Disconnect => {
                                info!("Client disconnecting");
                                break;
                            }
                            ControlMessage::RequestKeyframe => {
                                info!("Client requested keyframe");
                                if let Some(ref mut enc) = encoder {
                                    enc.request_keyframe();
                                }
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        error!("Control channel error: {}", e);
                        break;
                    }
                }
            }

            _ = tokio::time::sleep(std::time::Duration::from_millis(1)), if is_streaming => {
                // Process video frames
                let mut frames_this_tick = 0u32;
                while let Some(frame) = capturer.recv_frame() {
                    frames_this_tick += 1;

                    if is_raw_mode {
                        // Raw mode: send captured frame data directly (no encoding)
                        if let Some(video_ch) = conn.video_channel() {
                            let data = &frame.data;
                            let fragment_count = data.len().div_ceil(fragment_size);

                            debug!("Sending raw frame {} ({}x{}, {} bytes, {} fragments)",
                                   video_frame_number, frame.width, frame.height, data.len(), fragment_count);

                            for (i, chunk) in data.chunks(fragment_size).enumerate() {
                                let frame_header = VideoFrameHeader {
                                    frame_number: video_frame_number,
                                    pts_us: frame.pts_us,
                                    is_keyframe: true, // Every raw frame is independently decodable
                                    frame_size: data.len() as u32,
                                    fragment_index: i as u16,
                                    fragment_count: fragment_count as u16,
                                    width: capture_width,
                                    height: capture_height,
                                };

                                let header_bytes = match bincode::serialize(&frame_header) {
                                    Ok(b) => b,
                                    Err(e) => {
                                        warn!("Failed to serialize frame header: {}", e);
                                        continue;
                                    }
                                };

                                let mut packet = Vec::with_capacity(header_bytes.len() + chunk.len());
                                packet.extend_from_slice(&header_bytes);
                                packet.extend_from_slice(chunk);

                                if let Err(e) = video_ch.send(
                                    bridge_common::PacketType::Video,
                                    &packet,
                                ).await {
                                    warn!("Failed to send raw video fragment: {}", e);
                                }
                            }

                            video_frame_number += 1;
                        }
                    } else if let Some(ref mut enc) = encoder {
                        // Encoded mode: encode then send
                        debug!("Encoding frame {} ({}x{}, {} bytes)",
                               frame.frame_number, frame.width, frame.height, frame.data.len());
                        if let Err(e) = enc.encode(&frame) {
                            warn!("Encode error: {}", e);
                        }

                        while let Some(encoded) = enc.recv_frame() {
                            debug!("Encoded frame: {} bytes, keyframe={}", encoded.data.len(), encoded.is_keyframe);
                            if let Some(video_ch) = conn.video_channel() {
                                // Send with proper VideoFrameHeader and fragmentation
                                let data = &encoded.data;
                                let fragment_count = data.len().div_ceil(fragment_size);

                                for (i, chunk) in data.chunks(fragment_size).enumerate() {
                                    let frame_header = VideoFrameHeader {
                                        frame_number: video_frame_number,
                                        pts_us: encoded.pts_us,
                                        is_keyframe: encoded.is_keyframe && i == 0,
                                        frame_size: data.len() as u32,
                                        fragment_index: i as u16,
                                        fragment_count: fragment_count as u16,
                                        width: capture_width,
                                        height: capture_height,
                                    };

                                    let header_bytes = match bincode::serialize(&frame_header) {
                                        Ok(b) => b,
                                        Err(e) => {
                                            warn!("Failed to serialize frame header: {}", e);
                                            continue;
                                        }
                                    };

                                    let mut packet = Vec::with_capacity(header_bytes.len() + chunk.len());
                                    packet.extend_from_slice(&header_bytes);
                                    packet.extend_from_slice(chunk);

                                    if let Err(e) = video_ch.send(
                                        bridge_common::PacketType::Video,
                                        &packet,
                                    ).await {
                                        warn!("Failed to send video fragment: {}", e);
                                    }
                                }

                                video_frame_number += 1;
                            }
                        }
                    }
                }

                if frames_this_tick > 0 {
                    debug!("Processed {} frames this tick", frames_this_tick);
                }

                // Process audio packets
                if let Some(ref mut ac) = audio_capturer {
                    while let Some(packet) = ac.recv_packet() {
                        if let Some(audio_ch) = conn.audio_channel() {
                            let _ = audio_ch.send(
                                bridge_common::PacketType::Audio,
                                &packet.data,
                            ).await;
                        }
                    }
                }
            }
        }

        // Process incoming input events (non-blocking)
        if let Some(input_ch) = conn.input_channel() {
            while let Ok(Ok((header, data))) = tokio::time::timeout(
                std::time::Duration::from_micros(100),
                input_ch.recv()
            ).await {
                if header.packet_type == bridge_common::PacketType::Input {
                    if let Ok(event) = InputEvent::from_bytes(&data) {
                        if let Err(e) = input_injector.inject(&event) {
                            debug!("Input injection error: {}", e);
                        }
                    }
                }
            }
        }
    }

    // Clean up
    capturer.stop()?;
    if let Some(ref mut ac) = audio_capturer {
        let _ = ac.stop();
    }
    conn.disconnect().await?;

    info!("Client session ended");
    Ok(())
}
