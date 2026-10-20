use std::{
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use fm_protocol::{MAX_LINE_BYTES, WireMessage, decode_line};

use super::AppResult;

pub(crate) use access::{Reach, WebAccess, WebIdentity};
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

/// How often the gateway looks at the operator's certificate files.
const TRUST_POLL: Duration = Duration::from_secs(1);
/// How often it looks for changed LAN addresses (the local CA reissues its
/// leaf for them).
const LAN_SCAN: Duration = Duration::from_secs(15);

pub(super) struct WebGateway {
    address: SocketAddr,
    access: Arc<WebAccess>,
    events: Receiver<WebEvent>,
    cancel: Arc<AtomicBool>,
    accepting: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<bool>>,
    /// Reloads or reissues the certificate and follows the LAN addresses.
    maintenance_thread: Option<JoinHandle<()>>,
    /// The mDNS responder, while it advertises the gateway.
    mdns: Option<mdns::Mdns>,
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
    // Unset or empty: only paired devices are admitted.
    let Some(value) = env::var("FREEMIXD_WEB_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
    else {
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

/// `host:port`, bracketing an IPv6 literal.
fn host_authority(host: &str, port: u16) -> String {
    host.parse::<IpAddr>().map_or_else(
        |_| format!("{host}:{port}"),
        |address| authority(address, port),
    )
}

/// The addresses a gateway bound to `listen` is reached at on the LAN: the
/// listen address itself, or every non-loopback interface address (IPv4
/// for `0.0.0.0`, both families for `[::]`, never an IPv6 link-local one,
/// whose URL would need a zone), the default route's address first.
fn lan_addresses(listen: IpAddr) -> Vec<IpAddr> {
    if listen.is_loopback() {
        return Vec::new();
    }
    if !listen.is_unspecified() {
        return vec![listen];
    }
    let mut addresses: Vec<IpAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|interface| !interface.is_loopback())
        .map(|interface| interface.ip())
        .filter(|address| match address {
            IpAddr::V4(_) => true,
            IpAddr::V6(address) => listen.is_ipv6() && (address.segments()[0] & 0xffc0) != 0xfe80,
        })
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    if let Some(primary) = lan_address()
        && let Some(index) = addresses.iter().position(|address| *address == primary)
    {
        addresses.remove(index);
        addresses.insert(0, primary);
    }
    addresses
}

/// The URLs and certificate names for a gateway on `address` reached at
/// `lan`, under the operator's `--web-host` names and the mDNS `host`.
fn plan(
    settings: &WebSettings,
    address: SocketAddr,
    lan: &[IpAddr],
    mdns: Option<&str>,
    tls: bool,
) -> (Reach, Vec<String>) {
    let scheme = if tls { "https" } else { "http" };
    let port = address.port();
    let origin = |host: &str| format!("{scheme}://{}", host_authority(host, port));
    let main = settings.hosts.first().cloned().unwrap_or_else(|| {
        lan.first().copied().map_or_else(
            || {
                if address.ip().is_unspecified() {
                    Ipv4Addr::LOCALHOST.to_string()
                } else {
                    address.ip().to_string()
                }
            },
            |address| address.to_string(),
        )
    });
    let url = origin(&main);
    let mut urls: Vec<String> = Vec::new();
    let others = mdns
        .map(str::to_owned)
        .into_iter()
        .chain(lan.iter().map(ToString::to_string))
        .chain(settings.hosts.iter().skip(1).cloned());
    for host in others {
        let other = origin(&host);
        if other != url && !urls.contains(&other) && urls.len() < fm_protocol::MAX_WEB_URLS {
            urls.push(other);
        }
    }
    let mut names: Vec<String> = settings.hosts.clone();
    names.extend(mdns.map(str::to_owned));
    names.extend([
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ]);
    names.extend(lan.iter().map(ToString::to_string));
    if !address.ip().is_unspecified() {
        names.push(address.ip().to_string());
    }
    let mut unique = Vec::with_capacity(names.len());
    for name in names {
        if !unique.contains(&name) {
            unique.push(name);
        }
    }
    (
        Reach {
            url,
            urls,
            mdns: mdns.map(str::to_owned),
        },
        unique,
    )
}

/// Advertises the gateway at `address` as `host` over mDNS. When the
/// responder cannot start (no multicast), the gateway serves by address
/// alone and says so.
fn advertise(host: &str, computer: Option<&str>, address: SocketAddr) -> Option<mdns::Mdns> {
    let label = host.strip_suffix(".local").unwrap_or(host);
    let fixed = if address.ip().is_unspecified() {
        Vec::new()
    } else {
        vec![address.ip()]
    };
    let advertisement = mdns::Advertisement::new(label, computer, address.port(), fixed);
    match mdns::Mdns::start(&advertisement, address.is_ipv4()) {
        Ok(responder) => {
            eprintln!("FREEMIXD_WEB_MDNS\tv=1\tstate=advertised\thost={host}");
            Some(responder)
        }
        Err(error) => {
            eprintln!(
                "FREEMIXD_WEB_MDNS\tv=1\tstate=unavailable\thost={host}\terror={}",
                field(&error)
            );
            None
        }
    }
}

/// One readiness or status line value: no tabs or line breaks.
fn field(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// Keeps the certificate current and follows the LAN addresses until
/// `cancel`: reloads the operator's changed files, reissues the local CA
/// leaf for new addresses, and updates the URLs the console shows. Each
/// change is reported once on standard error, never with key material.
fn maintain(
    access: &WebAccess,
    settings: &WebSettings,
    address: SocketAddr,
    mut lan: Vec<IpAddr>,
    mut names: Vec<String>,
    cancel: &AtomicBool,
) {
    let mut scanned = Instant::now();
    let mut last_error: Option<String> = None;
    while !cancel.load(Ordering::Acquire) {
        let wake = Instant::now() + TRUST_POLL;
        while Instant::now() < wake {
            if cancel.load(Ordering::Acquire) {
                return;
            }
            thread::sleep(RELAY_POLL * 4);
        }
        let tls = access.tls();
        if scanned.elapsed() >= LAN_SCAN {
            scanned = Instant::now();
            let current = lan_addresses(address.ip());
            if current != lan {
                lan = current;
                let mdns = access.reach().mdns;
                let (reach, planned) =
                    plan(settings, address, &lan, mdns.as_deref(), tls.is_some());
                eprintln!(
                    "FREEMIXD_WEB_REACH\tv=1\turl={}\turls={}",
                    field(&reach.url),
                    field(&reach.urls.join(","))
                );
                access.set_reach(reach);
                names = planned;
            }
        }
        let Some(tls) = tls else {
            continue;
        };
        match tls.refresh(&names, tls::now_ms()) {
            Ok(true) => {
                last_error = None;
                eprintln!(
                    "FREEMIXD_WEB_TLS_RELOAD\tv=1\tstate=ok\tfingerprint={}",
                    tls.fingerprint()
                );
            }
            Ok(false) => {}
            Err(error) => {
                if last_error.as_deref() != Some(error.as_str()) {
                    eprintln!(
                        "FREEMIXD_WEB_TLS_RELOAD\tv=1\tstate=failed\tfingerprint={}\terror={}",
                        tls.fingerprint(),
                        field(&error)
                    );
                    last_error = Some(error);
                }
            }
        }
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
        let lan = lan_addresses(address.ip());
        let computer = mdns::hostname();
        let mdns_host = settings.advertises(address).then(|| {
            let label = settings
                .mdns_name
                .clone()
                .unwrap_or_else(|| mdns::default_label(computer.as_deref()));
            format!("{label}.local")
        });
        let tls_on = settings.tls != settings::WebTls::Off;
        let (reach, names) = plan(settings, address, &lan, mdns_host.as_deref(), tls_on);
        let tls = tls::load(&settings.tls, state_dir.as_deref(), &names)?;
        let config = tls.as_ref().map(tls::TlsState::server_config).transpose()?;
        let access = Arc::new(WebAccess::open(
            reach.url.clone(),
            tls,
            static_token_from_environment()?,
            state_dir.as_deref(),
        )?);
        let mdns = mdns_host
            .as_deref()
            .and_then(|host| advertise(host, computer.as_deref(), address));
        let reach = Reach {
            mdns: mdns.as_ref().and(reach.mdns),
            urls: if mdns.is_some() {
                reach.urls
            } else {
                let mdns_origin = mdns_host.as_deref().map(|host| {
                    format!(
                        "{}://{}",
                        if tls_on { "https" } else { "http" },
                        host_authority(host, address.port())
                    )
                });
                reach
                    .urls
                    .into_iter()
                    .filter(|url| Some(url) != mdns_origin.as_ref())
                    .collect()
            },
            url: reach.url,
        };
        access.set_reach(reach);
        let (events_tx, events_rx) = mpsc::sync_channel(MAX_WEB_CLIENTS);
        let cancel = Arc::new(AtomicBool::new(false));
        let accepting = Arc::new(AtomicBool::new(false));

        // Requests may name the advertised `.local` host besides addresses,
        // `localhost`, and the `--web-host` names.
        let mut hosts = settings.hosts.clone();
        if mdns.is_some() {
            hosts.extend(mdns_host.clone());
        }
        let gateway = Gateway {
            events: events_tx,
            cancel: Arc::clone(&cancel),
            access: Arc::clone(&access),
            tls: config,
            hosts: Arc::new(hosts),
            clients: Arc::new(AtomicUsize::new(0)),
        };
        let listener_accepting = Arc::clone(&accepting);
        let listener_thread = thread::Builder::new()
            .name("freemixd-web-listener".into())
            .spawn(move || listener_loop(&listener, &gateway, &listener_accepting))?;
        let maintenance_thread = {
            let access = Arc::clone(&access);
            let settings = settings.clone();
            let cancel = Arc::clone(&cancel);
            thread::Builder::new()
                .name("freemixd-web-trust".into())
                .spawn(move || maintain(&access, &settings, address, lan, names, &cancel))?
        };

        Ok(Self {
            address,
            access,
            events: events_rx,
            cancel,
            accepting,
            listener_thread: Some(listener_thread),
            maintenance_thread: Some(maintenance_thread),
            mdns,
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
        let tls = self.access.tls()?;
        let mut record = format!(
            "FREEMIXD_WEB_TLS\tv=1\turl={}\tfingerprint={}\ttrust={}",
            self.access.url(),
            tls.fingerprint(),
            tls.trust().token()
        );
        if let Some(ca) = tls.ca() {
            record.push_str("\tca_fingerprint=");
            record.push_str(ca.fingerprint());
        }
        if let Some(host) = self.access.reach().mdns {
            record.push_str("\tmdns=");
            record.push_str(&host);
        }
        Some(record)
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
        drop(self.mdns.take());
        let maintained = self
            .maintenance_thread
            .take()
            .is_none_or(|handle| handle.join().is_ok());
        let clean = self
            .listener_thread
            .take()
            .is_none_or(|handle| handle.join().unwrap_or(false))
            && maintained;
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
        drop(self.mdns.take());
        if let Some(handle) = self.maintenance_thread.take() {
            let _ = handle.join();
        }
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
mod certs;
mod http;
mod local_ca;
mod mdns;
mod page;
mod settings;
mod tls;
mod worker;
use worker::listener_loop;

#[cfg(test)]
mod tests {
    use super::{WebSettings, base64url, base64url_decode, plan};

    #[test]
    fn the_plan_lists_every_origin_and_certificate_name() {
        let settings = WebSettings::default();
        let address = "0.0.0.0:8443".parse().unwrap();
        let lan = ["192.168.1.20".parse().unwrap(), "10.0.0.9".parse().unwrap()];
        let (reach, names) = plan(&settings, address, &lan, Some("freemix-studio.local"), true);
        assert_eq!(reach.url, "https://192.168.1.20:8443");
        assert_eq!(
            reach.urls,
            ["https://freemix-studio.local:8443", "https://10.0.0.9:8443"]
        );
        assert_eq!(reach.mdns.as_deref(), Some("freemix-studio.local"));
        for name in [
            "freemix-studio.local",
            "localhost",
            "127.0.0.1",
            "::1",
            "192.168.1.20",
            "10.0.0.9",
        ] {
            assert!(names.iter().any(|known| known == name), "{name}");
        }
        // Loopback: no LAN, no mDNS, plain HTTP.
        let (reach, _) = plan(
            &settings,
            "127.0.0.1:4000".parse().unwrap(),
            &[],
            None,
            false,
        );
        assert_eq!(reach.url, "http://127.0.0.1:4000");
        assert!(reach.urls.is_empty() && reach.mdns.is_none());
        // IPv6 origins are bracketed.
        let (reach, _) = plan(
            &settings,
            "[::]:8443".parse().unwrap(),
            &["fd00::20".parse().unwrap()],
            None,
            true,
        );
        assert_eq!(reach.url, "https://[fd00::20]:8443");
    }

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
