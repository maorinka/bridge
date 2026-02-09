//! QUIC-based control channel for reliable messaging

use bridge_common::{BridgeError, BridgeResult, ControlMessage};
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use sha2::{Sha256, Digest};
use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use parking_lot::RwLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, error, info, warn};

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

/// Configure QUIC client with Trust-On-First-Use (TOFU) certificate verification
fn configure_client() -> ClientConfig {
    let tofu_verifier = TofuCertificateVerifier::new();

    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(tofu_verifier))
        .with_no_client_auth();

    ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .expect("Failed to create QUIC client config from rustls config"),
    ))
}

/// Get the path to the server certificate/key storage directory
fn get_server_cert_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let bridge_dir = PathBuf::from(home).join(".bridge");
    let _ = fs::create_dir_all(&bridge_dir);
    bridge_dir
}

/// Configure QUIC server with persistent self-signed certificate.
/// The cert/key are saved to ~/.bridge/ so they survive server restarts,
/// avoiding TOFU fingerprint mismatches on the client side.
fn configure_server() -> BridgeResult<(ServerConfig, CertificateDer<'static>)> {
    let cert_dir = get_server_cert_dir();
    let cert_path = cert_dir.join("server.cert");
    let key_path = cert_dir.join("server.key");

    let (cert_der, key_der) = if cert_path.exists() && key_path.exists() {
        // Load existing cert and key
        let cert_bytes = fs::read(&cert_path)
            .map_err(|e| BridgeError::Transport(format!("Failed to read cert: {}", e)))?;
        let key_bytes = fs::read(&key_path)
            .map_err(|e| BridgeError::Transport(format!("Failed to read key: {}", e)))?;
        info!("Loaded server certificate from {:?}", cert_path);
        (
            CertificateDer::from(cert_bytes),
            PrivatePkcs8KeyDer::from(key_bytes),
        )
    } else {
        // Generate new cert and key, then persist
        let cert = rcgen::generate_simple_self_signed(vec!["bridge".into()])
            .map_err(|e| BridgeError::Transport(format!("Failed to generate cert: {}", e)))?;

        let cert_der = CertificateDer::from(cert.cert);
        let key_bytes = cert.key_pair.serialize_der();
        let key_der = PrivatePkcs8KeyDer::from(key_bytes.clone());

        fs::write(&cert_path, cert_der.as_ref())
            .map_err(|e| BridgeError::Transport(format!("Failed to save cert: {}", e)))?;
        fs::write(&key_path, &key_bytes)
            .map_err(|e| BridgeError::Transport(format!("Failed to save key: {}", e)))?;

        info!("Generated and saved new server certificate to {:?}", cert_path);
        (cert_der, key_der)
    };

    let mut server_config = ServerConfig::with_single_cert(
        vec![cert_der.clone()],
        key_der.into(),
    )
    .map_err(|e| BridgeError::Transport(format!("Failed to configure server: {}", e)))?;

    // Configure transport for low latency
    if let Some(transport_config) = Arc::get_mut(&mut server_config.transport) {
        if let Ok(timeout) = std::time::Duration::from_secs(10).try_into() {
            transport_config.max_idle_timeout(Some(timeout));
        }
    }

    Ok((server_config, cert_der))
}

/// Trust-On-First-Use (TOFU) certificate verifier
///
/// On first connection to a server, stores the certificate fingerprint.
/// On subsequent connections, verifies the fingerprint matches.
/// This prevents MITM attacks while allowing self-signed certificates.
#[derive(Debug)]
struct TofuCertificateVerifier {
    /// Map of server name to certificate fingerprint (SHA-256 hex)
    known_hosts: Arc<RwLock<HashMap<String, String>>>,
    /// Path to persist known hosts
    storage_path: PathBuf,
}

impl TofuCertificateVerifier {
    fn new() -> Self {
        let storage_path = Self::get_storage_path();
        let known_hosts = Self::load_known_hosts(&storage_path);

        Self {
            known_hosts: Arc::new(RwLock::new(known_hosts)),
            storage_path,
        }
    }

    fn get_storage_path() -> PathBuf {
        // Use ~/.bridge/known_hosts
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let bridge_dir = PathBuf::from(home).join(".bridge");
        let _ = fs::create_dir_all(&bridge_dir);
        bridge_dir.join("known_hosts")
    }

    fn load_known_hosts(path: &PathBuf) -> HashMap<String, String> {
        if let Ok(content) = fs::read_to_string(path) {
            content
                .lines()
                .filter_map(|line| {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        Some((parts[0].to_string(), parts[1].to_string()))
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            HashMap::new()
        }
    }

    fn save_known_hosts(&self) {
        let hosts = self.known_hosts.read();
        let content: String = hosts
            .iter()
            .map(|(name, fingerprint)| format!("{} {}", name, fingerprint))
            .collect::<Vec<_>>()
            .join("\n");

        if let Err(e) = fs::write(&self.storage_path, content) {
            warn!("Failed to save known hosts: {}", e);
        }
    }

    fn compute_fingerprint(cert: &CertificateDer<'_>) -> String {
        let mut hasher = Sha256::new();
        hasher.update(cert.as_ref());
        let result = hasher.finalize();
        hex::encode(result)
    }
}

impl rustls::client::danger::ServerCertVerifier for TofuCertificateVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let server_name_str = server_name.to_str().to_string();
        let fingerprint = Self::compute_fingerprint(end_entity);

        // Check if we know this host
        {
            let hosts = self.known_hosts.read();
            if let Some(stored_fingerprint) = hosts.get(&server_name_str) {
                if stored_fingerprint == &fingerprint {
                    debug!("Certificate fingerprint verified for {}", server_name_str);
                    return Ok(rustls::client::danger::ServerCertVerified::assertion());
                } else {
                    // Certificate changed - this could be a MITM attack!
                    error!(
                        "CERTIFICATE CHANGED for {}! Stored: {}, Received: {}",
                        server_name_str, stored_fingerprint, fingerprint
                    );
                    error!("This could indicate a man-in-the-middle attack.");
                    error!("If the server certificate was legitimately changed, delete ~/.bridge/known_hosts");
                    return Err(rustls::Error::General(
                        "Server certificate fingerprint mismatch - possible MITM attack".into(),
                    ));
                }
            }
        }

        // First connection - trust and store the certificate
        info!(
            "First connection to {} - storing certificate fingerprint: {}",
            server_name_str, fingerprint
        );

        {
            let mut hosts = self.known_hosts.write();
            hosts.insert(server_name_str, fingerprint);
        }

        self.save_known_hosts();

        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // Use rustls's built-in verification
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // Use rustls's built-in verification
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
