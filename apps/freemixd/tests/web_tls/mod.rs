//! A TLS client for gateway tests that trusts exactly the certificate whose
//! SHA-256 fingerprint the daemon printed (`FREEMIXD_WEB_TLS`), as an
//! operator comparing the fingerprint Studio shows would.

#![allow(dead_code)] // each test target uses its own subset

use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime},
};

pub type TlsStream = StreamOwned<ClientConnection, TcpStream>;

/// SHA-256 of `der` as the daemon prints it: `AB:CD:...`.
pub fn fingerprint(der: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, der)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[derive(Debug)]
struct Pinned {
    fingerprint: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "certificate fingerprint mismatch".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn pins() -> &'static Mutex<HashMap<SocketAddr, String>> {
    static PINS: OnceLock<Mutex<HashMap<SocketAddr, String>>> = OnceLock::new();
    PINS.get_or_init(Mutex::default)
}

/// Trusts `fingerprint` for the gateway at `address`.
pub fn pin(address: SocketAddr, fingerprint: &str) {
    pins()
        .lock()
        .unwrap()
        .insert(address, fingerprint.to_owned());
}

/// A client configuration trusting only `fingerprint`.
pub fn client_config(fingerprint: &str) -> Arc<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(
        ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pinned {
                fingerprint: fingerprint.to_owned(),
                provider,
            }))
            .with_no_client_auth(),
    )
}

/// A TLS connection to the pinned gateway at `address` (the handshake runs
/// on first use).
pub fn connect(address: SocketAddr, timeout: Duration) -> std::io::Result<TlsStream> {
    let fingerprint = pins()
        .lock()
        .unwrap()
        .get(&address)
        .cloned()
        .expect("the gateway's fingerprint is pinned");
    connect_with(address, timeout, &client_config(&fingerprint))
}

pub fn connect_with(
    address: SocketAddr,
    timeout: Duration,
    config: &Arc<ClientConfig>,
) -> std::io::Result<TlsStream> {
    let socket = TcpStream::connect_timeout(&address, timeout)?;
    socket.set_read_timeout(Some(timeout))?;
    socket.set_write_timeout(Some(timeout))?;
    let connection = ClientConnection::new(
        Arc::clone(config),
        ServerName::IpAddress(address.ip().into()),
    )
    .map_err(std::io::Error::other)?;
    Ok(StreamOwned::new(connection, socket))
}

/// Sends one raw request over TLS and returns the status line, headers
/// (lowercase names), and body.
pub fn https(
    address: SocketAddr,
    request: &str,
    timeout: Duration,
) -> (String, Vec<(String, String)>, Vec<u8>) {
    let mut stream = connect(address, timeout).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("complete response head");
    let head = std::str::from_utf8(&response[..split]).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap().to_owned();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.trim().to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    (status, headers, response[split + 4..].to_vec())
}
