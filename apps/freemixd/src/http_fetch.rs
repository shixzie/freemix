//! Bounded HTTP title-data fetching (GX-005).
//!
//! One `GET` with connect/read timeouts, a response-size cap, at most three
//! redirects, and no request body. `http://` and `https://` are supported;
//! HTTPS uses rustls with the Mozilla root store from `webpki-roots`. The
//! caller parses the bytes; this module only owns the network bounds.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Largest response body accepted.
pub(crate) const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Most redirects followed.
const MAX_REDIRECTS: usize = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HEADER_BYTES: usize = 16 * 1024;

/// Fetches `url`, following bounded `http://` redirects.
///
/// # Errors
///
/// Returns a message naming the URL parse, connection, timeout, status,
/// redirect, or size bound failure. `https://` is refused explicitly.
pub(crate) fn fetch(url: &str) -> Result<Vec<u8>, String> {
    fetch_with(url, None, false)
}

/// [`fetch`] with an optional TLS client configuration and a test-only
/// permission for private addresses. Production callers use [`fetch`].
fn fetch_with(
    url: &str,
    tls: Option<&Arc<rustls::ClientConfig>>,
    allow_private: bool,
) -> Result<Vec<u8>, String> {
    let mut current = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        let parsed = ParsedUrl::parse(&current)?;
        let (response, location) = get(&parsed, tls, allow_private)?;
        if let Some(location) = location {
            current = parsed.resolve(&location);
            continue;
        }
        return Ok(response);
    }
    Err(format!("more than {MAX_REDIRECTS} redirects from {url}"))
}

struct ParsedUrl {
    tls: bool,
    host: String,
    port: u16,
    path: String,
}

impl ParsedUrl {
    fn parse(url: &str) -> Result<Self, String> {
        let (tls, rest) = if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else {
            return Err("URL source must start with http:// or https://".to_owned());
        };
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, "/".to_owned()),
        };
        if authority.is_empty() || authority.contains('@') {
            return Err(format!("invalid URL authority in {url}"));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() => (
                host.to_owned(),
                port.parse::<u16>()
                    .map_err(|_| format!("invalid port in {url}"))?,
            ),
            _ => (authority.to_owned(), if tls { 443 } else { 80 }),
        };
        let path = path.split('#').next().unwrap_or("/").to_owned();
        Ok(Self {
            tls,
            host,
            port,
            path,
        })
    }

    fn resolve(&self, location: &str) -> String {
        if location.starts_with("http://") || location.starts_with("https://") {
            return location.to_owned();
        }
        if location.starts_with('/') {
            return format!(
                "{}://{}:{}{}",
                if self.tls { "https" } else { "http" },
                self.host,
                self.port,
                location
            );
        }
        let base = self.path.rsplit_once('/').map_or("/", |(base, _)| base);
        format!(
            "{}://{}:{}{}/{}",
            if self.tls { "https" } else { "http" },
            self.host,
            self.port,
            base.trim_end_matches('/'),
            location
        )
    }
}

fn get(
    url: &ParsedUrl,
    tls: Option<&Arc<rustls::ClientConfig>>,
    allow_private: bool,
) -> Result<(Vec<u8>, Option<String>), String> {
    let mut addresses = (url.host.as_str(), url.port)
        .to_socket_addrs()
        .map_err(|error| format!("{}: {error}", url.host))?;
    let mut last_error = None;
    let mut stream = None;
    for address in addresses.by_ref() {
        if !allow_private && !public_address(address.ip()) {
            last_error = Some(std::io::Error::other(format!(
                "outbound address {} is not public",
                address.ip()
            )));
            continue;
        }
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(candidate) => {
                stream = Some(candidate);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let stream = stream.ok_or_else(|| format!("{}: {last_error:?}", url.host))?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(|error| error.to_string())?;
    let mut stream: Box<dyn ReadWrite> = if url.tls {
        Box::new(tls_stream(stream, &url.host, tls)?)
    } else {
        Box::new(stream)
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: application/json, text/csv, application/vnd.openxmlformats-officedocument.spreadsheetml.sheet, application/vnd.oasis.opendocument.spreadsheet, */*\r\n\r\n",
        url.path, url.host
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| error.to_string())?;
    let mut reader = (&mut stream).take((MAX_RESPONSE_BYTES + MAX_HEADER_BYTES) as u64);
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_RESPONSE_BYTES + MAX_HEADER_BYTES {
            return Err("HTTP response exceeds the size bound".to_owned());
        }
        if let Some(header_end) = find_headers_end(&buffer)
            && buffer.len() - header_end > MAX_RESPONSE_BYTES
        {
            return Err("HTTP response body exceeds the size bound".to_owned());
        }
    }
    let header_end = find_headers_end(&buffer)
        .ok_or_else(|| "HTTP response has no complete header".to_owned())?;
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = headers.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let code = status
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("invalid HTTP status line {status:?}"))?;
    let body = &buffer[header_end..];
    if (300..400).contains(&code) {
        let location = lines.find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("location")
                .then(|| value.trim().to_owned())
        });
        return location
            .map(|location| (Vec::new(), Some(location)))
            .ok_or_else(|| format!("HTTP {code} without Location"));
    }
    if code != 200 {
        return Err(format!("HTTP {code}"));
    }
    if body.len() > MAX_RESPONSE_BYTES {
        return Err("HTTP response body exceeds the size bound".to_owned());
    }
    let chunked = lines.any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
        })
    });
    if chunked {
        decode_chunked(body)
    } else {
        Ok((body.to_vec(), None))
    }
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

fn tls_config() -> Result<&'static Arc<rustls::ClientConfig>, String> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config);
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let _ = CONFIG.set(Arc::clone(&config));
    CONFIG
        .get()
        .ok_or_else(|| "TLS trust store could not be installed".to_owned())
}

fn tls_stream(
    stream: TcpStream,
    host: &str,
    config: Option<&Arc<rustls::ClientConfig>>,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, String> {
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| format!("invalid TLS server name {host:?}"))?;
    let config = match config {
        Some(config) => Arc::clone(config),
        None => Arc::clone(tls_config()?),
    };
    let connection =
        rustls::ClientConnection::new(config, name).map_err(|error| error.to_string())?;
    Ok(rustls::StreamOwned::new(connection, stream))
}

/// Whether `address` is a globally routable unicast address. Loopback,
/// private, link-local, CGNAT, unspecified, multicast, and IPv6 unique-local
/// ranges are refused so a data source cannot probe the host or its LAN.
fn public_address(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(address) => {
            !address.is_loopback()
                && !address.is_private()
                && !address.is_link_local()
                && !address.is_unspecified()
                && !address.is_multicast()
                && !address.is_broadcast()
                && !(address.octets()[0] == 100 && (64..128).contains(&address.octets()[1]))
                && address.octets()[0] != 0
        }
        std::net::IpAddr::V6(address) => {
            !address.is_loopback()
                && !address.is_unspecified()
                && !address.is_multicast()
                && (address.segments()[0] & 0xfe00) != 0xfc00
                && (address.segments()[0] & 0xffc0) != 0xfe80
        }
    }
}

fn find_headers_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn decode_chunked(mut body: &[u8]) -> Result<(Vec<u8>, Option<String>), String> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| "truncated chunk header".to_owned())?;
        let size = usize::from_str_radix(
            std::str::from_utf8(&body[..line_end])
                .map_err(|_| "invalid chunk header".to_owned())?
                .split(';')
                .next()
                .unwrap_or_default(),
            16,
        )
        .map_err(|_| "invalid chunk size".to_owned())?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok((out, None));
        }
        if size > body.len() || out.len() + size > MAX_RESPONSE_BYTES {
            return Err("truncated or oversized chunked body".to_owned());
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size..];
        if body.starts_with(b"\r\n") {
            body = &body[2..];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn serve(response: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream.write_all(response).unwrap();
        });
        format!("http://{address}/data.json")
    }

    #[test]
    fn a_bounded_http_get_returns_the_body() {
        let url = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]");
        assert_eq!(fetch_with(&url, None, true).unwrap(), b"[]");
    }

    #[test]
    fn redirects_and_size_bounds_are_enforced() {
        let url = serve(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/no\r\n\r\n");
        assert!(fetch_with(&url, None, true).is_err());
        let url = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n12345678");
        assert_eq!(fetch_with(&url, None, true).unwrap(), b"12345678");
        assert!(fetch("https://127.0.0.1:1/data.json").is_err());
    }

    /// Serves one HTTPS response under a fresh root, returning the URL and a
    /// client trusting only that root.
    fn serve_tls() -> (String, Arc<rustls::ClientConfig>) {
        use rcgen::{
            BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
            KeyPair, KeyUsagePurpose,
        };

        let root_key = KeyPair::generate().unwrap();
        let mut root_params = CertificateParams::default();
        root_params
            .distinguished_name
            .push(DnType::CommonName, "FreeMix title source test root");
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let root = root_params.self_signed(&root_key).unwrap();

        let leaf_key = KeyPair::generate().unwrap();
        let mut leaf_params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
        leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let issuer = Issuer::from_ca_cert_der(&root.der().clone(), &root_key).unwrap();
        let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

        let server_config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![leaf.der().clone()],
                    rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
                )
                .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let connection = rustls::ServerConnection::new(server_config).unwrap();
            let mut stream = rustls::StreamOwned::new(connection, stream);
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"a\":1}")
                .unwrap();
            stream.conn.send_close_notify();
            let _ = stream.flush();
        });

        let mut roots = rustls::RootCertStore::empty();
        roots.add(root.der().clone()).unwrap();
        let client = Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        (
            format!("https://localhost:{}/data.json", address.port()),
            client,
        )
    }

    #[test]
    fn a_local_tls_source_fetches_through_the_trusted_root() {
        let (url, client) = serve_tls();
        assert_eq!(fetch_with(&url, Some(&client), true).unwrap(), b"{\"a\":1}");
    }

    #[test]
    fn private_and_metadata_addresses_are_refused() {
        let url = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]");
        assert!(fetch(&url).is_err());
        assert!(fetch("http://169.254.169.254/latest/meta-data").is_err());
        assert!(fetch("http://10.0.0.1/data.json").is_err());
        assert!(fetch("http://[::1]/data.json").is_err());
        assert!(fetch_with(&url, None, true).is_ok());
    }
}
