//! UDP channel for low-latency video/audio streaming

use bridge_common::{BridgeError, BridgeResult, PacketHeader, PacketType};
use bytes::{Bytes, BytesMut};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tracing::{debug, trace, warn};

/// UDP channel for low-latency data transfer
pub struct UdpChannel {
    socket: Arc<UdpSocket>,
    remote_addr: Option<SocketAddr>,
    max_packet_size: usize,
    send_sequence: u64,
    recv_buffer: BytesMut,
}

impl UdpChannel {
    /// Bind to a local port (server mode)
    pub async fn bind(port: u16, max_packet_size: usize) -> BridgeResult<Self> {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let socket = UdpSocket::bind(addr).await.map_err(|e| {
            BridgeError::Transport(format!("Failed to bind UDP socket on port {}: {}", port, e))
        })?;

        // Set socket options for low latency
        Self::configure_socket(&socket)?;

        debug!("UDP channel bound to port {}", port);

        Ok(Self {
            socket: Arc::new(socket),
            remote_addr: None,
            max_packet_size,
            send_sequence: 0,
            recv_buffer: BytesMut::with_capacity(max_packet_size),
        })
    }

    /// Connect to a remote address (client mode)
    pub async fn connect(remote_addr: SocketAddr, max_packet_size: usize) -> BridgeResult<Self> {
        // Bind to any available port
        let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| {
            BridgeError::Transport(format!("Failed to bind UDP socket: {}", e))
        })?;

        socket.connect(remote_addr).await.map_err(|e| {
            BridgeError::Transport(format!("Failed to connect UDP socket: {}", e))
        })?;

        Self::configure_socket(&socket)?;

        debug!("UDP channel connected to {}", remote_addr);

        Ok(Self {
            socket: Arc::new(socket),
            remote_addr: Some(remote_addr),
            max_packet_size,
            send_sequence: 0,
            recv_buffer: BytesMut::with_capacity(max_packet_size),
        })
    }

    /// Configure socket for low latency
    fn configure_socket(socket: &UdpSocket) -> BridgeResult<()> {
        // These would require platform-specific code
        // For now, we rely on default settings
        // TODO: Set SO_SNDBUF, SO_RCVBUF, IP_TOS for QoS
        Ok(())
    }

    /// Get the local address
    pub fn local_addr(&self) -> BridgeResult<SocketAddr> {
        self.socket.local_addr().map_err(|e| {
            BridgeError::Transport(format!("Failed to get local address: {}", e))
        })
    }

    /// Set the remote address (for server mode after receiving first packet)
    pub fn set_remote_addr(&mut self, addr: SocketAddr) {
        self.remote_addr = Some(addr);
    }

    /// Send raw data with a packet header
    pub async fn send(&mut self, packet_type: PacketType, data: &[u8]) -> BridgeResult<usize> {
        let header = PacketHeader::new(packet_type, self.send_sequence, data.len() as u32);
        self.send_sequence += 1;

        let header_bytes = header.to_bytes()?;

        // Combine header and data
        let mut packet = Vec::with_capacity(header_bytes.len() + data.len());
        packet.extend_from_slice(&header_bytes);
        packet.extend_from_slice(data);

        let sent = if let Some(addr) = self.remote_addr {
            self.socket.send_to(&packet, addr).await
        } else {
            self.socket.send(&packet).await
        }.map_err(|e| BridgeError::Transport(format!("Send failed: {}", e)))?;

        trace!("Sent {} bytes (seq: {})", sent, self.send_sequence - 1);

        Ok(sent)
    }

    /// Send raw data without header (for fragmented frames)
    pub async fn send_raw(&self, data: &[u8]) -> BridgeResult<usize> {
        let sent = if let Some(addr) = self.remote_addr {
            self.socket.send_to(data, addr).await
        } else {
            self.socket.send(data).await
        }.map_err(|e| BridgeError::Transport(format!("Send failed: {}", e)))?;

        Ok(sent)
    }

    /// Receive data with header validation
    pub async fn recv(&mut self) -> BridgeResult<(PacketHeader, Bytes)> {
        let mut buf = vec![0u8; self.max_packet_size];

        let (len, from) = self.socket.recv_from(&mut buf).await.map_err(|e| {
            BridgeError::Transport(format!("Receive failed: {}", e))
        })?;

        // Update remote address if not set
        if self.remote_addr.is_none() {
            self.remote_addr = Some(from);
        }

        if len < PacketHeader::SIZE {
            return Err(BridgeError::Protocol("Packet too small".into()));
        }

        let header = PacketHeader::from_bytes(&buf[..PacketHeader::SIZE])?;

        if !header.is_valid() {
            return Err(BridgeError::Protocol("Invalid packet header".into()));
        }

        let data = Bytes::copy_from_slice(&buf[PacketHeader::SIZE..len]);

        trace!("Received {} bytes (seq: {}, type: {:?})", len, header.sequence, header.packet_type);

        Ok((header, data))
    }

    /// Receive raw data without header parsing
    pub async fn recv_raw(&mut self) -> BridgeResult<(Bytes, SocketAddr)> {
        let mut buf = vec![0u8; self.max_packet_size];

        let (len, from) = self.socket.recv_from(&mut buf).await.map_err(|e| {
            BridgeError::Transport(format!("Receive failed: {}", e))
        })?;

        Ok((Bytes::copy_from_slice(&buf[..len]), from))
    }

    /// Try to receive without blocking (returns None if no data available)
    pub fn try_recv(&mut self) -> BridgeResult<Option<(PacketHeader, Bytes)>> {
        // This would require non-blocking socket operations
        // For now, use the async version
        Ok(None)
    }

    /// Clone the socket for use in multiple tasks
    pub fn clone_socket(&self) -> Arc<UdpSocket> {
        self.socket.clone()
    }
}

/// Video frame sender that handles fragmentation
pub struct FrameSender {
    channel: UdpChannel,
    frame_number: u64,
    fragment_size: usize,
}

impl FrameSender {
    pub fn new(channel: UdpChannel) -> Self {
        // Leave room for headers
        let fragment_size = channel.max_packet_size - PacketHeader::SIZE - 64;

        Self {
            channel,
            frame_number: 0,
            fragment_size,
        }
    }

    /// Send a video frame, fragmenting if necessary
    pub async fn send_frame(
        &mut self,
        data: &[u8],
        pts_us: u64,
        is_keyframe: bool,
    ) -> BridgeResult<()> {
        use bridge_common::VideoFrameHeader;

        let fragment_count = (data.len() + self.fragment_size - 1) / self.fragment_size;

        for (i, chunk) in data.chunks(self.fragment_size).enumerate() {
            let frame_header = VideoFrameHeader {
                frame_number: self.frame_number,
                pts_us,
                is_keyframe: is_keyframe && i == 0,
                frame_size: data.len() as u32,
                fragment_index: i as u16,
                fragment_count: fragment_count as u16,
                width: 0, // Set by caller
                height: 0,
            };

            let header_bytes = bincode::serialize(&frame_header)
                .map_err(|e| BridgeError::Serialization(e.to_string()))?;

            let mut packet = Vec::with_capacity(header_bytes.len() + chunk.len());
            packet.extend_from_slice(&header_bytes);
            packet.extend_from_slice(chunk);

            self.channel.send(PacketType::Video, &packet).await?;
        }

        self.frame_number += 1;
        Ok(())
    }
}

/// Video frame receiver that handles reassembly
pub struct FrameReceiver {
    channel: UdpChannel,
    pending_frames: std::collections::HashMap<u64, PendingFrame>,
}

struct PendingFrame {
    fragments: Vec<Option<Bytes>>,
    received_count: usize,
    total_size: usize,
    pts_us: u64,
    is_keyframe: bool,
}

impl FrameReceiver {
    pub fn new(channel: UdpChannel) -> Self {
        Self {
            channel,
            pending_frames: std::collections::HashMap::new(),
        }
    }

    /// Receive a complete video frame
    pub async fn recv_frame(&mut self) -> BridgeResult<(Bytes, u64, bool)> {
        use bridge_common::VideoFrameHeader;

        loop {
            let (header, data) = self.channel.recv().await?;

            if header.packet_type != PacketType::Video {
                continue;
            }

            // Parse frame header
            if data.len() < VideoFrameHeader::SIZE {
                warn!("Video packet too small");
                continue;
            }

            let frame_header: VideoFrameHeader = bincode::deserialize(&data[..VideoFrameHeader::SIZE])
                .map_err(|e| BridgeError::Serialization(e.to_string()))?;

            let fragment_data = data.slice(VideoFrameHeader::SIZE..);

            // Get or create pending frame
            let pending = self.pending_frames
                .entry(frame_header.frame_number)
                .or_insert_with(|| PendingFrame {
                    fragments: vec![None; frame_header.fragment_count as usize],
                    received_count: 0,
                    total_size: frame_header.frame_size as usize,
                    pts_us: frame_header.pts_us,
                    is_keyframe: frame_header.is_keyframe,
                });

            // Store fragment
            let idx = frame_header.fragment_index as usize;
            if idx < pending.fragments.len() && pending.fragments[idx].is_none() {
                pending.fragments[idx] = Some(fragment_data);
                pending.received_count += 1;
            }

            // Check if frame is complete
            if pending.received_count == pending.fragments.len() {
                let frame_num = frame_header.frame_number;
                let pending = self.pending_frames.remove(&frame_num).unwrap();

                // Reassemble frame
                let mut frame_data = Vec::with_capacity(pending.total_size);
                for fragment in pending.fragments {
                    if let Some(data) = fragment {
                        frame_data.extend_from_slice(&data);
                    }
                }

                // Clean up old pending frames
                self.pending_frames.retain(|&num, _| num > frame_num.saturating_sub(10));

                return Ok((Bytes::from(frame_data), pending.pts_us, pending.is_keyframe));
            }
        }
    }
}
