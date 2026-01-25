//! Bridge Client - iMac application
//!
//! The client runs on the iMac and:
//! - Receives video from the server and displays it full-screen
//! - Captures keyboard/mouse input and sends to the server
//! - Receives audio from the server and plays it

use anyhow::Result;
use bridge_common::{
    ControlMessage, LatencyReport, elapsed_us,
};
use bridge_transport::{
    BridgeConnection, ServiceBrowser, TransportConfig, DEFAULT_CONTROL_PORT,
};
use bridge_video::{MetalDisplay, VideoDecoder, DecodedFrame};
use bridge_input::{CaptureConfig, InputCapturer};
use bridge_audio::{PlaybackConfig, AudioPlayer, AudioPacket};
use clap::Parser;
use std::net::SocketAddr;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::FmtSubscriber;

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

    // Connect to server
    let transport_config = TransportConfig::default();
    let mut conn = BridgeConnection::new(server_addr, transport_config);

    let welcome = conn.connect(&args.name).await?;
    info!("Connected to server: {}", welcome.server_name);
    info!("  Video: {}x{} @ {}fps",
        welcome.video_config.width,
        welcome.video_config.height,
        welcome.video_config.fps
    );

    let video_config = welcome.video_config;
    let audio_config = welcome.audio_config;

    // Initialize components
    let mut display = MetalDisplay::new(video_config.width, video_config.height)?;

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

    // Request stream start
    conn.send_control(ControlMessage::StartStream).await?;
    info!("Stream started");

    let mut latency_samples: Vec<u64> = Vec::with_capacity(60);
    let mut last_latency_report = tokio::time::Instant::now();
    let latency_report_interval = tokio::time::Duration::from_secs(1);

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
                        // Decode frame
                        let decoded = if let Some(ref mut dec) = decoder {
                            let encoded = bridge_video::EncodedFrame {
                                data: data.clone(),
                                pts_us: header.timestamp_us,
                                dts_us: header.timestamp_us,
                                is_keyframe: false,
                                frame_number: header.sequence,
                            };

                            dec.decode(&encoded)?;
                            dec.recv_frame()
                        } else {
                            Some(DecodedFrame {
                                data: data.to_vec(),
                                width: video_config.width,
                                height: video_config.height,
                                bytes_per_row: video_config.width * 4,
                                pts_us: header.timestamp_us,
                                io_surface: None,
                            })
                        };

                        if let Some(frame) = decoded {
                            display.render(&frame)?;

                            let latency = elapsed_us(header.timestamp_us);
                            latency_samples.push(latency);
                            if latency_samples.len() > 60 {
                                latency_samples.remove(0);
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

            let report = LatencyReport {
                rtt_us: avg_latency * 2,
                decode_latency_us: avg_latency / 2,
                display_latency_us: avg_latency / 2,
                packet_loss: 0.0,
                jitter_us: 0,
            };

            debug!("Average latency: {}us ({:.1}ms)", avg_latency, avg_latency as f64 / 1000.0);

            conn.send_control(ControlMessage::LatencyReport(report)).await?;
            last_latency_report = tokio::time::Instant::now();
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
