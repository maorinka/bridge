//! QUIC-based control channel for reliable messaging

use bridge_common::{BridgeError, BridgeResult, ControlMessage};
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, error, info};

/// QUIC connection for the control channel
pub struct QuicConnection {
    connection: Connection,
    send_stream: SendStream,
    recv_stream: RecvStream,
}

impl QuicConnection {
    /// Connect to a QUIC server (client mode)
    pub async fn connect(addr: SocketAddr) -> BridgeResult<Self> {
        let client_config = configure_client();

        let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())
            .map_err(|e| BridgeError::Transport(format!("Failed to create endpoint: {}", e)))?;

        endpoint.set_default_client_config(client_config);

        info!("Connecting to QUIC server at {}", addr);

        let connection = endpoint
            .connect(addr, "bridge")
            .map_err(|e| BridgeError::Transport(format!("Connection failed: {}", e)))?
            .await
            .map_err(|e| BridgeError::Transport(format!("Connection failed: {}", e)))?;

        debug!("QUIC connection established");

        // Open a bidirectional stream for control messages
        let (send_stream, recv_stream) = connection
            .open_bi()
            .await
            .map_err(|e| BridgeError::Transport(format!("Failed to open stream: {}", e)))?;

        Ok(Self {
            connection,
            send_stream,
            recv_stream,
        })
    }

    /// Accept a connection from a client (called by QuicServer)
    pub(crate) async fn accept(connection: Connection) -> BridgeResult<Self> {
        // Accept bidirectional stream from client
        let (send_stream, recv_stream) = connection
            .accept_bi()
            .await
            .map_err(|e| BridgeError::Transport(format!("Failed to accept stream: {}", e)))?;

        Ok(Self {
            connection,
            send_stream,
            recv_stream,
        })
    }

    /// Get the remote address
    pub fn remote_addr(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    /// Send a control message
    pub async fn send_control(&mut self, msg: ControlMessage) -> BridgeResult<()> {
        let data = msg.to_bytes()?;

        // Write length prefix (4 bytes, big-endian)
        let len = data.len() as u32;
        self.send_stream
            .write_all(&len.to_be_bytes())
            .await
            .map_err(|e| BridgeError::Transport(format!("Send failed: {}", e)))?;

        // Write message data
        self.send_stream
            .write_all(&data)
            .await
            .map_err(|e| BridgeError::Transport(format!("Send failed: {}", e)))?;

        debug!("Sent control message: {:?}", std::mem::discriminant(&msg));

        Ok(())
    }

    /// Receive a control message
    pub async fn recv_control(&mut self) -> BridgeResult<ControlMessage> {
        // Read length prefix
        let mut len_buf = [0u8; 4];
        self.recv_stream
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| BridgeError::Transport(format!("Receive failed: {}", e)))?;

        let len = u32::from_be_bytes(len_buf) as usize;

        if len > 1024 * 1024 {
            // 1MB max message size
            return Err(BridgeError::Protocol("Message too large".into()));
        }

        // Read message data
        let mut data = vec![0u8; len];
        self.recv_stream
            .read_exact(&mut data)
            .await
            .map_err(|e| BridgeError::Transport(format!("Receive failed: {}", e)))?;

        let msg = ControlMessage::from_bytes(&data)?;

        debug!("Received control message: {:?}", std::mem::discriminant(&msg));

        Ok(msg)
    }

    /// Close the connection
    pub fn close(&self) {
        self.connection.close(0u32.into(), b"closed");
    }
}

/// QUIC server for accepting control connections
pub struct QuicServer {
    endpoint: Endpoint,
}

impl QuicServer {
    /// Create a new QUIC server
    pub async fn bind(port: u16) -> BridgeResult<Self> {
        let (server_config, _cert) = configure_server()?;

        let addr = SocketAddr::from(([0, 0, 0, 0], port));

        let endpoint = Endpoint::server(server_config, addr)
            .map_err(|e| BridgeError::Transport(format!("Failed to bind QUIC server: {}", e)))?;

        info!("QUIC server listening on port {}", port);

        Ok(Self { endpoint })
    }

    /// Accept a new connection
    pub async fn accept(&self) -> BridgeResult<QuicConnection> {
        let incoming = self.endpoint
            .accept()
            .await
            .ok_or_else(|| BridgeError::Transport("Server closed".into()))?;

        let connection = incoming
            .await
            .map_err(|e| BridgeError::Transport(format!("Connection failed: {}", e)))?;

        info!("Accepted connection from {}", connection.remote_address());

        QuicConnection::accept(connection).await
    }

    /// Get the local address
    pub fn local_addr(&self) -> BridgeResult<SocketAddr> {
        self.endpoint
            .local_addr()
            .map_err(|e| BridgeError::Transport(format!("Failed to get local addr: {}", e)))
    }
}

/// Configure QUIC client with self-signed certificate trust
fn configure_client() -> ClientConfig {
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
    ))
}

/// Configure QUIC server with self-signed certificate
fn configure_server() -> BridgeResult<(ServerConfig, CertificateDer<'static>)> {
    let cert = rcgen::generate_simple_self_signed(vec!["bridge".into()])
        .map_err(|e| BridgeError::Transport(format!("Failed to generate cert: {}", e)))?;

    let cert_der = CertificateDer::from(cert.cert);
    let key_der = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());

    let mut server_config = ServerConfig::with_single_cert(
        vec![cert_der.clone()],
        key_der.into(),
    )
    .map_err(|e| BridgeError::Transport(format!("Failed to configure server: {}", e)))?;

    // Configure transport for low latency
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.max_idle_timeout(Some(std::time::Duration::from_secs(30).try_into().unwrap()));

    Ok((server_config, cert_der))
}

/// Skip server certificate verification (for self-signed certs)
#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}
