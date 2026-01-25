//! Audio capture using CoreAudio
//!
//! For system audio capture (capturing what the Mac is playing),
//! a DriverKit AudioServerPlugin virtual device would be ideal.
//! This module provides the Rust side of the capture interface.

use bridge_common::{BridgeResult, BridgeError, AudioConfig, AudioPacketHeader, now_us};
use bytes::Bytes;
use std::sync::Arc;
use parking_lot::Mutex;
use crossbeam_channel::{bounded, Receiver, Sender};
use tracing::{debug, error, info, trace, warn};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Captured audio packet
#[derive(Debug, Clone)]
pub struct AudioPacket {
    /// Audio sample data (PCM)
    pub data: Bytes,
    /// Presentation timestamp in microseconds
    pub pts_us: u64,
    /// Number of samples
    pub sample_count: u32,
    /// Sequence number
    pub sequence: u64,
}

/// Audio capturer configuration
#[derive(Debug, Clone)]
pub struct AudioCaptureConfig {
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of channels
    pub channels: u16,
    /// Buffer size in samples
    pub buffer_size: u32,
    /// Capture from input device (microphone) instead of output loopback
    pub capture_input: bool,
}

impl Default for AudioCaptureConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            buffer_size: 480, // 10ms at 48kHz
            capture_input: false,
        }
    }
}

impl From<&AudioConfig> for AudioCaptureConfig {
    fn from(config: &AudioConfig) -> Self {
        Self {
            sample_rate: config.sample_rate,
            channels: config.channels,
            buffer_size: config.buffer_size,
            ..Default::default()
        }
    }
}

/// Audio capturer using cpal (cross-platform audio library)
pub struct AudioCapturer {
    config: AudioCaptureConfig,
    packet_tx: Sender<AudioPacket>,
    packet_rx: Receiver<AudioPacket>,
    stream: Option<cpal::Stream>,
    sequence: Arc<Mutex<u64>>,
    is_running: Arc<Mutex<bool>>,
}

unsafe impl Send for AudioCapturer {}

impl AudioCapturer {
    /// Create a new audio capturer
    pub fn new(config: AudioCaptureConfig) -> BridgeResult<Self> {
        let (packet_tx, packet_rx) = bounded(32);

        Ok(Self {
            config,
            packet_tx,
            packet_rx,
            stream: None,
            sequence: Arc::new(Mutex::new(0)),
            is_running: Arc::new(Mutex::new(false)),
        })
    }

    /// List available audio input devices
    pub fn list_devices() -> BridgeResult<Vec<String>> {
        let host = cpal::default_host();
        let devices: Vec<String> = host
            .input_devices()
            .map_err(|e| BridgeError::Audio(format!("Failed to enumerate devices: {}", e)))?
            .filter_map(|d| d.name().ok())
            .collect();
        Ok(devices)
    }

    /// Start capturing audio
    pub fn start(&mut self) -> BridgeResult<()> {
        if *self.is_running.lock() {
            return Ok(());
        }

        info!("Starting audio capture: {}Hz, {} channels", self.config.sample_rate, self.config.channels);

        let host = cpal::default_host();

        // Get input device
        let device = if self.config.capture_input {
            host.default_input_device()
                .ok_or_else(|| BridgeError::Audio("No input device available".into()))?
        } else {
            // For output loopback, we'd need a virtual audio device
            // Fall back to input device for now
            warn!("Output loopback capture requires DriverKit virtual device. Using input device.");
            host.default_input_device()
                .ok_or_else(|| BridgeError::Audio("No input device available".into()))?
        };

        debug!("Using audio device: {:?}", device.name());

        // Configure stream
        let stream_config = cpal::StreamConfig {
            channels: self.config.channels,
            sample_rate: cpal::SampleRate(self.config.sample_rate),
            buffer_size: cpal::BufferSize::Fixed(self.config.buffer_size),
        };

        let packet_tx = self.packet_tx.clone();
        let sequence = self.sequence.clone();
        let sample_rate = self.config.sample_rate;

        // Create input stream for f32 samples
        let stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let timestamp = now_us();
                    let mut seq = sequence.lock();

                    // Convert f32 to bytes
                    let bytes: Vec<u8> = data
                        .iter()
                        .flat_map(|&sample| sample.to_le_bytes())
                        .collect();

                    let packet = AudioPacket {
                        data: Bytes::from(bytes),
                        pts_us: timestamp,
                        sample_count: data.len() as u32 / 2, // Stereo
                        sequence: *seq,
                    };

                    *seq += 1;
                    let _ = packet_tx.try_send(packet);
                },
                move |err| {
                    error!("Audio capture error: {}", err);
                },
                None, // No timeout
            )
            .map_err(|e| BridgeError::Audio(format!("Failed to build stream: {}", e)))?;

        stream.play().map_err(|e| BridgeError::Audio(format!("Failed to start stream: {}", e)))?;

        self.stream = Some(stream);
        *self.is_running.lock() = true;

        info!("Audio capture started");
        Ok(())
    }

    /// Stop capturing audio
    pub fn stop(&mut self) -> BridgeResult<()> {
        if !*self.is_running.lock() {
            return Ok(());
        }

        info!("Stopping audio capture");

        if let Some(stream) = self.stream.take() {
            drop(stream);
        }

        *self.is_running.lock() = false;
        Ok(())
    }

    /// Get the next audio packet (non-blocking)
    pub fn recv_packet(&self) -> Option<AudioPacket> {
        self.packet_rx.try_recv().ok()
    }

    /// Get the next audio packet (blocking)
    pub fn recv_packet_blocking(&self) -> BridgeResult<AudioPacket> {
        self.packet_rx.recv().map_err(|_| BridgeError::Audio("Packet channel closed".into()))
    }

    /// Get a receiver for audio packets
    pub fn packet_receiver(&self) -> Receiver<AudioPacket> {
        self.packet_rx.clone()
    }

    /// Check if capturing is active
    pub fn is_running(&self) -> bool {
        *self.is_running.lock()
    }
}

impl Drop for AudioCapturer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Placeholder for DriverKit virtual audio device capture
/// Would capture system audio output by creating a virtual audio device
/// that applications think is a physical audio output
pub struct VirtualAudioCapture {
    // Would contain connection to DriverKit audio driver
}

impl VirtualAudioCapture {
    /// Create connection to DriverKit virtual audio device
    pub fn new() -> BridgeResult<Self> {
        Err(BridgeError::Audio(
            "DriverKit virtual audio capture not implemented. \
            Requires AudioServerPlugin DriverKit extension.".into()
        ))
    }
}
