//! The `LiveLAN` viewer listener (OR-017).
//!
//! [`LiveLanServer`] serves people on the local network a browser page that
//! plays the daemon's `LiveLAN` HLS stream: Program, or one chosen project
//! output, encoded by the native media plane into a daemon-owned directory.
//! It never touches engine, control, or render state; the render loop
//! publishes what may be served through a [`LiveLanHandle`] and reads back
//! the viewer count and the bytes served.
//!
//! The stream is HLS with fragmented-MP4 (CMAF) segments: a browser that
//! plays HLS itself (Safari, iOS, Android, recent Chromium) is handed the
//! playlist, and every other browser with Media Source Extensions (desktop
//! Firefox; Chrome or Edge without HLS of their own) plays it through the
//! page's own small player, which fetches the playlist, the `init.mp4`
//! initialization segment, and the media segments and appends them to a
//! `SourceBuffer`.
//!
//! Access is a session: the page posts the optional PIN to
//! `POST /v1/live/session`, and a correct PIN (or none, when no PIN is set)
//! returns an `HttpOnly` session cookie that the playlist, initialization
//! segment, and media segment requests must carry. A viewer is a session
//! that fetched media within the last [`VIEWER_WINDOW`]; a new session is
//! refused while the configured maximum is watching; a page joining again
//! with its live session keeps it. Wrong PINs are rate-limited per IPv4
//! address or remote IPv6 `/64` (an on-link IPv6 peer, link-local or in
//! the server's own `/64`, counts as its own address): each run of [`MAX_PIN_FAILURES`] locks the
//! address out, for [`BASE_LOCKOUT`] at first and twice as long after each
//! further run, up to [`MAX_LOCKOUT`]. Disabling `LiveLAN` or changing its
//! PIN or source ends every session at once, and an encoder that starts
//! again ends the sessions it served.
//!
//! Every bound is fixed: at most [`MAX_CONNECTIONS`] connections are served
//! at once (each on its own short-lived thread), a request must arrive
//! within [`REQUEST_DEADLINE`], a request head is at most
//! [`MAX_REQUEST_BYTES`], a body at most [`MAX_BODY_BYTES`], a served file
//! at most [`MAX_FILE_BYTES`], and the session and failure tables are
//! capped. The PIN is compared in constant time, whatever its length, and
//! is never logged, echoed, or rendered by `Debug`. A request path is never
//! joined to a directory: only the `index.m3u8`, `init.mp4`, and
//! `segment-<digits>.m4s` leaves are served.

use std::{
    collections::HashMap,
    fmt::{self, Write as _},
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::AppResult;

/// Connections served at once; more are closed at once.
pub const MAX_CONNECTIONS: usize = 64;
/// Request-head cap.
pub const MAX_REQUEST_BYTES: usize = 4 * 1024;
/// Session-request body cap (`pin=` and at most twelve digits fit easily).
pub const MAX_BODY_BYTES: usize = 64;
/// Largest playlist or segment served.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// A session that fetched media this recently is a viewer.
pub const VIEWER_WINDOW: Duration = Duration::from_secs(10);
/// A session idle this long is forgotten.
pub const SESSION_IDLE: Duration = Duration::from_mins(1);
/// Wrong PINs one address may send per [`FAILURE_WINDOW`] before it is
/// locked out.
pub const MAX_PIN_FAILURES: u8 = 5;
/// How long a run of wrong PINs is remembered.
pub const FAILURE_WINDOW: Duration = Duration::from_mins(1);
/// The first lockout; each further one doubles, up to [`MAX_LOCKOUT`].
pub const BASE_LOCKOUT: Duration = Duration::from_mins(1);
/// The longest lockout.
pub const MAX_LOCKOUT: Duration = Duration::from_hours(1);
/// How long after its last lockout (or wrong PIN) an address's lockouts are
/// remembered, so the next lockout escalates; then it starts afresh.
pub const LOCKOUT_MEMORY: Duration = MAX_LOCKOUT;
/// Bytes every PIN comparison walks at least: more than any valid PIN, so
/// how long a comparison takes says nothing about the PIN's length.
const PIN_COMPARE_BYTES: usize = MAX_BODY_BYTES;
/// Sessions remembered at most, active or idle.
const MAX_SESSIONS: usize = 128;
/// Addresses whose wrong PINs are remembered at most.
const MAX_FAILURE_ADDRESSES: usize = 1024;
const SESSION_TOKEN_BYTES: usize = 16;
const COOKIE_NAME: &str = "fmlive";
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// Time allowed to receive one request, head and body: a client trickling
/// bytes cannot hold one of the [`MAX_CONNECTIONS`] for longer.
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// Time allowed to send one response, head and body.
const RESPONSE_DEADLINE: Duration = Duration::from_secs(15);
const ACCEPT_POLL: Duration = Duration::from_millis(10);
const COPY_CHUNK_BYTES: usize = 16 * 1024;
const PLAYLIST_LEAF: &str = "index.m3u8";
const INIT_LEAF: &str = "init.mp4";
const PLAYLIST_CONTENT_TYPE: &str = "application/vnd.apple.mpegurl";
const SEGMENT_CONTENT_TYPE: &str = "video/mp4";

/// The viewer page.
const PAGE: &str = include_str!("web/live_lan.html");

/// What viewers may watch: the PIN, if any, and how many may watch at once.
#[derive(Clone, Eq, PartialEq)]
pub struct LiveLanAccess {
    pin: Option<String>,
    max_viewers: u8,
}

impl LiveLanAccess {
    #[must_use]
    pub fn new(pin: Option<&str>, max_viewers: u8) -> Self {
        Self {
            pin: pin.map(str::to_owned),
            max_viewers,
        }
    }
}

impl fmt::Debug for LiveLanAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveLanAccess")
            .field("pin", &self.pin.as_ref().map(|_| "****"))
            .field("max_viewers", &self.max_viewers)
            .finish()
    }
}

/// Why a session or media request is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Refusal {
    /// `LiveLAN` is off.
    Off,
    /// The PIN is wrong, or the request carries no live session.
    Unauthorized,
    /// Too many wrong PINs from this address: locked out for this many more
    /// seconds.
    RateLimited(u64),
    /// The configured maximum is watching.
    Full,
}

impl Refusal {
    const fn status(self) -> (u16, &'static str, &'static str) {
        match self {
            Self::Off => (503, "Service Unavailable", "off"),
            Self::Unauthorized => (401, "Unauthorized", "pin"),
            Self::RateLimited(_) => (429, "Too Many Requests", "wait"),
            Self::Full => (503, "Service Unavailable", "full"),
        }
    }
}

#[derive(Default)]
struct AccessState {
    access: Option<LiveLanAccess>,
    /// The encoder's output directory, once it publishes.
    media: Option<PathBuf>,
    /// The last directory published, kept while nothing is.
    published: Option<PathBuf>,
    /// Counts the directories published: a session served media of one
    /// stream is ended when another starts.
    stream: u64,
    /// Session token to when it last fetched media (or was created).
    sessions: HashMap<String, Session>,
    /// Wrong PINs per [`throttle_key`].
    failures: HashMap<IpAddr, Failures>,
}

/// The bucket wrong PINs from `address`, which reached the server at
/// `local`, count against: an IPv4 address (an IPv4-mapped IPv6 peer of a
/// dual-stack listener included), an on-link IPv6 address (link-local, or
/// in `local`'s own `/64`) as itself, since every host of the venue LAN
/// shares that `/64`, or the `/64` network of any other IPv6 address,
/// since one remote host may hold any number of addresses in its `/64` and
/// would otherwise get a fresh allowance each (grouped as the Web
/// gateway's connection bound and pairing are, [`crate::web::peer_group_at`]).
fn throttle_key(address: IpAddr, local: Option<IpAddr>) -> IpAddr {
    crate::web::peer_group_at(address, local)
}

#[derive(Clone, Copy)]
struct Session {
    last_seen: Instant,
    /// Whether the session has fetched media and so counts as a viewer.
    watching: bool,
    /// The [`AccessState::stream`] the session was served media of.
    stream: Option<u64>,
}

/// Wrong PINs from one [`throttle_key`].
#[derive(Clone, Copy)]
struct Failures {
    /// Wrong PINs in the current run, since the last lockout.
    count: u8,
    /// When the current run began.
    since: Instant,
    /// Lockouts so far: the next one lasts `BASE_LOCKOUT * 2^lockouts`.
    lockouts: u32,
    /// The end of the current (or last) lockout.
    locked_until: Option<Instant>,
    /// The last wrong PIN.
    last: Instant,
}

impl Failures {
    /// Whether this record still matters at `now`: a lockout is running, a
    /// run is in progress, or an earlier lockout still escalates the next.
    fn remembered(&self, now: Instant) -> bool {
        let quiet_since = self
            .locked_until
            .map_or(self.last, |until| until.max(self.last));
        (self.count > 0 && now.duration_since(self.since) < FAILURE_WINDOW)
            || now < quiet_since
            || (self.lockouts > 0 && now.duration_since(quiet_since) < LOCKOUT_MEMORY)
    }

    /// Seconds left of a running lockout, rounded up.
    fn locked_for(&self, now: Instant) -> Option<u64> {
        let left = self.locked_until?.checked_duration_since(now)?;
        (!left.is_zero()).then(|| left.as_secs() + u64::from(left.subsec_nanos() > 0))
    }
}

/// How long the lockout after `earlier` lockouts lasts.
fn lockout(earlier: u32) -> Duration {
    BASE_LOCKOUT
        .checked_mul(1_u32.checked_shl(earlier).unwrap_or(u32::MAX))
        .map_or(MAX_LOCKOUT, |lockout| lockout.min(MAX_LOCKOUT))
}

impl AccessState {
    fn prune(&mut self, now: Instant) {
        self.sessions
            .retain(|_, session| now.duration_since(session.last_seen) < SESSION_IDLE);
        self.failures.retain(|_, failures| failures.remembered(now));
    }

    fn viewers(&self, now: Instant) -> usize {
        self.sessions
            .values()
            .filter(|session| {
                session.watching && now.duration_since(session.last_seen) < VIEWER_WINDOW
            })
            .count()
    }

    /// Seconds left of the lockout of `key` (a [`throttle_key`]), if it is
    /// locked out.
    fn locked_for(&self, key: IpAddr, now: Instant) -> Option<u64> {
        self.failures
            .get(&key)
            .and_then(|failures| failures.locked_for(now))
    }

    /// Counts a wrong PIN against `address` (a [`throttle_key`]); the last
    /// of a run of [`MAX_PIN_FAILURES`] locks it out, each lockout twice as
    /// long as the one before (up to [`MAX_LOCKOUT`]).
    fn record_failure(&mut self, address: IpAddr, now: Instant) {
        if !self.failures.contains_key(&address) && self.failures.len() >= MAX_FAILURE_ADDRESSES {
            return;
        }
        let failures = self.failures.entry(address).or_insert(Failures {
            count: 0,
            since: now,
            lockouts: 0,
            locked_until: None,
            last: now,
        });
        if failures.count == 0 || now.duration_since(failures.since) >= FAILURE_WINDOW {
            failures.count = 0;
            failures.since = now;
        }
        failures.count = failures.count.saturating_add(1);
        failures.last = now;
        if failures.count >= MAX_PIN_FAILURES {
            failures.locked_until = Some(now + lockout(failures.lockouts));
            failures.lockouts = failures.lockouts.saturating_add(1);
            failures.count = 0;
        }
    }

    /// Opens a session for a correct `pin` from `address`. A request that
    /// already carries a live session (`current`, a page joining again after
    /// a playback error or a reload) keeps it, and so keeps its seat rather
    /// than being refused by the viewer limit it counts toward itself.
    #[cfg(test)]
    fn open(
        &mut self,
        address: IpAddr,
        pin: &str,
        current: Option<&str>,
        now: Instant,
    ) -> Result<String, Refusal> {
        self.open_at(address, None, pin, current, now)
    }

    /// [`Self::open`] for a request that reached the server at `local`,
    /// which tells an on-link IPv6 peer ([`throttle_key`]).
    fn open_at(
        &mut self,
        address: IpAddr,
        local: Option<IpAddr>,
        pin: &str,
        current: Option<&str>,
        now: Instant,
    ) -> Result<String, Refusal> {
        let address = throttle_key(address, local);
        self.prune(now);
        let Some(access) = &self.access else {
            return Err(Refusal::Off);
        };
        if let Some(seconds) = self.locked_for(address, now) {
            return Err(Refusal::RateLimited(seconds));
        }
        // A full failure table fails closed for addresses it cannot track.
        if access.pin.is_some()
            && self.failures.len() >= MAX_FAILURE_ADDRESSES
            && !self.failures.contains_key(&address)
        {
            return Err(Refusal::RateLimited(FAILURE_WINDOW.as_secs()));
        }
        if let Some(expected) = &access.pin
            && !constant_time_eq(expected.as_bytes(), pin.as_bytes())
        {
            self.record_failure(address, now);
            return Err(Refusal::Unauthorized);
        }
        if let Some(current) = current
            && let Some(session) = self.sessions.get_mut(current)
        {
            session.last_seen = now;
            return Ok(current.to_owned());
        }
        if self.viewers(now) >= usize::from(access.max_viewers) {
            return Err(Refusal::Full);
        }
        if self.sessions.len() >= MAX_SESSIONS {
            // Forget the longest-idle session that is not watching to make
            // room: a flood of joins never ends a viewer's session. There is
            // always one, since at most `MAX_LIVE_LAN_VIEWERS` are watching.
            if let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, session)| {
                    (
                        session.watching && now.duration_since(session.last_seen) < VIEWER_WINDOW,
                        session.last_seen,
                    )
                })
                .map(|(token, _)| token.clone())
            {
                self.sessions.remove(&oldest);
            }
        }
        let token = session_token().ok_or(Refusal::Off)?;
        self.sessions.insert(
            token.clone(),
            Session {
                last_seen: now,
                watching: false,
                stream: None,
            },
        );
        Ok(token)
    }

    /// Admits a media fetch of session `token`, returning the directory to
    /// serve from.
    fn admit(&mut self, token: Option<&str>, now: Instant) -> Result<PathBuf, Refusal> {
        self.prune(now);
        let Some(access) = &self.access else {
            return Err(Refusal::Off);
        };
        let maximum = usize::from(access.max_viewers);
        let viewers = self.viewers(now);
        let Some((token, session)) =
            token.and_then(|token| self.sessions.get_mut(token).map(|session| (token, session)))
        else {
            return Err(Refusal::Unauthorized);
        };
        // Served media of an encoder that has since started again: the
        // page must join again and start the new stream from its beginning.
        if self.media.is_some() && session.stream.is_some_and(|stream| stream != self.stream) {
            self.sessions.remove(token);
            return Err(Refusal::Unauthorized);
        }
        let counted = session.watching && now.duration_since(session.last_seen) < VIEWER_WINDOW;
        if !counted && viewers >= maximum {
            return Err(Refusal::Full);
        }
        session.last_seen = now;
        session.watching = true;
        let media = self.media.clone().ok_or(Refusal::Off)?;
        session.stream = Some(self.stream);
        Ok(media)
    }
}

struct Shared {
    sessions: Mutex<AccessState>,
    egress_bytes: AtomicU64,
    connections: AtomicUsize,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, AccessState> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The render loop's side of a running [`LiveLanServer`].
#[derive(Clone)]
pub struct LiveLanHandle {
    shared: Arc<Shared>,
    address: SocketAddr,
}

impl LiveLanHandle {
    /// Sets who may watch; `None` turns `LiveLAN` off. A change of PIN, or
    /// turning it off, ends every session.
    pub fn set_access(&self, access: Option<LiveLanAccess>) {
        let mut sessions = self.shared.lock();
        if sessions.access == access {
            return;
        }
        let revoke = match (&sessions.access, &access) {
            (Some(before), Some(after)) => before.pin != after.pin,
            _ => true,
        };
        if revoke {
            sessions.sessions.clear();
            sessions.failures.clear();
        }
        if access.is_none() {
            sessions.media = None;
        }
        sessions.access = access;
    }

    /// Sets the directory the encoder publishes into; `None` while nothing
    /// is published. Publishing another directory than the last (an encoder
    /// started again, whose stream starts over) ends every session that was
    /// served media of the last, so each of those viewers joins again and
    /// starts the new stream from its beginning.
    pub fn set_media(&self, media: Option<PathBuf>) {
        let mut state = self.shared.lock();
        if let Some(directory) = &media
            && state.published.as_ref() != Some(directory)
        {
            state.stream = state.stream.wrapping_add(1);
            state.published = Some(directory.clone());
        }
        state.media = media;
    }

    /// Ends every session, so each viewer joins again and reloads the
    /// playlist (another source starts a new one).
    pub fn end_sessions(&self) {
        self.shared.lock().sessions.clear();
    }

    /// Sessions that fetched media within [`VIEWER_WINDOW`].
    #[must_use]
    pub fn viewers(&self) -> u8 {
        let now = Instant::now();
        let mut sessions = self.shared.lock();
        sessions.prune(now);
        u8::try_from(sessions.viewers(now)).unwrap_or(u8::MAX)
    }

    /// Media bytes served so far.
    #[must_use]
    pub fn egress_bytes(&self) -> u64 {
        self.shared.egress_bytes.load(Ordering::Relaxed)
    }

    /// The bound address.
    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }
}

/// The `LiveLAN` listener's accept thread and its shared state.
pub struct LiveLanServer {
    handle: LiveLanHandle,
    cancel: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl LiveLanServer {
    /// Binds `address` and starts accepting. `LiveLAN` starts off: nothing is
    /// served until [`LiveLanHandle::set_access`].
    ///
    /// # Errors
    ///
    /// Returns the bind or thread-spawn failure.
    pub fn start(address: SocketAddr) -> AppResult<Self> {
        let listener = TcpListener::bind(address)
            .map_err(|error| format!("--live-lan-listen could not bind {address}: {error}"))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            sessions: Mutex::new(AccessState::default()),
            egress_bytes: AtomicU64::new(0),
            connections: AtomicUsize::new(0),
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let accept = {
            let shared = Arc::clone(&shared);
            let cancel = Arc::clone(&cancel);
            thread::Builder::new()
                .name("freemixd-live-lan".into())
                .spawn(move || accept_loop(&listener, &shared, &cancel))?
        };
        Ok(Self {
            handle: LiveLanHandle { shared, address },
            cancel,
            accept: Some(accept),
        })
    }

    #[must_use]
    pub fn handle(&self) -> LiveLanHandle {
        self.handle.clone()
    }

    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.handle.address
    }

    /// Stops accepting. Connections in flight finish within their own
    /// deadlines.
    ///
    /// # Errors
    ///
    /// Returns an error when the accept thread panicked.
    pub fn shutdown(mut self) -> AppResult<()> {
        self.stop()
    }

    fn stop(&mut self) -> AppResult<()> {
        self.cancel.store(true, Ordering::Release);
        if let Some(accept) = self.accept.take() {
            accept
                .join()
                .map_err(|_| "the LiveLAN accept thread panicked")?;
        }
        Ok(())
    }
}

impl Drop for LiveLanServer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>, cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, peer)) => {
                if shared.connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
                    shared.connections.fetch_sub(1, Ordering::AcqRel);
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let worker = Arc::clone(shared);
                let spawned = thread::Builder::new()
                    .name("freemixd-live-lan-connection".into())
                    .spawn(move || {
                        serve_connection(stream, peer.ip(), &worker);
                        worker.connections.fetch_sub(1, Ordering::AcqRel);
                    });
                if spawned.is_err() {
                    shared.connections.fetch_sub(1, Ordering::AcqRel);
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => thread::sleep(ACCEPT_POLL),
        }
    }
}

/// One parsed request head.
struct Request {
    method: String,
    path: String,
    cookie: Option<String>,
    /// The whole body, at most [`MAX_BODY_BYTES`].
    body: Vec<u8>,
}

fn serve_connection(mut stream: TcpStream, peer: IpAddr, shared: &Shared) {
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(READ_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err()
    {
        return;
    }
    let received = read_request(&mut stream, Instant::now() + REQUEST_DEADLINE);
    let deadline = Instant::now() + RESPONSE_DEADLINE;
    match received {
        Ok(request) => {
            let local = stream.local_addr().ok().map(|local| local.ip());
            route(&mut stream, (peer, local), shared, &request, deadline);
        }
        Err(status) => {
            let _ = write_simple(&mut stream, status, "", deadline);
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// Reads one bounded read before `deadline`: the request is refused with
/// `408` once the deadline passes, however steadily the bytes trickle in.
fn read_before(
    stream: &mut TcpStream,
    chunk: &mut [u8],
    deadline: Instant,
) -> Result<usize, (u16, &'static str)> {
    const TIMED_OUT: (u16, &str) = (408, "Request Timeout");
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or(TIMED_OUT)?;
    stream
        .set_read_timeout(Some(remaining.min(READ_TIMEOUT)))
        .map_err(|_| (400, "Bad Request"))?;
    match stream.read(chunk) {
        Ok(0) => Err((400, "Bad Request")),
        Ok(read) => Ok(read),
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            Err(if Instant::now() >= deadline {
                TIMED_OUT
            } else {
                (400, "Bad Request")
            })
        }
        Err(_) => Err((400, "Bad Request")),
    }
}

fn read_request(stream: &mut TcpStream, deadline: Instant) -> Result<Request, (u16, &'static str)> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    let head_end = loop {
        if let Some(end) = find_head_end(&buffer) {
            break end;
        }
        if buffer.len() >= MAX_REQUEST_BYTES {
            return Err((431, "Request Header Fields Too Large"));
        }
        let read = read_before(stream, &mut chunk, deadline)?;
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = std::str::from_utf8(&buffer[..head_end]).map_err(|_| (400, "Bad Request"))?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(path), Some(version), None) = (
        request_line.next(),
        request_line.next(),
        request_line.next(),
        request_line.next(),
    ) else {
        return Err((400, "Bad Request"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err((400, "Bad Request"));
    }
    let mut content_length: usize = 0;
    let mut cookie = None;
    let mut host = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err((400, "Bad Request"));
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("host") {
            if host.replace(value).is_some() {
                return Err((400, "Bad Request"));
            }
        } else if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().map_err(|_| (400, "Bad Request"))?;
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err((411, "Length Required"));
        } else if name.eq_ignore_ascii_case("cookie") {
            cookie = session_cookie(value).map(str::to_owned);
        }
    }
    // DNS-rebinding defence: a page on another site that rebinds its own
    // name to this address reaches the listener under that name, so only
    // names a public DNS name cannot be are served.
    if !host.is_some_and(allowed_host) {
        return Err((421, "Misdirected Request"));
    }
    if content_length > MAX_BODY_BYTES {
        return Err((413, "Content Too Large"));
    }
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < content_length {
        let read = read_before(stream, &mut chunk[..MAX_BODY_BYTES], deadline)?;
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    // Strip any query: the routes take none.
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    Ok(Request {
        method: method.to_owned(),
        path: path.to_owned(),
        cookie,
        body,
    })
}

/// Private-use name suffixes no public DNS name ends in: mDNS (`.local`),
/// RFC 8375's home networks, ICANN's private-use `.internal`, the
/// loopback `.localhost`, and the `.lan` home routers hand out.
const PRIVATE_NAME_SUFFIXES: [&str; 5] =
    [".local", ".home.arpa", ".internal", ".localhost", ".lan"];

/// Whether a request's `Host` names this listener the way a viewer on the
/// LAN does: an IP address literal, `localhost`, a single-label name, or a
/// name under a private-use suffix ([`PRIVATE_NAME_SUFFIXES`]), with an
/// optional port. A page on the Internet that rebinds its own (public,
/// dotted) name to a LAN address is refused before any page, session, or
/// media.
fn allowed_host(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 255
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')
        })
    {
        return false;
    }
    let valid_port = |port: &str| {
        !port.is_empty() && port.len() <= 5 && port.bytes().all(|byte| byte.is_ascii_digit())
    };
    if let Some(rest) = value.strip_prefix('[') {
        return rest.split_once(']').is_some_and(|(address, port)| {
            (port.is_empty() || port.strip_prefix(':').is_some_and(valid_port))
                && address.parse::<std::net::Ipv6Addr>().is_ok()
        });
    }
    let name = match value.split_once(':') {
        Some((name, port)) if valid_port(port) => name,
        Some(_) => return false,
        None => value,
    };
    let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    if name.is_empty() || name.starts_with('.') {
        return false;
    }
    name.parse::<std::net::Ipv4Addr>().is_ok()
        || name == "localhost"
        || !name.contains('.')
        || PRIVATE_NAME_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// The `fmlive` session token of a `Cookie` header, if well formed.
fn session_cookie(header: &str) -> Option<&str> {
    header.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == COOKIE_NAME
            && value.len() == SESSION_TOKEN_BYTES * 2
            && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(value)
    })
}

/// Serves `request` from `peer`, which reached the server at the local
/// address beside it.
fn route(
    stream: &mut TcpStream,
    (peer, local): (IpAddr, Option<IpAddr>),
    shared: &Shared,
    request: &Request,
    deadline: Instant,
) {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET" | "HEAD", "/") => {
            let head_only = request.method == "HEAD";
            let head = encode_head(
                200,
                "OK",
                Some("text/html; charset=utf-8"),
                PAGE.len() as u64,
                PAGE_HEADERS,
            );
            if write_all(stream, &head, deadline) && !head_only {
                let _ = write_all(stream, PAGE.as_bytes(), deadline);
            }
        }
        ("POST", "/v1/live/session") => {
            let Some(pin) = form_pin(&request.body) else {
                let _ = write_simple(stream, (400, "Bad Request"), "", deadline);
                return;
            };
            let opened =
                shared
                    .lock()
                    .open_at(peer, local, pin, request.cookie.as_deref(), Instant::now());
            match opened {
                Ok(token) => {
                    let cookie = format!(
                        "Set-Cookie: {COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict\r\n"
                    );
                    let head = encode_head(204, "No Content", None, 0, &cookie);
                    let _ = write_all(stream, &head, deadline);
                }
                Err(refusal) => refuse(stream, refusal, deadline),
            }
        }
        ("GET" | "HEAD", path) if path.starts_with("/live/") => {
            let leaf = &path["/live/".len()..];
            let Some(content_type) = media_content_type(leaf) else {
                let _ = write_simple(stream, (404, "Not Found"), "", deadline);
                return;
            };
            let admitted = shared
                .lock()
                .admit(request.cookie.as_deref(), Instant::now());
            match admitted {
                Ok(directory) => serve_media(
                    stream,
                    shared,
                    &directory.join(leaf),
                    content_type,
                    request.method == "HEAD",
                    deadline,
                ),
                Err(refusal) => refuse(stream, refusal, deadline),
            }
        }
        (_, "/" | "/v1/live/session") => {
            let _ = write_simple(stream, (405, "Method Not Allowed"), "", deadline);
        }
        _ => {
            let _ = write_simple(stream, (404, "Not Found"), "", deadline);
        }
    }
}

/// The PIN of a `pin=<digits>` form body; empty when no PIN was entered.
fn form_pin(body: &[u8]) -> Option<&str> {
    let body = std::str::from_utf8(body).ok()?;
    let pin = body.trim_end_matches(['\r', '\n']).strip_prefix("pin=")?;
    pin.bytes().all(|byte| byte.is_ascii_digit()).then_some(pin)
}

/// The content type of a servable media leaf: the playlist, the
/// initialization segment, or one `segment-<digits>.m4s`.
fn media_content_type(leaf: &str) -> Option<&'static str> {
    if leaf == PLAYLIST_LEAF {
        return Some(PLAYLIST_CONTENT_TYPE);
    }
    if leaf == INIT_LEAF {
        return Some(SEGMENT_CONTENT_TYPE);
    }
    let digits = leaf.strip_prefix("segment-")?.strip_suffix(".m4s")?;
    ((5..=10).contains(&digits.len()) && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some(SEGMENT_CONTENT_TYPE)
}

fn refuse(stream: &mut TcpStream, refusal: Refusal, deadline: Instant) {
    let (status, reason, body) = refusal.status();
    let Refusal::RateLimited(seconds) = refusal else {
        let _ = write_simple(stream, (status, reason), body, deadline);
        return;
    };
    let head = encode_head(
        status,
        reason,
        Some("text/plain; charset=utf-8"),
        body.len() as u64,
        &format!("Retry-After: {seconds}\r\n"),
    );
    let _ = write_all(stream, &head, deadline) && write_all(stream, body.as_bytes(), deadline);
}

fn serve_media(
    stream: &mut TcpStream,
    shared: &Shared,
    path: &Path,
    content_type: &'static str,
    head_only: bool,
    deadline: Instant,
) {
    let Some((mut file, length)) = open_servable_file(path) else {
        // Not published yet, or already rotated out of the playlist.
        let _ = write_simple(stream, (404, "Not Found"), "", deadline);
        return;
    };
    let head = encode_head(200, "OK", Some(content_type), length, "");
    if !write_all(stream, &head, deadline) || head_only {
        return;
    }
    let mut chunk = [0_u8; COPY_CHUNK_BYTES];
    let mut remaining = length;
    while remaining > 0 {
        let want =
            usize::try_from(remaining.min(COPY_CHUNK_BYTES as u64)).unwrap_or(COPY_CHUNK_BYTES);
        match file.read(&mut chunk[..want]) {
            Ok(0) => return,
            Ok(read) => {
                if !write_all(stream, &chunk[..read], deadline) {
                    return;
                }
                let read = u64::try_from(read).unwrap_or(u64::MAX);
                shared.egress_bytes.fetch_add(read, Ordering::Relaxed);
                remaining = remaining.saturating_sub(read);
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

fn open_servable_file(path: &Path) -> Option<(File, u64)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    Some((file, metadata.len()))
}

/// Headers the viewer page carries: it runs only its own inline script and
/// plays only this listener's media, directly or through a Media Source
/// Extensions `blob:` URL of its own.
const PAGE_HEADERS: &str = "Content-Security-Policy: default-src 'none'; script-src \
                            'unsafe-inline'; style-src 'unsafe-inline'; media-src 'self' blob:; \
                            connect-src 'self'; form-action 'none'; frame-ancestors 'none'\r\n\
                            X-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\n";

fn write_simple(
    stream: &mut TcpStream,
    (status, reason): (u16, &'static str),
    body: &'static str,
    deadline: Instant,
) -> bool {
    let head = encode_head(
        status,
        reason,
        (!body.is_empty()).then_some("text/plain; charset=utf-8"),
        body.len() as u64,
        "",
    );
    write_all(stream, &head, deadline) && write_all(stream, body.as_bytes(), deadline)
}

/// Renders one HTTP/1.1 head. Nothing a request carried is ever placed in
/// a response.
fn encode_head(
    status: u16,
    reason: &'static str,
    content_type: Option<&'static str>,
    content_length: u64,
    extra: &str,
) -> Vec<u8> {
    let mut head = String::with_capacity(256 + extra.len());
    let _ = write!(head, "HTTP/1.1 {status} {reason}\r\n");
    if let Some(content_type) = content_type {
        let _ = write!(head, "Content-Type: {content_type}\r\n");
    }
    let _ = write!(
        head,
        "Content-Length: {content_length}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n{extra}\r\n"
    );
    head.into_bytes()
}

fn write_all(stream: &mut TcpStream, bytes: &[u8], deadline: Instant) -> bool {
    let mut offset = 0;
    while offset < bytes.len() {
        if Instant::now() >= deadline {
            return false;
        }
        match stream.write(&bytes[offset..]) {
            Ok(0) => return false,
            Ok(written) => offset += written,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}

/// Compares a secret with a candidate in time that depends only on the
/// candidate's length: no early exit on a differing byte or on a length
/// mismatch. Both are walked, zero-padded, over at least
/// [`PIN_COMPARE_BYTES`] (longer than any valid PIN), and the length
/// difference is folded into the result rather than branched on.
fn constant_time_eq(expected: &[u8], actual: &[u8]) -> bool {
    let walked = actual.len().max(PIN_COMPARE_BYTES);
    let mut difference = expected.len() ^ actual.len();
    for index in 0..walked {
        let expected = expected.get(index).copied().unwrap_or(0);
        let actual = actual.get(index).copied().unwrap_or(0);
        difference |= usize::from(std::hint::black_box(expected ^ actual));
    }
    std::hint::black_box(difference) == 0
}

/// A fresh random session token, or `None` when the system has no
/// randomness to give.
fn session_token() -> Option<String> {
    let mut bytes = [0_u8; SESSION_TOKEN_BYTES];
    getrandom::fill(&mut bytes).ok()?;
    let mut token = String::with_capacity(SESSION_TOKEN_BYTES * 2);
    for byte in bytes {
        let _ = write!(token, "{byte:02x}");
    }
    Some(token)
}

/// The address viewers open: the bound address, or for an unspecified
/// bind (`0.0.0.0` or `::`) this host's primary LAN address, found by
/// routing (not sending) a datagram. Empty when no address is known.
#[must_use]
pub fn viewer_url(bound: SocketAddr) -> String {
    let ip = if bound.ip().is_unspecified() {
        let probe = if bound.is_ipv4() {
            ("0.0.0.0:0", "192.0.2.1:9")
        } else {
            ("[::]:0", "[2001:db8::1]:9")
        };
        let Some(ip) = std::net::UdpSocket::bind(probe.0)
            .and_then(|socket| {
                socket.connect(probe.1)?;
                socket.local_addr()
            })
            .ok()
            .map(|address| address.ip())
            .filter(|ip| !ip.is_unspecified())
        else {
            return String::new();
        };
        ip
    } else {
        bound.ip()
    };
    let url = format!("http://{}/", SocketAddr::new(ip, bound.port()));
    if fm_protocol::live_lan_url_is_valid(&url) {
        url
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(address: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        String::from_utf8_lossy(&response).into_owned()
    }

    fn post_pin(address: SocketAddr, pin: &str) -> String {
        let body = format!("pin={pin}");
        exchange(
            address,
            &format!(
                "POST /v1/live/session HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    }

    fn cookie(response: &str) -> String {
        let start = response.find("fmlive=").unwrap();
        response[start..start + "fmlive=".len() + SESSION_TOKEN_BYTES * 2].to_owned()
    }

    fn get(address: SocketAddr, path: &str, cookie: Option<&str>) -> String {
        let cookie = cookie.map_or_else(String::new, |cookie| format!("Cookie: {cookie}\r\n"));
        exchange(
            address,
            &format!("GET {path} HTTP/1.1\r\nHost: x\r\n{cookie}\r\n"),
        )
    }

    /// A page on the Internet that rebinds its own name to the listener's
    /// LAN address reaches it under that public name: it is refused before
    /// the page, a session, or any media, while every name a viewer on the
    /// LAN uses is served.
    #[test]
    fn only_lan_host_names_are_served() {
        for host in [
            "192.168.1.20:8090",
            "127.0.0.1",
            "[::1]:8090",
            "[fe80::1]",
            "localhost:8090",
            "x",
            "Studio-PC",
            "studio.local",
            "Studio.Local.:8090",
            "studio.lan",
            "studio.home.arpa",
            "studio.internal",
            "app.localhost",
        ] {
            assert!(allowed_host(host), "{host}");
        }
        for host in [
            "",
            "evil.example",
            "evil.example:8090",
            "rebind.evil.example.",
            "127.0.0.1.nip.io",
            "studio.local.evil.example",
            "localhost.evil.example",
            "x:port",
            "x:123456",
            "::1",
            "[::1]x",
            "[evil.example]",
            ".local",
            "a b",
        ] {
            assert!(!allowed_host(host), "{host}");
        }

        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("index.m3u8"), "#EXTM3U\n").unwrap();
        let server = LiveLanServer::start("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = server.address();
        let handle = server.handle();
        // Open to anyone on the LAN: no PIN.
        handle.set_access(Some(LiveLanAccess::new(None, 4)));
        handle.set_media(Some(directory.path().to_path_buf()));
        let request = |head: &str| exchange(address, &format!("{head}\r\n"));
        for refused in [
            format!(
                "GET / HTTP/1.1\r\nHost: rebind.evil.example:{}\r\n",
                address.port()
            ),
            "GET / HTTP/1.1\r\n".to_owned(),
            "GET / HTTP/1.1\r\nHost: x\r\nHost: evil.example\r\n".to_owned(),
            "POST /v1/live/session HTTP/1.1\r\nHost: evil.example\r\nContent-Length: 4\r\n\r\npin="
                .to_owned(),
        ] {
            let response = request(&refused);
            assert!(
                response.starts_with("HTTP/1.1 421") || response.starts_with("HTTP/1.1 400"),
                "{refused:?}: {response}"
            );
            assert!(!response.contains("fmlive="), "{response}");
            assert!(!response.contains("/v1/live/session"), "{response}");
        }
        let rebound = request(&format!(
            "GET / HTTP/1.1\r\nHost: rebind.evil.example:{}\r\n",
            address.port()
        ));
        assert!(
            rebound.starts_with("HTTP/1.1 421 Misdirected Request"),
            "{rebound}"
        );
        // A viewer on the LAN, by address or by name, is served as before.
        for host in [address.to_string(), "studio.local".to_owned()] {
            let page = request(&format!("GET / HTTP/1.1\r\nHost: {host}\r\n"));
            assert!(page.starts_with("HTTP/1.1 200 OK"), "{host}: {page}");
        }
        let joined = post_pin(address, "");
        assert!(joined.starts_with("HTTP/1.1 204"), "{joined}");
        let playlist = get(address, "/live/index.m3u8", Some(&cookie(&joined)));
        assert!(playlist.starts_with("HTTP/1.1 200 OK"), "{playlist}");
    }

    #[test]
    fn sessions_gate_media_by_pin_capacity_and_rate_limit() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("index.m3u8"), "#EXTM3U\n").unwrap();
        fs::write(directory.path().join("init.mp4"), [0x01_u8; 24]).unwrap();
        fs::write(directory.path().join("segment-00000.m4s"), [0x47_u8; 376]).unwrap();
        let server = LiveLanServer::start("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = server.address();
        let handle = server.handle();

        // The page is always served; media and sessions wait for LiveLAN.
        let page = get(address, "/", None);
        assert!(page.starts_with("HTTP/1.1 200 OK"), "{page}");
        assert!(page.contains("Content-Security-Policy"));
        assert!(page.contains("/v1/live/session"));
        // The page may play a Media Source Extensions `blob:` URL, and only that.
        assert!(page.contains("media-src 'self' blob:;"), "{page}");
        assert!(post_pin(address, "").starts_with("HTTP/1.1 503"));

        handle.set_access(Some(LiveLanAccess::new(Some("4321"), 1)));
        assert!(!format!("{:?}", LiveLanAccess::new(Some("4321"), 1)).contains("4321"));
        handle.set_media(Some(directory.path().to_path_buf()));
        assert!(get(address, "/live/index.m3u8", None).starts_with("HTTP/1.1 401"));
        let wrong = post_pin(address, "1111");
        assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");
        assert!(!wrong.contains("4321") && !wrong.contains("1111"));
        let right = post_pin(address, "4321");
        assert!(right.starts_with("HTTP/1.1 204"), "{right}");
        assert!(right.contains("HttpOnly; SameSite=Strict"));
        let session = cookie(&right);
        let playlist = get(address, "/live/index.m3u8", Some(&session));
        assert!(playlist.starts_with("HTTP/1.1 200 OK"), "{playlist}");
        assert!(playlist.contains(PLAYLIST_CONTENT_TYPE) && playlist.ends_with("#EXTM3U\n"));
        // The initialization segment is media like any other: it needs the
        // session, and its bytes count.
        assert!(get(address, "/live/init.mp4", None).starts_with("HTTP/1.1 401"));
        let init = get(address, "/live/init.mp4", Some(&session));
        assert!(init.starts_with("HTTP/1.1 200 OK"), "{init}");
        assert!(init.contains("Content-Type: video/mp4"), "{init}");
        let segment = get(address, "/live/segment-00000.m4s", Some(&session));
        assert!(segment.contains("Content-Type: video/mp4"), "{segment}");
        assert_eq!(handle.viewers(), 1);
        assert_eq!(handle.egress_bytes(), 8 + 24 + 376);
        // Nothing outside the three leaf shapes is served.
        for path in [
            "/live/../index.m3u8",
            "/live/segment-1.m4s",
            "/live/segment-00000.ts",
            "/live/other.m4s",
            "/live/init.mp4/x",
            "/live/init_0.mp4",
            "/index.m3u8",
            "/init.mp4",
        ] {
            assert!(
                get(address, path, Some(&session)).starts_with("HTTP/1.1 404"),
                "{path}"
            );
        }

        // One viewer is the maximum: a second correct PIN is refused.
        let full = post_pin(address, "4321");
        assert!(
            full.starts_with("HTTP/1.1 503") && full.ends_with("full"),
            "{full}"
        );

        // Wrong PINs are rate-limited per address, even a right one after.
        for _ in 1..MAX_PIN_FAILURES {
            assert!(post_pin(address, "0000").starts_with("HTTP/1.1 401"));
        }
        let locked = post_pin(address, "4321");
        assert!(locked.starts_with("HTTP/1.1 429"), "{locked}");
        assert!(locked.contains("Retry-After: 60\r\n"), "{locked}");

        // A new PIN ends every session and forgets the failures.
        handle.set_access(Some(LiveLanAccess::new(Some("8765"), 2)));
        assert!(get(address, "/live/index.m3u8", Some(&session)).starts_with("HTTP/1.1 401"));
        assert_eq!(handle.viewers(), 0);
        let renewed = post_pin(address, "8765");
        assert!(renewed.starts_with("HTTP/1.1 204"), "{renewed}");

        // Off: sessions end and media is refused.
        handle.set_access(None);
        assert!(
            get(address, "/live/index.m3u8", Some(&cookie(&renewed))).starts_with("HTTP/1.1 503")
        );
        server.shutdown().unwrap();
    }

    #[test]
    fn no_pin_opens_a_session_without_one_and_bad_requests_are_refused() {
        let server = LiveLanServer::start("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = server.address();
        server
            .handle()
            .set_access(Some(LiveLanAccess::new(None, 4)));
        let opened = post_pin(address, "");
        assert!(opened.starts_with("HTTP/1.1 204"), "{opened}");
        // Not published yet.
        assert!(
            get(address, "/live/index.m3u8", Some(&cookie(&opened))).starts_with("HTTP/1.1 503")
        );
        assert!(post_pin(address, "12a4").starts_with("HTTP/1.1 400"));
        let oversized = exchange(
            address,
            "POST /v1/live/session HTTP/1.1\r\nHost: x\r\nContent-Length: 65\r\n\r\n",
        );
        assert!(oversized.starts_with("HTTP/1.1 413"), "{oversized}");
        let huge = format!(
            "GET / HTTP/1.1\r\nX: {}\r\n\r\n",
            "a".repeat(MAX_REQUEST_BYTES)
        );
        assert!(exchange(address, &huge).starts_with("HTTP/1.1 431"));
        assert!(
            exchange(address, "DELETE / HTTP/1.1\r\nHost: x\r\n\r\n").starts_with("HTTP/1.1 405")
        );
        assert!(
            exchange(
                address,
                "POST /v1/live/session HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n"
            )
            .starts_with("HTTP/1.1 411")
        );
        server.shutdown().unwrap();
    }

    fn post_pin_with(address: SocketAddr, pin: &str, cookie: &str) -> String {
        let body = format!("pin={pin}");
        exchange(
            address,
            &format!(
                "POST /v1/live/session HTTP/1.1\r\nHost: x\r\nCookie: {cookie}\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    }

    /// A client that trickles its request head a byte at a time, each well
    /// inside the per-read timeout, is cut off at the request deadline
    /// instead of holding a connection for hours.
    #[test]
    fn a_trickled_request_is_cut_off_at_the_request_deadline() {
        let server = LiveLanServer::start("127.0.0.1:0".parse().unwrap()).unwrap();
        let mut stream = TcpStream::connect(server.address()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let started = Instant::now();
        let mut response = Vec::new();
        let mut trickle = "GET / HTTP/1.1\r\n".bytes().chain(std::iter::repeat(b'a'));
        let limit = REQUEST_DEADLINE + READ_TIMEOUT + Duration::from_secs(3);
        while started.elapsed() < limit {
            if let Some(byte) = trickle.next() {
                let _ = stream.write_all(&[byte]);
            }
            let mut chunk = [0_u8; 256];
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => response.extend_from_slice(&chunk[..read]),
                Err(_) => thread::sleep(Duration::from_millis(250)),
            }
        }
        let elapsed = started.elapsed();
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 408"), "{response}");
        assert!(
            elapsed < REQUEST_DEADLINE + READ_TIMEOUT,
            "held for {elapsed:?}"
        );
        server.shutdown().unwrap();
    }

    /// Every venue host shares the server's own `/64` (SLAAC) and
    /// `fe80::/64`: an on-link IPv6 peer's wrong PINs lock out only that
    /// peer, while a remote `/64` stays one allowance.
    #[test]
    fn on_link_ipv6_peers_are_throttled_on_their_own() {
        let now = Instant::now();
        let mut state = AccessState {
            access: Some(LiveLanAccess::new(Some("4321"), 8)),
            ..AccessState::default()
        };
        let lan = |host: u16| IpAddr::from([0x2001, 0xdb8, 0, 7, 0, 0, 0, host]);
        let local = Some(lan(1));
        for _ in 0..MAX_PIN_FAILURES {
            let _ = state.open_at(lan(66), local, "0000", None, now);
        }
        assert_eq!(
            state.open_at(lan(66), local, "4321", None, now),
            Err(Refusal::RateLimited(BASE_LOCKOUT.as_secs()))
        );
        assert!(state.open_at(lan(67), local, "4321", None, now).is_ok());

        let link_local = |host: u16| IpAddr::from([0xfe80, 0, 0, 0, 0, 0, 0, host]);
        for _ in 0..MAX_PIN_FAILURES {
            let _ = state.open_at(link_local(5), local, "0000", None, now);
        }
        assert!(matches!(
            state.open_at(link_local(5), local, "4321", None, now),
            Err(Refusal::RateLimited(_))
        ));
        assert!(
            state
                .open_at(link_local(6), local, "4321", None, now)
                .is_ok()
        );

        // A remote network's hosts still share one allowance.
        let remote = |host: u16| IpAddr::from([0x2001, 0xdb8, 0, 9, 0, 0, 0, host]);
        for host in 1..=u16::from(MAX_PIN_FAILURES) {
            let _ = state.open_at(remote(host), local, "0000", None, now);
        }
        assert!(matches!(
            state.open_at(remote(999), local, "4321", None, now),
            Err(Refusal::RateLimited(_))
        ));
    }

    /// Wrong PINs from one IPv6 `/64` share one allowance, however many
    /// addresses the host uses; IPv4 peers of a dual-stack listener keep
    /// their own.
    #[test]
    fn wrong_pins_are_throttled_per_ipv6_network_and_ipv4_address() {
        let now = Instant::now();
        let mut state = AccessState {
            access: Some(LiveLanAccess::new(Some("4321"), 4)),
            ..AccessState::default()
        };
        let ipv6 = |host: u16| IpAddr::from([0x2001, 0xdb8, 0, 0, 0, 0, 0, host]);
        for host in 1..=u16::from(MAX_PIN_FAILURES) {
            assert_eq!(
                state.open(ipv6(host), "0000", None, now),
                Err(Refusal::Unauthorized)
            );
        }
        assert_eq!(
            state.open(ipv6(1000), "4321", None, now),
            Err(Refusal::RateLimited(BASE_LOCKOUT.as_secs()))
        );
        // Another /64 is unaffected.
        let other = IpAddr::from([0x2001, 0xdb8, 0, 1, 0, 0, 0, 1]);
        assert!(state.open(other, "4321", None, now).is_ok());

        // IPv4-mapped peers count as their IPv4 address, not as one /64.
        let mapped =
            |host: u8| IpAddr::from(std::net::Ipv4Addr::new(192, 0, 2, host).to_ipv6_mapped());
        for _ in 0..MAX_PIN_FAILURES {
            let _ = state.open(mapped(1), "0000", None, now);
        }
        assert_eq!(
            state.open(IpAddr::from([192, 0, 2, 1]), "4321", None, now),
            Err(Refusal::RateLimited(BASE_LOCKOUT.as_secs()))
        );
        assert!(state.open(mapped(2), "4321", None, now).is_ok());
    }

    /// Opening sessions in a flood (no PIN needed) forgets idle sessions, not
    /// the ones watching.
    #[test]
    fn a_flood_of_joins_never_evicts_a_watching_session() {
        let now = Instant::now();
        let mut state = AccessState {
            access: Some(LiveLanAccess::new(None, 4)),
            media: Some(PathBuf::from("media")),
            ..AccessState::default()
        };
        let viewer = state
            .open(IpAddr::from([192, 0, 2, 1]), "", None, now)
            .unwrap();
        assert!(state.admit(Some(&viewer), now).is_ok());
        let later = now + Duration::from_secs(1);
        for _ in 0..MAX_SESSIONS * 2 {
            state
                .open(IpAddr::from([192, 0, 2, 2]), "", None, later)
                .unwrap();
        }
        assert_eq!(state.sessions.len(), MAX_SESSIONS);
        assert!(state.admit(Some(&viewer), later).is_ok());
    }

    /// An encoder that starts again publishes into another directory: every
    /// session served media of the last one ends, so each of those viewers
    /// starts the new stream from its beginning. Publishing the same
    /// directory again keeps them.
    #[test]
    fn publishing_another_directory_ends_the_sessions_it_served() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for directory in [&first, &second] {
            fs::write(directory.path().join("index.m3u8"), "#EXTM3U\n").unwrap();
        }
        let server = LiveLanServer::start("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = server.address();
        let handle = server.handle();
        handle.set_access(Some(LiveLanAccess::new(None, 2)));
        handle.set_media(Some(first.path().to_path_buf()));
        let session = cookie(&post_pin(address, ""));
        assert!(get(address, "/live/index.m3u8", Some(&session)).starts_with("HTTP/1.1 200"));
        handle.set_media(None);
        handle.set_media(Some(first.path().to_path_buf()));
        assert!(get(address, "/live/index.m3u8", Some(&session)).starts_with("HTTP/1.1 200"));
        handle.set_media(None);
        // A session that joins while nothing is published, and asks for
        // media meanwhile, plays the next stream.
        let waiting = cookie(&post_pin(address, ""));
        assert!(get(address, "/live/index.m3u8", Some(&waiting)).starts_with("HTTP/1.1 503"));
        handle.set_media(Some(second.path().to_path_buf()));
        assert!(get(address, "/live/index.m3u8", Some(&session)).starts_with("HTTP/1.1 401"));
        assert!(get(address, "/live/index.m3u8", Some(&waiting)).starts_with("HTTP/1.1 200"));
        let again = cookie(&post_pin(address, ""));
        assert!(get(address, "/live/index.m3u8", Some(&again)).starts_with("HTTP/1.1 200"));
        server.shutdown().unwrap();
    }

    /// A page that joins again (after a playback error, or a reload) while
    /// its own session is a viewer keeps its seat instead of being refused
    /// as full.
    #[test]
    fn joining_again_with_a_live_session_keeps_its_seat() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("index.m3u8"), "#EXTM3U\n").unwrap();
        let server = LiveLanServer::start("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = server.address();
        let handle = server.handle();
        handle.set_access(Some(LiveLanAccess::new(Some("4321"), 1)));
        handle.set_media(Some(directory.path().to_path_buf()));
        let session = cookie(&post_pin(address, "4321"));
        assert!(get(address, "/live/index.m3u8", Some(&session)).starts_with("HTTP/1.1 200"));
        let again = post_pin_with(address, "4321", &session);
        assert!(again.starts_with("HTTP/1.1 204"), "{again}");
        assert_eq!(cookie(&again), session);
        assert!(get(address, "/live/index.m3u8", Some(&session)).starts_with("HTTP/1.1 200"));
        // The PIN is still checked, and another browser is still refused.
        assert!(post_pin_with(address, "1111", &session).starts_with("HTTP/1.1 401"));
        assert!(post_pin(address, "4321").starts_with("HTTP/1.1 503"));
        assert_eq!(handle.viewers(), 1);
        server.shutdown().unwrap();
    }

    #[test]
    fn cookies_forms_leaves_and_urls_are_strict() {
        let token = "0123456789abcdef0123456789abcdef";
        assert_eq!(session_cookie(&format!("a=b; fmlive={token}")), Some(token));
        assert_eq!(session_cookie("fmlive=short"), None);
        assert_eq!(form_pin(b"pin=0042"), Some("0042"));
        assert_eq!(form_pin(b"pin="), Some(""));
        assert_eq!(form_pin(b"pin=4%32"), None);
        assert_eq!(form_pin(b"code=1234"), None);
        assert_eq!(
            media_content_type("segment-00012.m4s"),
            Some(SEGMENT_CONTENT_TYPE)
        );
        assert_eq!(media_content_type("init.mp4"), Some(SEGMENT_CONTENT_TYPE));
        assert_eq!(media_content_type("segment-0012.m4s"), None);
        assert_eq!(media_content_type("segment-00012.ts"), None);
        assert_eq!(media_content_type("segment-00012.m4s.part"), None);
        assert_eq!(media_content_type("Init.mp4"), None);
        assert_eq!(
            media_content_type("index.m3u8"),
            Some(PLAYLIST_CONTENT_TYPE)
        );
        assert_eq!(
            viewer_url("192.168.1.20:8090".parse().unwrap()),
            "http://192.168.1.20:8090/"
        );
        assert_eq!(
            viewer_url("[fd00::1]:8090".parse().unwrap()),
            "http://[fd00::1]:8090/"
        );
    }

    /// Each run of wrong PINs locks an address out for twice as long as the
    /// run before, up to the cap; a quiet address is forgotten in time; a
    /// right PIN is refused while locked out.
    #[test]
    fn wrong_pin_lockouts_escalate_and_are_capped() {
        let start = Instant::now();
        let mut state = AccessState {
            access: Some(LiveLanAccess::new(Some("4321"), 4)),
            ..AccessState::default()
        };
        let peer = IpAddr::from([192, 0, 2, 7]);
        let mut now = start;
        let mut expected = BASE_LOCKOUT;
        for run in 0..10 {
            for attempt in 0..MAX_PIN_FAILURES {
                assert_eq!(
                    state.open(peer, "0000", None, now),
                    Err(Refusal::Unauthorized),
                    "run {run}, attempt {attempt}"
                );
            }
            assert_eq!(
                state.open(peer, "4321", None, now),
                Err(Refusal::RateLimited(expected.as_secs())),
                "run {run}"
            );
            // Still locked just before the end, free at the end.
            let almost = (now + expected)
                .checked_sub(Duration::from_millis(500))
                .unwrap();
            assert_eq!(
                state.open(peer, "4321", None, almost),
                Err(Refusal::RateLimited(1)),
                "run {run}"
            );
            now += expected;
            expected = (expected * 2).min(MAX_LOCKOUT);
        }
        assert_eq!(expected, MAX_LOCKOUT);
        assert_eq!(lockout(0), BASE_LOCKOUT);
        assert_eq!(lockout(1), BASE_LOCKOUT * 2);
        assert_eq!(lockout(40), MAX_LOCKOUT);
        assert_eq!(lockout(u32::MAX), MAX_LOCKOUT);
        // Free again after the last lockout: a right PIN opens a session.
        assert!(state.open(peer, "4321", None, now).is_ok());
        // Fewer than a run of wrong PINs locks nothing, and a run spread
        // wider than the failure window never adds up.
        for _ in 1..MAX_PIN_FAILURES {
            let _ = state.open(peer, "0000", None, now);
            now += FAILURE_WINDOW;
        }
        assert!(state.open(peer, "4321", None, now).is_ok());
        // After a long quiet spell the address starts afresh at the base.
        now += LOCKOUT_MEMORY + FAILURE_WINDOW;
        state.prune(now);
        assert!(state.failures.is_empty());
        for _ in 0..MAX_PIN_FAILURES {
            let _ = state.open(peer, "0000", None, now);
        }
        assert_eq!(
            state.open(peer, "4321", None, now),
            Err(Refusal::RateLimited(BASE_LOCKOUT.as_secs()))
        );
        // A lockout keys on the IPv6 /64 as before: another host of the
        // network is locked out too, another network is not.
        let ipv6 =
            |network: u16, host: u16| IpAddr::from([0x2001, 0xdb8, 0, network, 0, 0, 0, host]);
        for host in 0..u16::from(MAX_PIN_FAILURES) {
            let _ = state.open(ipv6(0, host), "0000", None, now);
        }
        assert!(matches!(
            state.open(ipv6(0, 999), "4321", None, now),
            Err(Refusal::RateLimited(_))
        ));
        assert!(state.open(ipv6(1, 1), "4321", None, now).is_ok());
    }

    /// The comparison is right whatever the lengths, and a candidate
    /// shorter or longer than the PIN is refused like any other.
    #[test]
    fn pin_comparison_folds_in_the_length() {
        assert!(constant_time_eq(b"4321", b"4321"));
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"4321", b"432"));
        assert!(!constant_time_eq(b"4321", b"43210"));
        // Zero padding never makes a prefix match.
        assert!(!constant_time_eq(b"4321", b"4321\0"));
        assert!(!constant_time_eq(b"4321\0", b"4321"));
        assert!(!constant_time_eq(b"4321", b""));
        assert!(!constant_time_eq(b"", b"0"));
        let long = [b'7'; PIN_COMPARE_BYTES + 8];
        assert!(constant_time_eq(&long, &long));
        assert!(!constant_time_eq(b"7777", &long));
    }

    /// The page plays HLS natively where the browser can, and otherwise
    /// through its own Media Source Extensions player that fetches the
    /// playlist, the initialization segment, and the media segments with
    /// the session cookie.
    #[test]
    fn the_page_carries_a_native_and_an_mse_player() {
        for needle in [
            "application/vnd.apple.mpegurl",
            "MediaSource",
            "ManagedMediaSource",
            "addSourceBuffer",
            "EXT-X-MAP",
            "EXT-X-MEDIA-SEQUENCE",
            "avcC",
            "credentials: \"same-origin\"",
            "/live/index.m3u8",
            "Retry-After",
        ] {
            assert!(PAGE.contains(needle), "the page lacks {needle}");
        }
        // Nothing outside this listener is loaded.
        for absent in ["http://", "https://", "<script src", "<link"] {
            assert!(!PAGE.contains(absent), "the page carries {absent}");
        }
    }
}
