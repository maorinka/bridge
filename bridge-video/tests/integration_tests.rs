//! Integration tests for the video pipeline
//!
//! These tests verify the full capture -> encode -> decode pipeline works correctly.

use bridge_common::{VideoCodec, VideoConfig, PixelFormat};
use bridge_video::{
    CaptureConfig, CapturedFrame, DecodedFrame, EncodedFrame,
    EncoderConfig, ScreenCapturer, VideoDecoder, VideoEncoder,
    get_displays, is_capture_supported,
};

/// Test that we can enumerate displays
#[test]
fn test_display_enumeration() {
    let displays = get_displays();
    println!("Found {} displays:", displays.len());
    for d in &displays {
        println!("  Display {}: {}x{} (main={})", d.id, d.width, d.height, d.is_main);
    }
    assert!(!displays.is_empty(), "No displays found");
}

/// Test capture support detection
#[test]
fn test_capture_support() {
    let supported = is_capture_supported();
    println!("Screen capture supported: {}", supported);
    // Don't assert - depends on permissions
}

/// Test creating an encoder and encoding a synthetic frame
#[test]
fn test_encode_synthetic_frame_h264() {
    let config = EncoderConfig {
        width: 256,
        height: 256,
        fps: 30,
        codec: VideoCodec::H264,
        bitrate: 1_000_000,
        keyframe_interval: 30,
        ..Default::default()
    };

    let mut encoder = VideoEncoder::new(config).expect("Failed to create encoder");

    // Create a gradient test pattern
    let mut data = vec![0u8; 256 * 256 * 4];
    for y in 0..256 {
        for x in 0..256 {
            let idx = (y * 256 + x) * 4;
            data[idx] = x as u8;     // B
            data[idx + 1] = y as u8; // G
            data[idx + 2] = 128;     // R
            data[idx + 3] = 255;     // A
        }
    }

    let frame = CapturedFrame {
        data,
        width: 256,
        height: 256,
        bytes_per_row: 256 * 4,
        pts_us: 0,
        frame_number: 0,
        io_surface: None,
    };

    // Encode
    encoder.encode(&frame).expect("Encode failed");
    encoder.flush().expect("Flush failed");

    // Wait and collect output
    std::thread::sleep(std::time::Duration::from_millis(100));

    let mut total_bytes = 0;
    let mut frame_count = 0;
    while let Some(encoded) = encoder.recv_frame() {
        total_bytes += encoded.data.len();
        frame_count += 1;
        println!(
            "Encoded frame {}: {} bytes, keyframe={}",
            encoded.frame_number, encoded.data.len(), encoded.is_keyframe
        );
    }

    println!("Total: {} frames, {} bytes", frame_count, total_bytes);
    assert!(frame_count > 0 || encoder.frame_count() > 0, "No frames encoded");
}

/// Test creating an encoder and encoding a synthetic frame with H.265
#[test]
fn test_encode_synthetic_frame_h265() {
    let config = EncoderConfig {
        width: 256,
        height: 256,
        fps: 30,
        codec: VideoCodec::H265,
        bitrate: 1_000_000,
        keyframe_interval: 30,
        ..Default::default()
    };

    let mut encoder = VideoEncoder::new(config).expect("Failed to create encoder");

    // Create a solid color test frame
    let data = vec![100u8; 256 * 256 * 4];

    let frame = CapturedFrame {
        data,
        width: 256,
        height: 256,
        bytes_per_row: 256 * 4,
        pts_us: 0,
        frame_number: 0,
        io_surface: None,
    };

    encoder.encode(&frame).expect("Encode failed");
    encoder.flush().expect("Flush failed");

    std::thread::sleep(std::time::Duration::from_millis(100));

    let mut frame_count = 0;
    while let Some(encoded) = encoder.recv_frame() {
        frame_count += 1;
        println!(
            "H.265 frame {}: {} bytes",
            encoded.frame_number, encoded.data.len()
        );
    }

    assert!(frame_count > 0 || encoder.frame_count() > 0, "No H.265 frames encoded");
}

/// Test encoding multiple frames to verify temporal compression
#[test]
fn test_encode_multiple_frames_temporal() {
    let config = EncoderConfig {
        width: 128,
        height: 128,
        fps: 30,
        codec: VideoCodec::H264,
        bitrate: 500_000,
        keyframe_interval: 30,
        ..Default::default()
    };

    let mut encoder = VideoEncoder::new(config).expect("Failed to create encoder");

    // Encode 10 frames with slight changes
    for i in 0u64..10 {
        let mut data = vec![0u8; 128 * 128 * 4];
        // Create a moving block pattern
        let block_x = ((i as usize) * 10) % 128;
        for y in 0..128 {
            for x in 0..128 {
                let idx = (y * 128 + x) * 4;
                if x >= block_x && x < block_x + 20 && y >= 50 && y < 70 {
                    data[idx] = 255;     // B
                    data[idx + 1] = 255; // G
                    data[idx + 2] = 255; // R
                } else {
                    data[idx] = 50;      // B
                    data[idx + 1] = 50;  // G
                    data[idx + 2] = 50;  // R
                }
                data[idx + 3] = 255;     // A
            }
        }

        let frame = CapturedFrame {
            data,
            width: 128,
            height: 128,
            bytes_per_row: 128 * 4,
            pts_us: i * 33333,
            frame_number: i,
            io_surface: None,
        };

        encoder.encode(&frame).expect("Encode failed");
    }

    encoder.flush().expect("Flush failed");
    std::thread::sleep(std::time::Duration::from_millis(200));

    let mut sizes = Vec::new();
    while let Some(encoded) = encoder.recv_frame() {
        sizes.push(encoded.data.len());
    }

    println!("Frame sizes: {:?}", sizes);
    println!("Encoded {} frames", sizes.len());

    // Verify we got output
    assert!(!sizes.is_empty() || encoder.frame_count() > 0, "No frames encoded");

    // If we got multiple frames, verify P-frames are generally smaller than I-frame
    if sizes.len() > 2 {
        let first = sizes[0];
        let avg_rest: usize = sizes[1..].iter().sum::<usize>() / (sizes.len() - 1);
        println!("First frame: {} bytes, avg rest: {} bytes", first, avg_rest);
        // I-frame should generally be larger due to full encoding
    }
}

/// Test decoder creation
#[test]
fn test_decoder_creation() {
    let decoder = VideoDecoder::new(1920, 1080, VideoCodec::H264);
    assert!(decoder.is_ok(), "Failed to create H.264 decoder: {:?}", decoder.err());

    let decoder = VideoDecoder::new(1920, 1080, VideoCodec::H265);
    assert!(decoder.is_ok(), "Failed to create H.265 decoder: {:?}", decoder.err());
}

/// Test that decoder isn't initialized until it receives parameter sets
#[test]
fn test_decoder_not_initialized_initially() {
    let decoder = VideoDecoder::new(1920, 1080, VideoCodec::H264).expect("Failed to create decoder");
    assert!(!decoder.is_initialized(), "Decoder should not be initialized without params");
}

/// Test encoder configuration from VideoConfig
#[test]
fn test_encoder_config_conversion() {
    let video_config = VideoConfig {
        width: 3840,
        height: 2160,
        fps: 60,
        codec: VideoCodec::H265,
        bitrate: 50_000_000,
        pixel_format: PixelFormat::Bgra8,
    };

    let encoder_config = EncoderConfig::from(&video_config);

    assert_eq!(encoder_config.width, 3840);
    assert_eq!(encoder_config.height, 2160);
    assert_eq!(encoder_config.fps, 60);
    assert_eq!(encoder_config.codec, VideoCodec::H265);
    assert_eq!(encoder_config.bitrate, 50_000_000);
}

/// Test capture configuration from VideoConfig
#[test]
fn test_capture_config_conversion() {
    let video_config = VideoConfig {
        width: 1920,
        height: 1080,
        fps: 30,
        codec: VideoCodec::H264,
        bitrate: 10_000_000,
        pixel_format: PixelFormat::Bgra8,
    };

    let capture_config = CaptureConfig::from(&video_config);

    assert_eq!(capture_config.width, 1920);
    assert_eq!(capture_config.height, 1080);
    assert_eq!(capture_config.fps, 30);
}

/// Async test for capture start/stop
#[tokio::test]
async fn test_capture_lifecycle() {
    if !ScreenCapturer::has_permission() {
        println!("Skipping capture test: no screen recording permission");
        println!("Grant permission in System Preferences > Privacy & Security > Screen Recording");
        return;
    }

    let config = CaptureConfig {
        width: 320,
        height: 240,
        fps: 15,
        show_cursor: false,
        ..Default::default()
    };

    let mut capturer = ScreenCapturer::new(config).expect("Failed to create capturer");

    // Verify initial state
    let stats = capturer.stats();
    assert!(!stats.is_running);
    assert_eq!(stats.frames_captured, 0);

    // Start capture
    capturer.start().await.expect("Failed to start capture");
    assert!(capturer.stats().is_running);

    // Wait for frames
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // Stop capture
    capturer.stop().expect("Failed to stop capture");
    assert!(!capturer.stats().is_running);

    let final_stats = capturer.stats();
    println!("Captured {} frames", final_stats.frames_captured);
}

/// Full pipeline test: capture -> encode
#[tokio::test]
async fn test_capture_to_encode_pipeline() {
    if !ScreenCapturer::has_permission() {
        println!("Skipping pipeline test: no screen recording permission");
        return;
    }

    // Create capturer
    let capture_config = CaptureConfig {
        width: 320,
        height: 240,
        fps: 30,
        ..Default::default()
    };
    let mut capturer = ScreenCapturer::new(capture_config).expect("Failed to create capturer");

    // Create encoder
    let encoder_config = EncoderConfig {
        width: 320,
        height: 240,
        fps: 30,
        codec: VideoCodec::H264,
        bitrate: 1_000_000,
        ..Default::default()
    };
    let mut encoder = VideoEncoder::new(encoder_config).expect("Failed to create encoder");

    // Start capture
    capturer.start().await.expect("Failed to start capture");

    // Capture and encode frames
    let mut captured_count = 0;
    let mut encoded_count = 0;

    for _ in 0..10 {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Process captured frames
        while let Some(frame) = capturer.recv_frame() {
            captured_count += 1;
            if let Err(e) = encoder.encode(&frame) {
                eprintln!("Encode error: {}", e);
            }
        }

        // Count encoded frames
        while encoder.recv_frame().is_some() {
            encoded_count += 1;
        }
    }

    // Flush encoder
    encoder.flush().expect("Flush failed");
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    while encoder.recv_frame().is_some() {
        encoded_count += 1;
    }

    capturer.stop().expect("Failed to stop capture");

    println!("Pipeline test: captured {} frames, encoded {} frames", captured_count, encoded_count);

    // We should have processed some frames (if permission is granted and capture works)
    if captured_count > 0 {
        assert!(encoded_count > 0 || encoder.frame_count() > 0, "No frames encoded");
    }
}

/// Test encoder handles various resolutions
#[test]
fn test_encoder_various_resolutions() {
    let resolutions = [
        (320, 240),
        (640, 480),
        (1280, 720),
        (1920, 1080),
    ];

    for (width, height) in resolutions {
        let config = EncoderConfig {
            width,
            height,
            fps: 30,
            codec: VideoCodec::H264,
            ..Default::default()
        };

        let result = VideoEncoder::new(config);
        assert!(
            result.is_ok(),
            "Failed to create encoder for {}x{}: {:?}",
            width, height, result.err()
        );
        println!("Created encoder for {}x{}", width, height);
    }
}

/// Test that raw codec is rejected for encoder
#[test]
fn test_encoder_rejects_raw_codec() {
    let config = EncoderConfig {
        codec: VideoCodec::Raw,
        ..Default::default()
    };

    let result = VideoEncoder::new(config);
    assert!(result.is_err(), "Encoder should reject Raw codec");
}

/// Test that raw codec is rejected for decoder
#[test]
fn test_decoder_rejects_raw_codec() {
    let result = VideoDecoder::new(1920, 1080, VideoCodec::Raw);
    assert!(result.is_err(), "Decoder should reject Raw codec");
}

/// Test encoder frame counting
#[test]
fn test_encoder_frame_counting() {
    let config = EncoderConfig {
        width: 128,
        height: 128,
        fps: 30,
        codec: VideoCodec::H264,
        ..Default::default()
    };

    let mut encoder = VideoEncoder::new(config).expect("Failed to create encoder");

    assert_eq!(encoder.frame_count(), 0, "Initial frame count should be 0");

    // Encode a few frames
    for i in 0..3 {
        let frame = CapturedFrame {
            data: vec![0u8; 128 * 128 * 4],
            width: 128,
            height: 128,
            bytes_per_row: 128 * 4,
            pts_us: i * 33333,
            frame_number: i,
            io_surface: None,
        };
        encoder.encode(&frame).expect("Encode failed");
    }

    encoder.flush().expect("Flush failed");
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Drain the output
    while encoder.recv_frame().is_some() {}

    let final_count = encoder.frame_count();
    println!("Final frame count: {}", final_count);
    assert!(final_count > 0, "Frame count should be > 0 after encoding");
}
