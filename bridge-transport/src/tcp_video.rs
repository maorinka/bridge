//! TCP video channel for Thunderbolt+LZ4 streaming
//!
//! Uses dedicated OS threads (not tokio) with crossbeam channels for
//! zero-overhead integration with the async main loops.
//! Wire format: [total_len: u32 LE][payload]
//! where payload = [VideoFrameHeader (bincode)][LZ4 data]

use std::io::{Read, Write, BufReader, BufWriter};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use crossbeam_channel::{Sender, Receiver};
use tracing::{info, warn, error};

const BUF_SIZE: usize = 8 * 1024 * 1024; // 8 MB

/// A complete frame to send over TCP (header bytes + LZ4 payload already concatenated)
pub struct TcpVideoFrame {
    pub data: Vec<u8>,
}

/// Start a TCP video server that accepts one connection and sends frames.
/// Returns a Sender to push frames into; the background thread writes them.
pub fn start_tcp_video_server(
    port: u16,
    allowed_ip: IpAddr,
) -> std::io::Result<Sender<TcpVideoFrame>> {
    let listener = TcpListener::bind(SocketAddr::new("0.0.0.0".parse().unwrap(), port))?;
    info!("TCP video server listening on port {}", port);

    // Channel capacity 4: allows frames to queue while TCP writes.
    // If writer can't keep up, try_send drops frames (backpressure).
    let (tx, rx): (Sender<TcpVideoFrame>, Receiver<TcpVideoFrame>) = crossbeam_channel::bounded(4);

    std::thread::Builder::new()
        .name("tcp-video-server".into())
        .spawn(move || {
            // Accept one connection from allowed IP
            let stream = loop {
                match listener.accept() {
                    Ok((stream, addr)) => {
                        if addr.ip() == allowed_ip {
                            info!("TCP video: accepted connection from {}", addr);
                            break stream;
                        } else {
                            warn!("TCP video: rejected connection from {} (expected {})", addr, allowed_ip);
                        }
                    }
                    Err(e) => {
                        error!("TCP video: accept error: {}", e);
                        return;
                    }
                }
            };

            stream.set_nodelay(true).ok();
            let mut writer = BufWriter::with_capacity(BUF_SIZE, stream);

            for frame in rx {
                let len = frame.data.len() as u32;
                if writer.write_all(&len.to_le_bytes()).is_err() {
                    break;
                }
                if writer.write_all(&frame.data).is_err() {
                    break;
                }
                // Flush after each frame to avoid data stuck in BufWriter.
                // Without this, the last frame of a capture burst stays buffered
                // while the writer blocks on rx.recv(), causing the client to stall.
                // At 6MB/frame on Thunderbolt, flush takes <1ms.
                if writer.flush().is_err() {
                    break;
                }
            }

            info!("TCP video server thread exiting");
        })?;

    Ok(tx)
}

/// Start a TCP video client that connects to the server and receives frames.
/// Returns a Receiver to pull complete frames from.
pub fn start_tcp_video_client(
    server_addr: SocketAddr,
) -> std::io::Result<Receiver<TcpVideoFrame>> {
    let stream = TcpStream::connect(server_addr)?;
    stream.set_nodelay(true)?;
    info!("TCP video: connected to {}", server_addr);

    let (tx, rx): (Sender<TcpVideoFrame>, Receiver<TcpVideoFrame>) = crossbeam_channel::bounded(2);

    std::thread::Builder::new()
        .name("tcp-video-client".into())
        .spawn(move || {
            let mut reader = BufReader::with_capacity(BUF_SIZE, stream);
            let mut len_buf = [0u8; 4];

            loop {
                if reader.read_exact(&mut len_buf).is_err() {
                    break;
                }
                let len = u32::from_le_bytes(len_buf) as usize;
                if len > 64 * 1024 * 1024 {
                    error!("TCP video: frame too large ({} bytes)", len);
                    break;
                }
                let mut data = vec![0u8; len];
                if reader.read_exact(&mut data).is_err() {
                    break;
                }
                if tx.send(TcpVideoFrame { data }).is_err() {
                    break; // receiver dropped
                }
            }

            info!("TCP video client thread exiting");
        })?;

    Ok(rx)
}
