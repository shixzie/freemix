use std::{
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use fm_protocol::{MAX_LINE_BYTES, WireMessage, decode_line};

use super::AppResult;

pub(crate) use access::{WebAccess, WebIdentity};
pub(crate) use settings::WebSettings;

const PATH: &str = "/v1/control";
const TOKEN_MIN_BYTES: usize = 32;
const TOKEN_MAX_BYTES: usize = 256;
const SOCKET_HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(500);
/// A TLS handshake plus the request head: phones on Wi-Fi need a few round
/// trips, so TLS connections get longer than plain loopback ones.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const RELAY_POLL: Duration = Duration::from_millis(25);
const WEBSOCKET_READ_BUFFER: usize = 8 * 1024;
const WEBSOCKET_WRITE_BUFFER: usize = 8 * 1024;
const WEBSOCKET_MAX_WRITE_BUFFER: usize = 2 * MAX_LINE_BYTES;
const WEBSOCKET_MESSAGE_LIMIT: usize = MAX_LINE_BYTES;

const INBOUND_CAPACITY: usize = 8;
const OUTBOUND_CAPACITY: usize = 32;

/// The most Web clients (tally and operator pages, native WebSocket
/// clients) connected at once. Each is its own control session with its own
/// role, heartbeat, subscription, and outbound queue, relayed by its own
/// gateway thread, so one slow browser never stalls another or the daemon.
/// An authorized upgrade beyond the bound is refused with
/// `503 Service Unavailable` and the page retries with backoff.
pub(super) const MAX_WEB_CLIENTS: usize = 16;
/// Connections still serving a page or completing an upgrade, on top of the
/// connected clients. Each is bounded by its handshake timeout; a
/// connection beyond this bound is closed unanswered.
const MAX_PENDING_CONNECTIONS: usize = 8;

pub(super) struct WebGateway {
    address: SocketAddr,
    access: Arc<WebAccess>,
    events: Receiver<WebEvent>,
    cancel: Arc<AtomicBool>,
    accepting: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<bool>>,
}

pub(super) enum WebEvent {
    Connected(WebConnection),
}

pub(super) struct WebConnection {
    pub(super) inbound: Receiver<WireMessage>,
    pub(super) outbound: SyncSender<Vec<u8>>,
    pub(super) acknowledgements: Receiver<()>,
    pub(super) cancel: Arc<AtomicBool>,
    /// Who the client authenticated as; its session's roles follow.
    pub(super) identity: WebIdentity,
    /// The client's gateway slot, held until the scheduler drops the session,
    /// so the gateway never admits more clients than the scheduler serves.
    pub(super) slot: ClientSlot,
}

/// Subprotocol a browser offers, and the gateway selects, for control.
const CONTROL_SUBPROTOCOL: &str = "freemix.control.v1";
/// Browsers cannot set `Authorization` on a WebSocket, so the page presents
/// its token as `freemix.bearer.<unpadded base64url(token)>` in its offered
/// subprotocol list. The gateway never selects or echoes it.
const BEARER_SUBPROTOCOL_PREFIX: &str = "freemix.bearer.";

/// The optional operator token (`FREEMIXD_WEB_TOKEN`).
fn static_token_from_environment() -> AppResult<Option<Vec<u8>>> {
    let Ok(value) = env::var("FREEMIXD_WEB_TOKEN") else {
        return Ok(None);
    };
    let bytes = value.into_bytes();
    if !(TOKEN_MIN_BYTES..=TOKEN_MAX_BYTES).contains(&bytes.len())
        || !bytes.iter().all(u8::is_ascii_graphic)
    {
        return Err("FREEMIXD_WEB_TOKEN must be 32..=256 ASCII graphic bytes".into());
    }
    Ok(Some(bytes))
}

/// Unpadded RFC 4648 base64url.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity((bytes.len() * 4).div_ceil(3));
    for chunk in bytes.chunks(3) {
        let value = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |value, (index, byte)| {
                value | (u32::from(*byte) << (16 - 8 * index))
            });
        for index in 0..=chunk.len() {
            let sextet = (value >> (18 - 6 * index)) & 0x3f;
            encoded.push(char::from(ALPHABET[sextet as usize]));
        }
    }
    encoded
}

/// Decodes unpadded base64url exactly as [`base64url`] encodes it.
fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    fn sextet(byte: u8) -> Option<u32> {
        Some(u32::from(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        }))
    }
    if text.len() % 4 == 1 {
        return None;
    }
    let mut decoded = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.as_bytes().chunks(4) {
        let mut value = 0_u32;
        for (index, byte) in chunk.iter().enumerate() {
            value |= sextet(*byte)? << (18 - 6 * index);
        }
        let bytes = value.to_be_bytes();
        decoded.extend_from_slice(&bytes[1..chunk.len()]);
    }
    // Only the canonical encoding is accepted.
    (base64url(&decoded) == text).then_some(decoded)
}

/// The address a phone on the LAN reaches when the gateway listens on every
/// interface: the source address of the default route. Nothing is sent.
fn lan_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    let address = socket.local_addr().ok()?.ip();
    (!address.is_unspecified() && !address.is_loopback()).then_some(address)
}

fn authority(address: IpAddr, port: u16) -> String {
    match address {
        IpAddr::V4(address) => format!("{address}:{port}"),
        IpAddr::V6(address) => format!("[{address}]:{port}"),
    }
}

impl WebGateway {
    /// Starts the gateway on `listener` with `settings`: loads or creates
    /// the TLS certificate and the paired devices, then accepts connections
    /// once [`Self::start_accepting`] is called.
    ///
    /// # Errors
    ///
    /// Invalid TLS files or token, or an unreadable state directory.
    pub(super) fn bind(listener: TcpListener, settings: &WebSettings) -> AppResult<Self> {
        let address = listener.local_addr()?;
        let state_dir = settings.resolved_state_dir(|name| env::var(name).ok());
        let lan = address.ip().is_unspecified().then(lan_address).flatten();
        let mut names: Vec<String> = settings.hosts.clone();
        names.extend([
            "localhost".to_owned(),
            "127.0.0.1".to_owned(),
            "::1".to_owned(),
        ]);
        if let Some(lan) = lan {
            names.push(lan.to_string());
        }
        if !address.ip().is_unspecified() {
            names.push(address.ip().to_string());
        }
        let tls = tls::load(&settings.tls, state_dir.as_deref(), &names)?;
        let host = settings.hosts.first().map_or_else(
            || {
                authority(
                    lan.unwrap_or_else(|| {
                        if address.ip().is_unspecified() {
                            IpAddr::V4(Ipv4Addr::LOCALHOST)
                        } else {
                            address.ip()
                        }
                    }),
                    address.port(),
                )
            },
            |name| {
                if name.parse::<IpAddr>().is_ok() {
                    authority(name.parse().expect("checked"), address.port())
                } else {
                    format!("{name}:{}", address.port())
                }
            },
        );
        let url = format!("{}://{host}", if tls.is_some() { "https" } else { "http" });
        let access = Arc::new(WebAccess::open(
            url,
            tls.as_ref().map(|material| material.fingerprint.clone()),
            static_token_from_environment()?,
            state_dir.as_deref(),
        )?);
        let (events_tx, events_rx) = mpsc::sync_channel(MAX_WEB_CLIENTS);
        let cancel = Arc::new(AtomicBool::new(false));
        let accepting = Arc::new(AtomicBool::new(false));

        let gateway = Gateway {
            events: events_tx,
            cancel: Arc::clone(&cancel),
            access: Arc::clone(&access),
            tls: tls.map(|material| material.config),
            hosts: Arc::new(settings.hosts.clone()),
            clients: Arc::new(AtomicUsize::new(0)),
        };
        let listener_accepting = Arc::clone(&accepting);
        let listener_thread = thread::Builder::new()
            .name("freemixd-web-listener".into())
            .spawn(move || listener_loop(&listener, &gateway, &listener_accepting))?;

        Ok(Self {
            address,
            access,
            events: events_rx,
            cancel,
            accepting,
            listener_thread: Some(listener_thread),
        })
    }

    pub(super) fn address(&self) -> SocketAddr {
        self.address
    }

    /// Pairing and paired devices, shared with the session scheduler.
    pub(super) fn access(&self) -> &Arc<WebAccess> {
        &self.access
    }

    /// `FREEMIXD_WEB_TLS` readiness: the URL phones open and the certificate
    /// fingerprint to compare, when TLS is on.
    pub(super) fn tls_record(&self) -> Option<String> {
        let fingerprint = self.access.fingerprint()?;
        Some(format!(
            "FREEMIXD_WEB_TLS\tv=1\turl={}\tfingerprint={fingerprint}",
            self.access.url()
        ))
    }

    pub(super) fn start_accepting(&self) {
        self.accepting.store(true, Ordering::Release);
    }

    pub(super) fn try_event(&self) -> Result<Option<WebEvent>, ()> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(()),
        }
    }

    /// Stops accepting, then joins the listener, which joins every
    /// connection thread (each ends within its handshake timeout or relay
    /// poll).
    pub(super) fn shutdown(mut self) -> AppResult<()> {
        self.cancel.store(true, Ordering::Release);
        let clean = self
            .listener_thread
            .take()
            .is_none_or(|handle| handle.join().unwrap_or(false));
        if clean {
            Ok(())
        } else {
            Err("freemixd WebSocket gateway thread panicked".into())
        }
    }
}

impl Drop for WebGateway {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(handle) = self.listener_thread.take() {
            let _ = handle.join();
        }
    }
}

/// What every connection thread shares.
#[derive(Clone)]
struct Gateway {
    events: SyncSender<WebEvent>,
    cancel: Arc<AtomicBool>,
    access: Arc<WebAccess>,
    /// The TLS configuration; plain HTTP (loopback only) without one.
    tls: Option<Arc<rustls::ServerConfig>>,
    /// The `--web-host` names served besides addresses and `localhost`.
    hosts: Arc<Vec<String>>,
    /// Upgrades holding a client slot: connected, or authorized and
    /// completing their handshake.
    clients: Arc<AtomicUsize>,
}

/// One of the [`MAX_WEB_CLIENTS`] slots, released when dropped.
pub(super) struct ClientSlot(Arc<AtomicUsize>);

impl ClientSlot {
    fn acquire(clients: &Arc<AtomicUsize>) -> Option<Self> {
        let mut held = clients.load(Ordering::Acquire);
        while held < MAX_WEB_CLIENTS {
            match clients.compare_exchange_weak(held, held + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return Some(Self(Arc::clone(clients))),
                Err(current) => held = current,
            }
        }
        None
    }
}

impl Drop for ClientSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

mod access;
mod http;
mod page;
mod settings;
mod tls;
mod worker;
use worker::listener_loop;

#[cfg(test)]
mod tests {
    use super::{base64url, base64url_decode};

    #[test]
    fn base64url_is_unpadded_rfc4648() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
            ("\u{3f}\u{3f}>", "Pz8-"),
            ("??/", "Pz8v"),
            ("\u{7e}\u{7e}\u{7f}", "fn5_"),
        ] {
            assert_eq!(base64url(plain.as_bytes()), encoded, "{plain:?}");
            assert_eq!(
                base64url_decode(encoded).as_deref(),
                Some(plain.as_bytes()),
                "{encoded:?}"
            );
        }
        for invalid in ["Z", "Zh", "Zm9=", "Zm9v+", "Zm 9"] {
            assert_eq!(base64url_decode(invalid), None, "{invalid:?}");
        }
    }
}
