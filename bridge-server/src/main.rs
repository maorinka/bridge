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
    DEFAULT_CONTROL_PORT,
};
use bridge_video::{CaptureConfig, EncoderConfig, ScreenCapturer, VideoEncoder};
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

                        tokio::spawn(async move {
                            if let Err(e) = handle_client(
                                control_conn,
                                &server_name,
                                transport_config,
                                video_config,
                                audio_config,
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
    video_config: VideoConfig,
    audio_config: AudioConfig,
) -> Result<()> {
    info!("Client connected from {}", control_conn.remote_addr());

    // Accept the connection and perform handshake
    let max_packet_size = config.max_packet_size;
    let (mut conn, hello) = BridgeConnection::accept(control_conn, config, server_name).await?;

    info!("Handshake complete with client: {}", hello.client_name);

    // Initialize components
    let capture_config = CaptureConfig::from(&video_config);
    let mut capturer = ScreenCapturer::new(capture_config)?;

    let mut encoder = if video_config.codec != bridge_common::VideoCodec::Raw {
        let encoder_config = EncoderConfig::from(&video_config);
        Some(VideoEncoder::new(encoder_config)?)
    } else {
        None
    };

    let audio_capture_config = AudioCaptureConfig::from(&audio_config);
    let mut audio_capturer = AudioCapturer::new(audio_capture_config)?;

    let mut input_injector = InputInjector::new()?;

    let mut is_streaming = false;
    let mut video_frame_number: u64 = 0;

    // Calculate fragment size based on MTU (leave room for headers)
    let fragment_size = max_packet_size - bridge_common::PacketHeader::SIZE - VideoFrameHeader::SIZE - 64;

    // Adaptive bitrate control state
    let initial_bitrate = video_config.bitrate;
    let min_bitrate = 5_000_000u32;   // 5 Mbps minimum
    let max_bitrate = 100_000_000u32; // 100 Mbps maximum
    let target_latency_us = 16_000u64; // 16ms target for 60fps
    let max_acceptable_latency_us = 33_000u64; // 33ms max before quality reduction
    let mut current_bitrate = initial_bitrate;
    let mut last_bitrate_adjustment = tokio::time::Instant::now();
    let bitrate_adjustment_interval = tokio::time::Duration::from_secs(2);

    info!("Server components initialized");

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
                                audio_capturer.start()?;
                                is_streaming = true;
                            }
                            ControlMessage::StopStream => {
                                info!("Client requested stream stop");
                                capturer.stop()?;
                                audio_capturer.stop()?;
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
                                debug!("Latency report: RTT={}us, decode={}us, loss={:.1}%, jitter={}us",
                                    report.rtt_us, report.decode_latency_us,
                                    report.packet_loss * 100.0, report.jitter_us);

                                // Adaptive bitrate adjustment
                                if let Some(ref mut enc) = encoder {
                                    if last_bitrate_adjustment.elapsed() > bitrate_adjustment_interval {
                                        let total_latency = report.rtt_us / 2 + report.decode_latency_us + report.display_latency_us;
                                        let new_bitrate = if total_latency > max_acceptable_latency_us || report.packet_loss > 0.05 {
                                            // Reduce bitrate by 20% if latency too high or packet loss > 5%
                                            ((current_bitrate as f64) * 0.8) as u32
                                        } else if total_latency < target_latency_us && report.packet_loss < 0.01 {
                                            // Increase bitrate by 10% if latency is good and packet loss < 1%
                                            ((current_bitrate as f64) * 1.1) as u32
                                        } else {
                                            current_bitrate
                                        };

                                        let new_bitrate = new_bitrate.clamp(min_bitrate, max_bitrate);
                                        if new_bitrate != current_bitrate {
                                            if let Err(e) = enc.set_bitrate(new_bitrate) {
                                                warn!("Failed to adjust bitrate: {}", e);
                                            } else {
                                                info!("Bitrate adjusted: {} -> {} Mbps (latency: {}us, loss: {:.1}%)",
                                                    current_bitrate / 1_000_000,
                                                    new_bitrate / 1_000_000,
                                                    total_latency,
                                                    report.packet_loss * 100.0);
                                                current_bitrate = new_bitrate;
                                            }
                                        }
                                        last_bitrate_adjustment = tokio::time::Instant::now();
                                    }
                                }
                            }
                            ControlMessage::Disconnect => {
                                info!("Client disconnecting");
                                break;
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
                while let Some(frame) = capturer.recv_frame() {
                    if let Some(ref mut enc) = encoder {
                        if let Err(e) = enc.encode(&frame) {
                            warn!("Encode error: {}", e);
                        }

                        while let Some(encoded) = enc.recv_frame() {
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
                                        width: video_config.width,
                                        height: video_config.height,
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

                                    let _ = video_ch.send(
                                        bridge_common::PacketType::Video,
                                        &packet,
                                    ).await;
                                }

                                video_frame_number += 1;
                            }
                        }
                    }
                }

                // Process audio packets
                while let Some(packet) = audio_capturer.recv_packet() {
                    if let Some(audio_ch) = conn.audio_channel() {
                        let _ = audio_ch.send(
                            bridge_common::PacketType::Audio,
                            &packet.data,
                        ).await;
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
    audio_capturer.stop()?;
    conn.disconnect().await?;

    info!("Client session ended");
    Ok(())
}
