//! Bounded loopback HLS listener for local `hls://` stream destinations.
//!
//! [`HlsServer`] is a read-only view of the directories native `freemixd`
//! publishes into, where a stream target writes `index.m3u8` next to
//! `segment-%05d.ts`, or, for a multi-bitrate ladder, `<rung>/index.m3u8` and
//! `<rung>/segment-%05d.ts` under pre-created rung directories `0..=7`. It owns
//! a fixed set of threads, never touches engine, control, or render state, and
//! every bound is fixed at compile time: at most [`PENDING_CAPACITY`] sockets
//! held by the accept thread, [`CONNECTION_WORKERS`] request workers plus one
//! queued socket each, [`MAX_REQUEST_BYTES`] of request head, and a served file
//! no larger than [`MAX_HLS_FILE_BYTES`].
//!
//! The accept thread only reads request heads from non-blocking sockets and
//! writes the small authentication and routing replies itself. A file body is
//! handed to a request worker, so a slow reader can never stall the accept
//! thread, and a connection that sends nothing holds a pending slot for at most
//! [`FIRST_BYTE_TIMEOUT`]. A request path is never composed from unvalidated
//! text: [`HlsRoots::resolve`] accepts exactly the four shapes a local HLS
//! destination publishes, so no request can escape its root.

use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fmt::{self, Write as _},
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::AppResult;

/// Environment variable holding the bearer token that guards the listener.
pub const HLS_TOKEN_ENVIRONMENT: &str = "FREEMIXD_HLS_TOKEN";
/// Smallest accepted token length in bytes.
pub const HLS_TOKEN_MIN_BYTES: usize = 32;
/// Largest accepted token length in bytes.
pub const HLS_TOKEN_MAX_BYTES: usize = 256;
/// Largest file the listener will serve. A larger file answers 404.
pub const MAX_HLS_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Request-head cap. A head that reaches this size without a terminator, or
/// that carries more headers than [`MAX_REQUEST_HEADERS`], answers 431.
pub const MAX_REQUEST_BYTES: usize = 4 * 1024;
/// A connection that sends nothing is closed this fast so it cannot crowd the
/// pending set.
pub const FIRST_BYTE_TIMEOUT: Duration = Duration::from_millis(50);
/// Time allowed to finish a head once its first byte has arrived.
pub const HEAD_DEADLINE: Duration = Duration::from_millis(250);
/// Time allowed to push a reply the accept thread writes itself.
pub const REPLY_DEADLINE: Duration = Duration::from_millis(250);

/// Request workers. One queued socket each caps concurrent file bodies at four.
const CONNECTION_WORKERS: usize = 2;
const WORKER_QUEUE_CAPACITY: usize = 1;
/// Sockets the accept thread reads concurrently.
const PENDING_CAPACITY: usize = 64;
const MAX_REQUEST_HEADERS: usize = 32;
const MAX_DRAIN_BYTES: usize = 16 * 1024;
const COPY_CHUNK_BYTES: usize = 16 * 1024;

/// Accept-thread sleep while it holds no socket.
const ACCEPT_POLL: Duration = Duration::from_millis(5);
/// Accept-thread sleep while sockets are mid-head or mid-reply.
const PENDING_POLL: Duration = Duration::from_millis(1);
const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_millis(250);
/// Time allowed to push a reply a request worker writes, head and body.
const RESPONSE_DEADLINE: Duration = Duration::from_millis(500);

const HLS_PREFIX: &str = "/v1/hls/";
const PLAYLIST_LEAF: &str = "index.m3u8";
const SEGMENT_PREFIX: &str = "segment-";
const SEGMENT_SUFFIX: &str = ".ts";
const SEGMENT_DIGITS: usize = 5;
const MAX_RUNG: u8 = b'7';
const PLAYLIST_CONTENT_TYPE: &str = "application/vnd.apple.mpegurl";
const SEGMENT_CONTENT_TYPE: &str = "video/mp2t";

/// Whether `token` is a usable bearer token: [`HLS_TOKEN_MIN_BYTES`] through
/// [`HLS_TOKEN_MAX_BYTES`] ASCII graphic bytes.
#[must_use]
pub fn valid_token(token: &[u8]) -> bool {
    (HLS_TOKEN_MIN_BYTES..=HLS_TOKEN_MAX_BYTES).contains(&token.len())
        && token.iter().all(u8::is_ascii_graphic)
}

/// Reads the listener token from [`HLS_TOKEN_ENVIRONMENT`].
///
/// # Errors
///
/// Returns a sanitized error naming the environment variable only when it is
/// absent, shorter than [`HLS_TOKEN_MIN_BYTES`], longer than
/// [`HLS_TOKEN_MAX_BYTES`], or not entirely ASCII graphic bytes.
pub fn token_from_environment() -> AppResult<Vec<u8>> {
    let Ok(value) = env::var(HLS_TOKEN_ENVIRONMENT) else {
        return Err(
            format!("{HLS_TOKEN_ENVIRONMENT} is required when --hls-listen is enabled").into(),
        );
    };
    let bytes = value.into_bytes();
    if !valid_token(&bytes) {
        return Err(format!(
            "{HLS_TOKEN_ENVIRONMENT} must be {HLS_TOKEN_MIN_BYTES}..={HLS_TOKEN_MAX_BYTES} ASCII graphic bytes"
        )
        .into());
    }
    Ok(bytes)
}

/// Why a target or relative path is not served.
///
/// Neither variant ever carries path or target text, so no error can render the
/// operator's directory or an unknown request path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HlsRouteError {
    /// The target id is unknown, or the relative path is not one of the four
    /// shapes a local HLS destination publishes.
    NotFound,
    /// Two entries of [`HlsRoots::try_from_targets`] named the same target id.
    DuplicateTarget,
}

impl fmt::Display for HlsRouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("hls route not found"),
            Self::DuplicateTarget => formatter.write_str("duplicate hls stream target"),
        }
    }
}

impl Error for HlsRouteError {}

/// The validated mapping from engine stream target ids to absolute local HLS
/// output directories.
pub struct HlsRoots {
    roots: BTreeMap<u128, PathBuf>,
}

impl HlsRoots {
    /// Builds the route table, refusing duplicate target ids instead of
    /// letting the last entry silently win.
    ///
    /// # Errors
    ///
    /// Returns [`HlsRouteError::DuplicateTarget`] when two entries name the same
    /// target id. The error carries no path text.
    pub fn try_from_targets(
        targets: impl IntoIterator<Item = (u128, PathBuf)>,
    ) -> Result<Self, HlsRouteError> {
        let mut roots = BTreeMap::new();
        for (target, root) in targets {
            if roots.insert(target, root).is_some() {
                return Err(HlsRouteError::DuplicateTarget);
            }
        }
        Ok(Self { roots })
    }

    /// Whether the table names no target at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Resolves one request's target and relative shape to a rooted path.
    ///
    /// A relative path is accepted only in exactly these four shapes:
    ///
    /// - `index.m3u8`
    /// - `segment-` followed by exactly five ASCII digits followed by `.ts`
    /// - `<rung>/index.m3u8` with `<rung>` a single ASCII digit `0..=7`
    /// - `<rung>/segment-` followed by exactly five ASCII digits followed by
    ///   `.ts` with `<rung>` a single ASCII digit `0..=7`
    ///
    /// Anything else is refused: empty segments, `.`, `..`, absolute paths,
    /// backslashes, extra slashes, query or fragment characters, longer digit
    /// runs, other extensions, and rungs outside `0..=7`. The returned path is
    /// the target's root joined with the validated relative shape, so no input
    /// can name a path outside that root.
    ///
    /// # Errors
    ///
    /// Returns [`HlsRouteError::NotFound`] for an unknown target id or any
    /// relative shape other than the four above. The error carries no path
    /// text.
    pub fn resolve(&self, target: u128, relative: &str) -> Result<PathBuf, HlsRouteError> {
        let Some(root) = self.roots.get(&target) else {
            return Err(HlsRouteError::NotFound);
        };
        if !valid_relative(relative) {
            return Err(HlsRouteError::NotFound);
        }
        Ok(root.join(relative))
    }
}

/// Whether a relative request path is one of the four published shapes.
fn valid_relative(relative: &str) -> bool {
    match relative.split_once('/') {
        None => valid_leaf(relative),
        Some((rung, leaf)) => valid_rung(rung) && valid_leaf(leaf),
    }
}

/// Whether `rung` is a single ASCII digit in `0..=7`.
fn valid_rung(rung: &str) -> bool {
    let mut bytes = rung.bytes();
    matches!(
        (bytes.next(), bytes.next()),
        (Some(byte), None) if byte.is_ascii_digit() && byte <= MAX_RUNG
    )
}

/// Whether `leaf` is a playlist name or one five-digit segment name.
fn valid_leaf(leaf: &str) -> bool {
    if leaf == PLAYLIST_LEAF {
        return true;
    }
    let Some(digits) = leaf
        .strip_prefix(SEGMENT_PREFIX)
        .and_then(|rest| rest.strip_suffix(SEGMENT_SUFFIX))
    else {
        return false;
    };
    digits.len() == SEGMENT_DIGITS && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// A bounded HTTP/1.1 listener serving the local `hls://` destinations.
pub struct HlsServer {
    address: SocketAddr,
    cancel: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl HlsServer {
    /// Binds the loopback listener and starts its fixed thread set.
    ///
    /// `shutdown` is the cooperative stop flag shared with the caller; the
    /// listener stops reading heads as soon as it is set. Only callers on the
    /// same host can reach the bound socket, and every request must present
    /// `Authorization: Bearer <token>` with `token` byte-for-byte.
    ///
    /// # Errors
    ///
    /// Fails when the address is not loopback, the token is not
    /// [`HLS_TOKEN_MIN_BYTES`] through [`HLS_TOKEN_MAX_BYTES`] ASCII graphic
    /// bytes, the socket cannot be bound, or a listener thread cannot be
    /// spawned. Threads already started are stopped before returning.
    pub fn start(
        address: SocketAddr,
        token: Vec<u8>,
        roots: HlsRoots,
        shutdown: Arc<AtomicBool>,
    ) -> AppResult<Self> {
        if !address.ip().is_loopback() {
            return Err("--hls-listen must use a loopback address".into());
        }
        if !valid_token(&token) {
            return Err(format!(
                "{HLS_TOKEN_ENVIRONMENT} must be {HLS_TOKEN_MIN_BYTES}..={HLS_TOKEN_MAX_BYTES} ASCII graphic bytes"
            )
            .into());
        }
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let bound = listener.local_addr()?;

        let cancel = shutdown;
        let mut senders = Vec::with_capacity(CONNECTION_WORKERS);
        let mut threads = Vec::with_capacity(CONNECTION_WORKERS + 1);
        for index in 0..CONNECTION_WORKERS {
            let (sender, receiver) = mpsc::sync_channel(WORKER_QUEUE_CAPACITY);
            let worker_cancel = Arc::clone(&cancel);
            match thread::Builder::new()
                .name(format!("freemixd-hls-{index}"))
                .spawn(move || worker_loop(&receiver, &worker_cancel))
            {
                Ok(handle) => {
                    senders.push(sender);
                    threads.push(handle);
                }
                Err(error) => return Err(stop_threads(&cancel, threads, error)),
            }
        }

        let accept_cancel = Arc::clone(&cancel);
        match thread::Builder::new()
            .name("freemixd-hls-accept".into())
            .spawn(move || {
                accept_loop(&listener, senders, &accept_cancel, &token, &roots);
            }) {
            Ok(handle) => threads.insert(0, handle),
            Err(error) => return Err(stop_threads(&cancel, threads, error)),
        }
        Ok(Self {
            address: bound,
            cancel,
            threads,
        })
    }

    /// The address the listener actually bound.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Stops accepting and joins every listener thread.
    ///
    /// The join is bounded by the accept poll plus, for a request worker,
    /// [`RESPONSE_DEADLINE`] and one socket write timeout — roughly 800 ms worst
    /// case, and a few milliseconds in practice.
    ///
    /// # Errors
    ///
    /// Returns an error if a listener thread panicked.
    pub fn shutdown(mut self) -> AppResult<()> {
        if self.stop() {
            return Err("freemixd HLS listener thread panicked".into());
        }
        Ok(())
    }

    fn stop(&mut self) -> bool {
        self.cancel.store(true, Ordering::Release);
        let mut panicked = false;
        for handle in std::mem::take(&mut self.threads) {
            panicked |= handle.join().is_err();
        }
        panicked
    }
}

impl Drop for HlsServer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn stop_threads(
    cancel: &AtomicBool,
    threads: Vec<JoinHandle<()>>,
    error: std::io::Error,
) -> Box<dyn Error> {
    cancel.store(true, Ordering::Release);
    for handle in threads {
        let _ = handle.join();
    }
    error.into()
}

/// A socket the accept thread owns while it reads a head or writes a reply.
struct Pending {
    stream: TcpStream,
    head: Vec<u8>,
    /// First-byte deadline until the peer speaks, then the head or reply
    /// deadline.
    deadline: Instant,
    started: bool,
    reply: Option<Outgoing>,
}

impl Pending {
    fn reading(stream: TcpStream, now: Instant) -> Self {
        Self {
            stream,
            head: Vec::new(),
            deadline: now + FIRST_BYTE_TIMEOUT,
            started: false,
            reply: None,
        }
    }

    fn replying(stream: TcpStream, response: &Response, now: Instant) -> Self {
        Self {
            stream,
            head: Vec::new(),
            deadline: now + REPLY_DEADLINE,
            started: true,
            reply: Some(Outgoing::new(response)),
        }
    }

    fn answer(&mut self, response: &Response, now: Instant) {
        self.reply = Some(Outgoing::new(response));
        self.deadline = now + REPLY_DEADLINE;
    }
}

struct Outgoing {
    bytes: Vec<u8>,
    offset: usize,
}

impl Outgoing {
    fn new(response: &Response) -> Self {
        Self {
            bytes: encode_response(response),
            offset: 0,
        }
    }
}

/// One authorized, routed file request handed to a request worker.
struct HlsJob {
    stream: TcpStream,
    path: PathBuf,
    content_type: &'static str,
    head_only: bool,
}

/// What the accept thread should do with a pending socket after one step.
enum Step {
    Hold,
    Close,
    Job {
        path: PathBuf,
        content_type: &'static str,
        head_only: bool,
    },
}

fn accept_loop(
    listener: &TcpListener,
    mut senders: Vec<SyncSender<HlsJob>>,
    cancel: &AtomicBool,
    token: &[u8],
    roots: &HlsRoots,
) {
    let mut pending: Vec<Pending> = Vec::with_capacity(PENDING_CAPACITY);
    while !cancel.load(Ordering::Acquire) {
        let listening = accept_available(listener, &mut pending);
        let progressed = advance_pending(&mut pending, &mut senders, token, roots);
        if !listening {
            break;
        }
        if pending.is_empty() {
            thread::sleep(ACCEPT_POLL);
        } else if !progressed {
            thread::sleep(PENDING_POLL);
        }
    }
    for connection in pending {
        close_pending(connection);
    }
}

/// Fills the pending set from the accept queue. Returns false on a fatal error.
///
/// Nothing is admitted past [`PENDING_CAPACITY`]; the surplus waits in the
/// kernel backlog, where it costs the listener nothing, until a slot frees.
fn accept_available(listener: &TcpListener, pending: &mut Vec<Pending>) -> bool {
    while pending.len() < PENDING_CAPACITY {
        match listener.accept() {
            Ok((stream, _)) => {
                if stream.set_nonblocking(true).is_err() {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                pending.push(Pending::reading(stream, Instant::now()));
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) =>
            {
                return true;
            }
            Err(_) => return false,
        }
    }
    true
}

/// Advances every pending socket by one non-blocking step.
fn advance_pending(
    pending: &mut Vec<Pending>,
    senders: &mut [SyncSender<HlsJob>],
    token: &[u8],
    roots: &HlsRoots,
) -> bool {
    let now = Instant::now();
    let mut progressed = false;
    let mut index = 0;
    while index < pending.len() {
        match advance_one(&mut pending[index], token, roots, now, &mut progressed) {
            Step::Hold => index += 1,
            Step::Close => close_pending(pending.swap_remove(index)),
            Step::Job {
                path,
                content_type,
                head_only,
            } => {
                let Pending { stream, .. } = pending.swap_remove(index);
                let job = HlsJob {
                    stream,
                    path,
                    content_type,
                    head_only,
                };
                if let Err(stream) = dispatch_job(job, senders) {
                    pending.push(Pending::replying(stream, &Response::busy(), now));
                }
                progressed = true;
            }
        }
    }
    progressed
}

fn advance_one(
    connection: &mut Pending,
    token: &[u8],
    roots: &HlsRoots,
    now: Instant,
    progressed: &mut bool,
) -> Step {
    if let Some(outgoing) = connection.reply.as_mut() {
        return match push_reply(&mut connection.stream, outgoing) {
            Progress::Done => {
                *progressed = true;
                Step::Close
            }
            Progress::Blocked if now < connection.deadline => Step::Hold,
            Progress::Blocked | Progress::Failed => Step::Close,
        };
    }
    let mut chunk = [0_u8; 1024];
    match connection.stream.read(&mut chunk) {
        Ok(0) => Step::Close,
        Ok(read) => {
            *progressed = true;
            if !connection.started {
                connection.started = true;
                connection.deadline = now + HEAD_DEADLINE;
            }
            let room = MAX_REQUEST_BYTES.saturating_sub(connection.head.len());
            connection.head.extend_from_slice(&chunk[..read.min(room)]);
            classify(connection, token, roots, now)
        }
        Err(error) if error.kind() == ErrorKind::Interrupted => Step::Hold,
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            if now >= connection.deadline {
                connection.answer(&Response::request_timeout(), now);
            }
            Step::Hold
        }
        Err(_) => Step::Close,
    }
}

/// Routes a head as soon as it is complete, or rejects one that never ends.
fn classify(connection: &mut Pending, token: &[u8], roots: &HlsRoots, now: Instant) -> Step {
    if let Some(end) = connection
        .head
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
    {
        connection.head.truncate(end);
        match route(&connection.head, token, roots) {
            Routed::Reply(response) => {
                connection.answer(&response, now);
                Step::Hold
            }
            Routed::Job {
                path,
                content_type,
                head_only,
            } => Step::Job {
                path,
                content_type,
                head_only,
            },
        }
    } else if connection.head.len() >= MAX_REQUEST_BYTES {
        connection.answer(&Response::headers_too_large(), now);
        Step::Hold
    } else {
        Step::Hold
    }
}

/// Hands an authorized routed request to a file worker.
///
/// A disconnected worker means that thread panicked. The connection is carried
/// on to the next worker instead of being dropped into a dead channel.
fn dispatch_job(job: HlsJob, senders: &mut [SyncSender<HlsJob>]) -> Result<(), TcpStream> {
    let mut job = job;
    let mut index = 0;
    while index < senders.len() {
        match senders[index].try_send(job) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned) | TrySendError::Disconnected(returned)) => {
                job = returned;
                index += 1;
            }
        }
    }
    Err(job.stream)
}

enum Progress {
    Done,
    Blocked,
    Failed,
}

fn push_reply(stream: &mut TcpStream, outgoing: &mut Outgoing) -> Progress {
    while outgoing.offset < outgoing.bytes.len() {
        match stream.write(&outgoing.bytes[outgoing.offset..]) {
            Ok(0) => return Progress::Failed,
            Ok(written) => outgoing.offset += written,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Progress::Blocked;
            }
            Err(_) => return Progress::Failed,
        }
    }
    Progress::Done
}

fn close_pending(mut connection: Pending) {
    drain_buffered(&mut connection.stream);
    let _ = connection.stream.shutdown(Shutdown::Both);
}

/// Consumes what the peer already sent so the close is a FIN, not a reset.
/// The socket is non-blocking here, so this cannot wait on anything.
fn drain_buffered(stream: &mut TcpStream) {
    let mut chunk = [0_u8; 1024];
    let mut drained = 0;
    while drained < MAX_DRAIN_BYTES {
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => drained += read,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

fn worker_loop(jobs: &Receiver<HlsJob>, cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        match jobs.recv_timeout(ACCEPT_POLL) {
            Ok(job) => serve_file(job),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Serves one routed file request, answering 404 for anything that is not a
/// regular file within [`MAX_HLS_FILE_BYTES`].
fn serve_file(job: HlsJob) {
    let HlsJob {
        mut stream,
        path,
        content_type,
        head_only,
    } = job;
    if prepare(&mut stream).is_ok() {
        match open_servable_file(&path) {
            Some((mut file, length)) => {
                serve_opened_file(&mut stream, &mut file, length, content_type, head_only);
            }
            None => write_response(&mut stream, &Response::not_found()),
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// Opens `path` only when it is a regular file within the size cap.
///
/// The symlink check is a `symlink_metadata` lookup before the open, so a
/// symlink, directory, fifo, socket, or oversized file never reaches a reader.
/// The opened handle's own metadata supplies the served length.
fn open_servable_file(path: &Path) -> Option<(File, u64)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_HLS_FILE_BYTES {
        return None;
    }
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_HLS_FILE_BYTES {
        return None;
    }
    Some((file, metadata.len()))
}

/// Writes a `200` head and, unless this is a `HEAD` request, streams the body.
///
/// The deadline is checked between writes and between file reads, so the true
/// bound is [`RESPONSE_DEADLINE`] plus one [`SOCKET_WRITE_TIMEOUT`].
fn serve_opened_file(
    stream: &mut TcpStream,
    file: &mut File,
    length: u64,
    content_type: &'static str,
    head_only: bool,
) {
    let deadline = Instant::now() + RESPONSE_DEADLINE;
    let head = encode_head(200, "OK", Some(content_type), length);
    if !write_all(stream, &head, deadline) {
        return;
    }
    if head_only {
        return;
    }
    let mut chunk = [0_u8; COPY_CHUNK_BYTES];
    let mut remaining = length;
    while remaining > 0 {
        if Instant::now() >= deadline {
            return;
        }
        let want =
            usize::try_from(remaining.min(COPY_CHUNK_BYTES as u64)).unwrap_or(COPY_CHUNK_BYTES);
        match file.read(&mut chunk[..want]) {
            Ok(0) => return,
            Ok(read) => {
                if !write_all(stream, &chunk[..read], deadline) {
                    return;
                }
                remaining = remaining.saturating_sub(u64::try_from(read).unwrap_or(u64::MAX));
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

fn prepare(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(SOCKET_WRITE_TIMEOUT))
}

fn write_response(stream: &mut TcpStream, response: &Response) {
    let bytes = encode_response(response);
    let _ = write_all(stream, &bytes, Instant::now() + REPLY_DEADLINE);
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

/// Whether the request presents exactly `token` under the `Bearer` scheme.
///
/// The scheme is matched case-insensitively like RFC 7235 requires, while the
/// token itself is compared byte-for-byte.
fn token_matches(token: &[u8], authorization: Option<&[u8]>) -> bool {
    let Some(authorization) = authorization else {
        return false;
    };
    constant_time_eq(bearer_credentials(authorization), token)
}

/// Extracts the credentials of a `Bearer` challenge.
///
/// RFC 7235 makes the scheme case-insensitive and allows more than one space
/// before the credentials; the credentials themselves stay exact.
fn bearer_credentials(authorization: &[u8]) -> &[u8] {
    let Some(space) = authorization.iter().position(|byte| *byte == b' ') else {
        return &[];
    };
    let (scheme, rest) = authorization.split_at(space);
    if !scheme.eq_ignore_ascii_case(b"bearer") {
        return &[];
    }
    let start = rest
        .iter()
        .position(|byte| *byte != b' ')
        .unwrap_or(rest.len());
    &rest[start..]
}

/// Compares two secrets without an early exit on the first differing byte.
fn constant_time_eq(expected: &[u8], actual: &[u8]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (expected, actual) in expected.iter().zip(actual) {
        difference |= expected ^ actual;
    }
    difference == 0
}

struct RequestHead<'a> {
    method: &'a str,
    path: &'a str,
    authorization: Option<&'a [u8]>,
}

impl<'a> RequestHead<'a> {
    fn parse(head: &'a [u8]) -> Result<Self, Response> {
        let Ok(text) = str::from_utf8(head) else {
            return Err(Response::bad_request());
        };
        let mut lines = text.split("\r\n");
        let Some(request_line) = lines.next() else {
            return Err(Response::bad_request());
        };
        let mut parts = request_line.split(' ');
        let (Some(method), Some(target), Some(version), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(Response::bad_request());
        };
        if !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
            return Err(Response::bad_request());
        }

        let mut authorization = None;
        let mut headers = 0_usize;
        for line in lines.filter(|line| !line.is_empty()) {
            headers += 1;
            if headers > MAX_REQUEST_HEADERS {
                return Err(Response::headers_too_large());
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(Response::bad_request());
            };
            if name.eq_ignore_ascii_case("authorization") {
                if authorization.is_some() {
                    return Err(Response::bad_request());
                }
                authorization = Some(value.trim().as_bytes());
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                || (name.eq_ignore_ascii_case("content-length") && value.trim() != "0")
            {
                // The listener never reads a request body.
                return Err(Response::bad_request());
            }
        }
        Ok(Self {
            method,
            path: target,
            authorization,
        })
    }
}

/// Either a reply the accept thread can write itself, or a file a worker owns.
enum Routed {
    Reply(Response),
    Job {
        path: PathBuf,
        content_type: &'static str,
        head_only: bool,
    },
}

fn route(head: &[u8], token: &[u8], roots: &HlsRoots) -> Routed {
    let request = match RequestHead::parse(head) {
        Ok(request) => request,
        Err(response) => return Routed::Reply(response),
    };
    // Authentication is evaluated before the route so an unauthenticated
    // caller learns nothing from the shape of the rejection.
    if !token_matches(token, request.authorization) {
        return Routed::Reply(Response::unauthorized());
    }
    let Some((target, relative)) = hls_route(request.path) else {
        return Routed::Reply(Response::not_found());
    };
    if !matches!(request.method, "GET" | "HEAD") {
        return Routed::Reply(Response::method_not_allowed());
    }
    let Ok(path) = roots.resolve(target, relative) else {
        return Routed::Reply(Response::not_found());
    };
    // `resolve` accepted the relative shape, so its leaf is exactly one of the
    // two published names.
    let leaf = relative.rsplit_once('/').map_or(relative, |(_, leaf)| leaf);
    let content_type = if leaf == PLAYLIST_LEAF {
        PLAYLIST_CONTENT_TYPE
    } else {
        SEGMENT_CONTENT_TYPE
    };
    Routed::Job {
        path,
        content_type,
        head_only: request.method == "HEAD",
    }
}

/// Splits `/v1/hls/<target>/<relative>` into its target id and relative path.
///
/// The target id must be the canonical decimal form of a nonzero `u128`, so
/// unknown ids, signed forms, and leading zeros all fall through to 404.
fn hls_route(path: &str) -> Option<(u128, &str)> {
    let rest = path.strip_prefix(HLS_PREFIX)?;
    let (target_text, relative) = rest.split_once('/')?;
    let target = target_text.parse::<u128>().ok()?;
    (target != 0 && target.to_string() == target_text).then_some((target, relative))
}

fn encode_response(response: &Response) -> Vec<u8> {
    encode_head(
        response.status,
        response.reason,
        response.content_type,
        response.content_length,
    )
}

/// Renders one bounded HTTP/1.1 head. Nothing derived from a path, a target
/// directory, a host, or a token is ever placed into a response.
fn encode_head(
    status: u16,
    reason: &'static str,
    content_type: Option<&'static str>,
    content_length: u64,
) -> Vec<u8> {
    let mut head = String::with_capacity(192);
    let _ = write!(head, "HTTP/1.1 {status} {reason}\r\n");
    if let Some(content_type) = content_type {
        let _ = write!(head, "Content-Type: {content_type}\r\n");
    }
    let _ = write!(
        head,
        "Content-Length: {content_length}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    head.into_bytes()
}

struct Response {
    status: u16,
    reason: &'static str,
    content_type: Option<&'static str>,
    content_length: u64,
}

impl Response {
    fn unauthorized() -> Self {
        Self {
            status: 401,
            reason: "Unauthorized",
            content_type: None,
            content_length: 0,
        }
    }

    fn not_found() -> Self {
        Self {
            status: 404,
            reason: "Not Found",
            content_type: None,
            content_length: 0,
        }
    }

    fn method_not_allowed() -> Self {
        Self {
            status: 405,
            reason: "Method Not Allowed",
            content_type: None,
            content_length: 0,
        }
    }

    fn bad_request() -> Self {
        Self {
            status: 400,
            reason: "Bad Request",
            content_type: None,
            content_length: 0,
        }
    }

    fn request_timeout() -> Self {
        Self {
            status: 408,
            reason: "Request Timeout",
            content_type: None,
            content_length: 0,
        }
    }

    fn headers_too_large() -> Self {
        Self {
            status: 431,
            reason: "Request Header Fields Too Large",
            content_type: None,
            content_length: 0,
        }
    }

    /// Admission pressure from the fixed worker set, not ill health.
    fn busy() -> Self {
        Self {
            status: 503,
            reason: "Service Unavailable",
            content_type: None,
            content_length: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TOKEN: &str = "hls-token-0123456789abcdef0123456789";
    const TEST_ROOT: &str = "/srv/freemix/hls";

    fn test_roots() -> HlsRoots {
        HlsRoots::try_from_targets([(7, PathBuf::from(TEST_ROOT))]).unwrap()
    }

    #[test]
    fn route_matrix_accepts_exactly_the_four_published_shapes() {
        let roots = test_roots();
        for relative in [
            "index.m3u8",
            "segment-00000.ts",
            "segment-99999.ts",
            "0/index.m3u8",
            "7/index.m3u8",
            "0/segment-00000.ts",
            "7/segment-54321.ts",
        ] {
            assert_eq!(
                roots.resolve(7, relative),
                Ok(PathBuf::from(TEST_ROOT).join(relative)),
                "{relative:?}"
            );
        }
    }

    #[test]
    fn route_matrix_refuses_traversal_dotted_and_malformed_shapes() {
        let roots = test_roots();
        for relative in [
            "",
            "/",
            "/index.m3u8",
            "//index.m3u8",
            "index.m3u8/",
            "index/m3u8",
            ".",
            "..",
            "../index.m3u8",
            "./index.m3u8",
            "0/../index.m3u8",
            "0/./index.m3u8",
            "0//index.m3u8",
            "0/",
            "\\index.m3u8",
            "0\\index.m3u8",
            "index.m3u8?query=1",
            "index.m3u8#fragment",
            "segment-00000.ts?query=1",
            "index.m3u8.bak",
            "index.mp4",
            "INDEX.M3U8",
            "Segment-00000.ts",
            "segment-0000.ts",
            "segment-000000.ts",
            "segment-0000a.ts",
            "segment-00000.TS",
            "segment-00000.ts.ts",
            "segment-00000.mp4",
            "segment-00000",
            "segment-",
            "segment-.ts",
            "8/index.m3u8",
            "8/segment-00000.ts",
            "9/index.m3u8",
            "00/index.m3u8",
            "07/segment-00000.ts",
            "-1/index.m3u8",
            "a/index.m3u8",
            "rung/index.m3u8",
            "0/segment-00000.ts/1",
            "0/index.m3u8/segment-00000.ts",
            "0/1/index.m3u8",
        ] {
            assert_eq!(
                roots.resolve(7, relative),
                Err(HlsRouteError::NotFound),
                "{relative:?}"
            );
        }
    }

    #[test]
    fn duplicate_target_ids_are_refused() {
        assert!(matches!(
            HlsRoots::try_from_targets([
                (1, PathBuf::from("/srv/one")),
                (2, PathBuf::from("/srv/two")),
                (1, PathBuf::from("/srv/three")),
            ]),
            Err(HlsRouteError::DuplicateTarget)
        ));
    }

    #[test]
    fn unknown_target_is_not_found_and_empty_roots_are_empty() {
        let roots = test_roots();
        assert_eq!(roots.resolve(8, "index.m3u8"), Err(HlsRouteError::NotFound));
        assert!(
            HlsRoots::try_from_targets(std::iter::empty::<(u128, PathBuf)>())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn file_gate_refuses_symlinks_directories_and_oversized_files() {
        let directory = tempfile::tempdir().unwrap();
        let regular = directory.path().join("segment-00000.ts");
        fs::write(&regular, b"ts").unwrap();
        assert!(open_servable_file(&regular).is_some());

        #[cfg(unix)]
        {
            let link = directory.path().join("segment-00001.ts");
            std::os::unix::fs::symlink(&regular, &link).unwrap();
            assert!(open_servable_file(&link).is_none());
        }

        assert!(open_servable_file(directory.path()).is_none());
        assert!(open_servable_file(&directory.path().join("missing.ts")).is_none());

        // The cap is checked before any byte is read, so the test allocates a
        // sparse file instead of writing 16 MiB.
        let oversized = directory.path().join("segment-00002.ts");
        let file = File::create(&oversized).unwrap();
        file.set_len(MAX_HLS_FILE_BYTES + 1).unwrap();
        drop(file);
        assert!(open_servable_file(&oversized).is_none());
    }

    fn raw_request(address: SocketAddr, request: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
            .expect("the HLS listener is reachable");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request).unwrap();
        stream.flush().unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        response
    }

    fn response_parts(response: &[u8]) -> (String, Vec<u8>) {
        let end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("the response carries a head terminator");
        (
            String::from_utf8(response[..end].to_vec()).unwrap(),
            response[end + 4..].to_vec(),
        )
    }

    #[test]
    fn loopback_listener_requires_its_token_and_serves_exact_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let playlist = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n";
        let segment = b"\x47segment-bytes-00000";
        fs::write(directory.path().join("index.m3u8"), playlist).unwrap();
        fs::write(directory.path().join("segment-00000.ts"), segment).unwrap();

        let roots = HlsRoots::try_from_targets([(9_u128, directory.path().to_path_buf())]).unwrap();
        let server = HlsServer::start(
            "127.0.0.1:0".parse().unwrap(),
            TEST_TOKEN.as_bytes().to_vec(),
            roots,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let address = server.address();
        let playlist_request = format!(
            "GET /v1/hls/9/index.m3u8 HTTP/1.1\r\nHost: hls\r\nAuthorization: Bearer {TEST_TOKEN}\r\n\r\n"
        );

        // Missing and wrong tokens both answer 401 with no body.
        for request in [
            b"GET /v1/hls/9/index.m3u8 HTTP/1.1\r\nHost: hls\r\n\r\n".to_vec(),
            b"GET /v1/hls/9/index.m3u8 HTTP/1.1\r\nHost: hls\r\nAuthorization: Bearer hls-token-0123456789abcdef0123456780\r\n\r\n".to_vec(),
        ] {
            let (head, body) = response_parts(&raw_request(address, &request));
            assert!(head.starts_with("HTTP/1.1 401 Unauthorized\r\n"), "{head}");
            assert!(head.contains("\r\nContent-Length: 0\r\n"), "{head}");
            assert!(body.is_empty(), "401 carries a body");
        }

        let (head, body) = response_parts(&raw_request(address, playlist_request.as_bytes()));
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
        assert!(
            head.contains("\r\nContent-Type: application/vnd.apple.mpegurl\r\n"),
            "{head}"
        );
        assert!(
            head.contains(&format!("\r\nContent-Length: {}\r\n", playlist.len())),
            "{head}"
        );
        assert!(head.contains("\r\nCache-Control: no-store\r\n"), "{head}");
        assert_eq!(body, playlist.to_vec());

        let segment_request = format!(
            "GET /v1/hls/9/segment-00000.ts HTTP/1.1\r\nHost: hls\r\nAuthorization: Bearer {TEST_TOKEN}\r\n\r\n"
        );
        let (head, body) = response_parts(&raw_request(address, segment_request.as_bytes()));
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
        assert!(head.contains("\r\nContent-Type: video/mp2t\r\n"), "{head}");
        assert_eq!(body, segment.to_vec());

        // HEAD answers the headers a GET would and no body.
        let head_request = format!(
            "HEAD /v1/hls/9/segment-00000.ts HTTP/1.1\r\nHost: hls\r\nAuthorization: Bearer {TEST_TOKEN}\r\n\r\n"
        );
        let (head, body) = response_parts(&raw_request(address, head_request.as_bytes()));
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
        assert!(
            head.contains(&format!("\r\nContent-Length: {}\r\n", segment.len())),
            "{head}"
        );
        assert!(body.is_empty(), "HEAD carries a body");

        // A missing file, a traversal shape, and an unknown target answer 404.
        for request in [
            format!(
                "GET /v1/hls/9/segment-00001.ts HTTP/1.1\r\nHost: hls\r\nAuthorization: Bearer {TEST_TOKEN}\r\n\r\n"
            ),
            format!(
                "GET /v1/hls/9/../index.m3u8 HTTP/1.1\r\nHost: hls\r\nAuthorization: Bearer {TEST_TOKEN}\r\n\r\n"
            ),
            format!(
                "GET /v1/hls/10/index.m3u8 HTTP/1.1\r\nHost: hls\r\nAuthorization: Bearer {TEST_TOKEN}\r\n\r\n"
            ),
        ] {
            let (head, body) = response_parts(&raw_request(address, request.as_bytes()));
            assert!(head.starts_with("HTTP/1.1 404 Not Found\r\n"), "{head}");
            assert!(body.is_empty(), "404 carries a body");
        }

        let post = format!(
            "POST /v1/hls/9/index.m3u8 HTTP/1.1\r\nHost: hls\r\nAuthorization: Bearer {TEST_TOKEN}\r\n\r\n"
        );
        let (head, body) = response_parts(&raw_request(address, post.as_bytes()));
        assert!(
            head.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"),
            "{head}"
        );
        assert!(body.is_empty(), "405 carries a body");

        // No response ever carries the token or the served directory.
        let root_text = directory.path().display().to_string();
        let traversal = format!(
            "GET /v1/hls/9/../index.m3u8 HTTP/1.1\r\nHost: hls\r\nAuthorization: Bearer {TEST_TOKEN}\r\n\r\n"
        );
        for request in [playlist_request.as_str(), traversal.as_str()] {
            let response = raw_request(address, request.as_bytes());
            let text = String::from_utf8_lossy(&response).into_owned();
            assert!(!text.contains(TEST_TOKEN), "{text}");
            assert!(!text.contains(&root_text), "{text}");
        }

        server.shutdown().unwrap();
        assert!(
            TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_err(),
            "the listener still accepts after shutdown"
        );
    }
}
