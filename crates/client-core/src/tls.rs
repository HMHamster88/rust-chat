//! WebSocket transport with TOFU certificate pinning.

use futures_util::{SinkExt, StreamExt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use std::fmt;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{WebSocketStream, client_async};

pub type Socket = WebSocketStream<Transport>;

#[derive(Debug)]
pub struct TlsError {
    pub message: String,
    /// The certificate the server actually presented, when the handshake got that far.
    pub captured: Option<String>,
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TlsError {}

impl TlsError {
    fn new(message: impl Into<String>, captured: Option<String>) -> Self {
        Self {
            message: message.into(),
            captured,
        }
    }
}

#[derive(Debug)]
pub enum Transport {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for Transport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Transport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

pub fn fingerprint(der: &[u8]) -> String {
    rust_chat_crypto::to_fingerprint(&rust_chat_crypto::sha256(der))
}

#[derive(Debug)]
struct PinnedVerifier {
    expected: Option<String>,
    captured: Arc<Mutex<Option<String>>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let actual = fingerprint(end_entity.as_ref());
        *self
            .captured
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(actual.clone());
        match &self.expected {
            Some(expected) if expected != &actual => Err(Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            )),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Connects and upgrades to WebSocket. `expected` is the pinned fingerprint; when it is `None`
/// the certificate is accepted and captured so the caller can ask the user before sending data.
pub async fn connect(
    host: &str,
    port: u16,
    tls_enabled: bool,
    expected: Option<String>,
) -> Result<(Socket, Option<String>), TlsError> {
    let address = format!("{host}:{port}");
    if !tls_enabled {
        let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&address))
            .await
            .map_err(|_| TlsError::new("connecting timed out", None))?
            .map_err(|error| {
                TlsError::new(format!("cannot connect to {address}: {error}"), None)
            })?;
        let (socket, _) = client_async(format!("ws://{address}/"), Transport::Plain(stream))
            .await
            .map_err(|error| TlsError::new(format!("websocket handshake failed: {error}"), None))?;
        return Ok((socket, None));
    }

    let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&address))
        .await
        .map_err(|_| TlsError::new("connecting timed out", None))?
        .map_err(|error| TlsError::new(format!("cannot connect to {address}: {error}"), None))?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let captured = Arc::new(Mutex::new(None));
    let verifier = Arc::new(PinnedVerifier {
        expected: expected.clone(),
        captured: captured.clone(),
        provider: provider.clone(),
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| TlsError::new(format!("TLS configuration: {error}"), None))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));

    let server_name = if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        ServerName::IpAddress(ip.into())
    } else {
        ServerName::try_from(host.to_owned())
            .map_err(|_| TlsError::new(format!("invalid server name `{host}`"), None))?
    };

    let tls = tokio::time::timeout(Duration::from_secs(10), connector.connect(server_name, tcp))
        .await
        .map_err(|_| TlsError::new("TLS handshake timed out", None))?
        .map_err(|error| {
            let actual = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let message = match (&expected, &actual) {
                (Some(expected), Some(actual)) if expected != actual => format!(
                    "server certificate changed: pinned {expected}, server presented {actual}"
                ),
                _ => format!("TLS handshake failed: {error}"),
            };
            TlsError::new(message, actual)
        })?;

    let actual = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (socket, _) = client_async(format!("wss://{address}/"), Transport::Tls(Box::new(tls)))
        .await
        .map_err(|error| {
            TlsError::new(
                format!("websocket handshake failed: {error}"),
                actual.clone(),
            )
        })?;
    Ok((socket, actual))
}

pub async fn send_envelope(
    socket: &mut Socket,
    envelope: &rust_chat_protocol::Envelope,
) -> io::Result<()> {
    let bytes = envelope
        .encode()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let text = String::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

pub async fn next_envelope(
    socket: &mut Socket,
) -> Option<Result<rust_chat_protocol::Envelope, String>> {
    loop {
        match socket.next().await? {
            Ok(Message::Text(text)) => {
                return Some(
                    rust_chat_protocol::Envelope::decode(text.as_bytes())
                        .map_err(|error| error.to_string()),
                );
            }
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
            Ok(Message::Close(_)) => return None,
            Ok(other) => return Some(Err(format!("unexpected frame: {other:?}"))),
            Err(error) => return Some(Err(error.to_string())),
        }
    }
}

pub async fn close(socket: &mut Socket) {
    let _ = socket.close(None).await;
}
