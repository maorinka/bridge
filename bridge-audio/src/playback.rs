//! Audio playback using CoreAudio via cpal
//!
//! Provides low-latency audio playback with configurable buffer sizes.

use bridge_common::{BridgeResult, BridgeError, AudioConfig};
use std::collections::VecDeque;
use std::sync::Arc;
use parking_lot::Mutex;
use crossbeam_channel::{bounded, Sender};
use tracing::{debug, error, info};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::AudioPacket;

/// Audio playback configuration
#[derive(Debug, Clone)]
pub struct PlaybackConfig {
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of channels
    pub channels: u16,
    /// Buffer size in samples (lower = less latency but more CPU)
    pub buffer_size: u32,
    /// Number of buffers to queue (jitter buffer)
    pub buffer_count: u32,
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            buffer_size: 960, // 20ms at 48kHz
            buffer_count: 4,   // 80ms total jitter buffer
        }
    }
}

impl From<&AudioConfig> for PlaybackConfig {
    fn from(config: &AudioConfig) -> Self {
        Self {
            sample_rate: config.sample_rate,
            channels: config.channels,
            buffer_size: config.buffer_size,
            ..Default::default()
        }
    }
}

/// Audio player using cpal
pub struct AudioPlayer {
    config: PlaybackConfig,
    sample_tx: Sender<Vec<f32>>,
    stream: Option<cpal::Stream>,
    is_running: Arc<Mutex<bool>>,
    stats: Arc<Mutex<PlaybackStats>>,
}

/// Playback statistics
#[derive(Debug, Clone, Default)]
pub struct PlaybackStats {
    pub packets_played: u64,
    pub underruns: u64,
    pub buffer_level: usize,
}

unsafe impl Send for AudioPlayer {}

impl AudioPlayer {
    /// Create a new audio player
    pub fn new(config: PlaybackConfig) -> BridgeResult<Self> {
        // Buffer for incoming samples
        let (sample_tx, _sample_rx) = bounded::<Vec<f32>>(config.buffer_count as usize * 2);

        Ok(Self {
            config,
            sample_tx,
            stream: None,
            is_running: Arc::new(Mutex::new(false)),
            stats: Arc::new(Mutex::new(PlaybackStats::default())),
        })
    }

    /// List available audio output devices
    pub fn list_devices() -> BridgeResult<Vec<String>> {
        let host = cpal::default_host();
        let devices: Vec<String> = host
            .output_devices()
            .map_err(|e| BridgeError::Audio(format!("Failed to enumerate devices: {}", e)))?
            .filter_map(|d| d.name().ok())
            .collect();
        Ok(devices)
    }

    /// Start audio playback
    pub fn start(&mut self) -> BridgeResult<()> {
        if *self.is_running.lock() {
            return Ok(());
        }

        info!("Starting audio playback: {}Hz, {} channels", self.config.sample_rate, self.config.channels);

        let host = cpal::default_host();

        let device = host.default_output_device()
            .ok_or_else(|| BridgeError::Audio("No output device available".into()))?;

        debug!("Using audio device: {:?}", device.name());

        // Configure stream for low latency
        let stream_config = cpal::StreamConfig {
            channels: self.config.channels,
            sample_rate: cpal::SampleRate(self.config.sample_rate),
            buffer_size: cpal::BufferSize::Fixed(self.config.buffer_size),
        };

        // Create sample buffer shared with callback
        let (sample_tx, sample_rx) = bounded::<Vec<f32>>(self.config.buffer_count as usize * 2);
        let buffer: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let buffer_clone = buffer.clone();
        let stats = self.stats.clone();

        // Spawn task to receive samples and add to buffer
        let buffer_for_receiver = buffer.clone();
        std::thread::spawn(move || {
            while let Ok(samples) = sample_rx.recv() {
                let mut buf = buffer_for_receiver.lock();
                buf.extend(samples);
            }
        });

        // Create output stream
        let stream = device
            .build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut buf = buffer_clone.lock();
                    let mut stats_guard = stats.lock();

                    for sample in data.iter_mut() {
                        if let Some(s) = buf.pop_front() {
                            *sample = s;
                        } else {
                            // Underrun - output silence
                            *sample = 0.0;
                            stats_guard.underruns += 1;
                        }
                    }

                    stats_guard.buffer_level = buf.len();
                },
                move |err| {
                    error!("Audio playback error: {}", err);
                },
                None,
            )
            .map_err(|e| BridgeError::Audio(format!("Failed to build stream: {}", e)))?;

        stream.play().map_err(|e| BridgeError::Audio(format!("Failed to start stream: {}", e)))?;

        self.sample_tx = sample_tx;
        self.stream = Some(stream);
        *self.is_running.lock() = true;

        info!("Audio playback started");
        Ok(())
    }

    /// Stop audio playback
    pub fn stop(&mut self) -> BridgeResult<()> {
        if !*self.is_running.lock() {
            return Ok(());
        }

        info!("Stopping audio playback");

        if let Some(stream) = self.stream.take() {
            drop(stream);
        }

        *self.is_running.lock() = false;
        Ok(())
    }

    /// Play an audio packet
    pub fn play(&self, packet: &AudioPacket) -> BridgeResult<()> {
        if !*self.is_running.lock() {
            return Err(BridgeError::Audio("Playback not started".into()));
        }

        // Convert bytes back to f32 samples
        let samples: Vec<f32> = packet.data
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();

        self.sample_tx.send(samples)
            .map_err(|_| BridgeError::Audio("Failed to queue samples".into()))?;

        let mut stats = self.stats.lock();
        stats.packets_played += 1;

        Ok(())
    }

    /// Play raw f32 samples directly
    pub fn play_samples(&self, samples: Vec<f32>) -> BridgeResult<()> {
        if !*self.is_running.lock() {
            return Err(BridgeError::Audio("Playback not started".into()));
        }

        self.sample_tx.send(samples)
            .map_err(|_| BridgeError::Audio("Failed to queue samples".into()))?;

        Ok(())
    }

    /// Get playback statistics
    pub fn stats(&self) -> PlaybackStats {
        self.stats.lock().clone()
    }

    /// Check if playback is active
    pub fn is_running(&self) -> bool {
        *self.is_running.lock()
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Audio synchronization helper for A/V sync
pub struct AudioSync {
    /// Target latency in microseconds
    target_latency_us: u64,
    /// Current audio PTS
    audio_pts_us: u64,
    /// Current video PTS
    video_pts_us: u64,
    /// Sync adjustment in microseconds (positive = audio ahead)
    adjustment_us: i64,
}

impl AudioSync {
    pub fn new(target_latency_us: u64) -> Self {
        Self {
            target_latency_us,
            audio_pts_us: 0,
            video_pts_us: 0,
            adjustment_us: 0,
        }
    }

    /// Update with new audio timestamp
    pub fn update_audio(&mut self, pts_us: u64) {
        self.audio_pts_us = pts_us;
        self.calculate_adjustment();
    }

    /// Update with new video timestamp
    pub fn update_video(&mut self, pts_us: u64) {
        self.video_pts_us = pts_us;
        self.calculate_adjustment();
    }

    fn calculate_adjustment(&mut self) {
        // Positive drift means audio is ahead of video
        let drift = self.audio_pts_us as i64 - self.video_pts_us as i64;

        // Simple low-pass filter for smooth adjustment
        self.adjustment_us = self.adjustment_us + (drift - self.adjustment_us) / 10;
    }

    /// Get the current sync adjustment
    pub fn adjustment_us(&self) -> i64 {
        self.adjustment_us
    }

    /// Check if sync is within acceptable bounds
    pub fn is_synced(&self) -> bool {
        self.adjustment_us.abs() < self.target_latency_us as i64
    }
}
