//! Bounded loopback status listener for operators and process supervisors.
//!
//! The listener owns a fixed set of threads and never touches engine, control,
//! or render state. It answers probes from [`DaemonStatus`], an atomic snapshot
//! the control loop republishes on every pass, so a probe can never block or
//! slow the scheduler and can never read a value frozen at startup.
//!
//! Liveness and readiness are answered by the accept thread itself. Both are a
//! pure atomic read with no further I/O, so they must never queue behind
//! anything: the accept thread reads request heads with non-blocking sockets
//! and a fixed pending set, and hands work to a request thread only for the
//! support bundle, the one route that costs real work. A flood of connections
//! that send nothing therefore cannot starve a supervisor probe.
//!
//! Every bound is fixed at compile time: at most [`PENDING_CAPACITY`] sockets
//! held by the accept thread, [`CONNECTION_WORKERS`] bundle threads plus one
//! queued socket each, [`MAX_REQUEST_BYTES`] of request head,
//! [`MAX_REQUEST_HEADERS`] headers, and a response no larger than the capped
//! support bundle. Each phase carries its own deadline, which is what bounds
//! how long [`StatusListener::shutdown`] can take to join.
//!
//! The support bundle also carries the latest operator-state section the
//! control loop published, bounded to [`OPERATOR_STATE_MAX_BYTES`]. The cell
//! is written at most once a second and copied out under one short lock, so a
//! probe never queues behind rendering and the listener still owns no engine
//! state. The daemon's named statistics (RC-015) follow it under
//! `## Statistics`, published the same way and bounded to
//! [`STATISTICS_MAX_BYTES`].

use std::{
    env,
    fmt::Write as _,
    io::{ErrorKind, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use fm_observability::{
    Category, ComponentHealth, EventField, EventLog, HealthCheck, HealthRegistry, Metric,
    MetricStore, STATISTICS_HEADING, Severity, SupportBundle,
};
use fm_server::{HealthState, ReadinessState, ServiceStatus};

use super::AppResult;

const TOKEN_ENVIRONMENT: &str = "FREEMIXD_STATUS_TOKEN";
const TOKEN_MIN_BYTES: usize = 32;
const TOKEN_MAX_BYTES: usize = 256;

const HEALTH_PATH: &str = "/healthz";
const READY_PATH: &str = "/readyz";
const BUNDLE_PATH: &str = "/v1/support-bundle";

/// Support-bundle threads. One queued socket each caps bundle work at four.
const CONNECTION_WORKERS: usize = 2;
const WORKER_QUEUE_CAPACITY: usize = 1;
/// Sockets the accept thread reads concurrently. A connection that sends
/// nothing holds one of these for at most [`FIRST_BYTE_TIMEOUT`] and never
/// holds a request thread at all.
const PENDING_CAPACITY: usize = 64;
const MAX_REQUEST_BYTES: usize = 4 * 1024;
const MAX_REQUEST_HEADERS: usize = 32;
const MAX_DRAIN_BYTES: usize = 16 * 1024;
const BUNDLE_MAX_BYTES: usize = 32 * 1024;
const EVENT_CAPACITY: usize = 8;
const METRIC_CAPACITY: usize = 8;

/// Operator-state section published by the control loop. The daemon renders
/// within this bound; an oversized publish is truncated at a UTF-8 boundary
/// and marked instead of being dropped or growing the cell. The bundle route's
/// own [`BUNDLE_MAX_BYTES`] cap still applies on top.
const OPERATOR_STATE_MAX_BYTES: usize = 4 * 1024;
const OPERATOR_STATE_TRUNCATION_MARKER: &str = "\n[TRUNCATED]\n";
/// The `fm-statistics-v1` text the control loop publishes once a second
/// (RC-015), truncated and marked like the operator state.
const STATISTICS_MAX_BYTES: usize = 16 * 1024;

/// Accept-thread sleep while it holds no socket. Matched to the control loop's
/// own poll interval so a probe is never accepted later than the daemon's
/// scheduling quantum.
const ACCEPT_POLL: Duration = Duration::from_millis(5);
/// Accept-thread sleep while sockets are mid-head or mid-reply.
const PENDING_POLL: Duration = Duration::from_millis(1);
/// A connection that sends nothing is closed this fast so it cannot crowd the
/// pending set and delay a supervisor probe behind it.
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_millis(50);
/// Time allowed to finish a head once its first byte has arrived.
const HEAD_DEADLINE: Duration = Duration::from_millis(250);
/// Time allowed to push a reply the accept thread writes itself.
const REPLY_DEADLINE: Duration = Duration::from_millis(250);
/// How long a stopping accept thread keeps serving the sockets it already
/// admitted, so a probe made while the daemon drains reads `draining` instead
/// of a bare close a supervisor cannot tell from a crash.
const SHUTDOWN_FLUSH: Duration = Duration::from_millis(250);

const SOCKET_READ_TIMEOUT: Duration = Duration::from_millis(250);
const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_millis(250);
const RESPONSE_DEADLINE: Duration = Duration::from_millis(500);
const DRAIN_DEADLINE: Duration = Duration::from_millis(100);

/// How long the control loop may go silent before liveness fails.
///
/// The loop polls at `CLIENT_READ_POLL_INTERVAL` (5 ms), so this is 150 passes
/// and roughly 45 dropped frames at 60 fps: far beyond scheduling jitter or one
/// slow pass, so a healthy daemon cannot be restarted by a supervisor watching
/// this endpoint. It sits just above the 600 ms of silence after which the same
/// loop declares a *client* dead, so by the time the daemon calls its own
/// control loop stalled it has already stopped meeting the liveness budget it
/// enforces on everyone else — the report is a fact, not a guess.
const CONTROL_STALL_LIMIT_MILLIS: u64 = 750;

const READINESS_STARTING: u8 = 0;
const READINESS_READY: u8 = 1;
const READINESS_DRAINING: u8 = 2;
const READINESS_UNHEALTHY: u8 = 3;
const HEALTH_HEALTHY: u8 = 0;
const HEALTH_UNHEALTHY: u8 = 1;
const UNSET_MILLIS: u64 = u64::MAX;

/// Daemon state shared with the status listener threads.
///
/// Every probe field is atomic, so `/healthz` and `/readyz` are pure reads.
/// The operator-state section is the one lock-guarded cell: the control loop
/// writes it at most once a second and a bundle worker copies it out, and both
/// sides hold the lock only for one bounded copy.
pub(super) struct DaemonStatus {
    started: Instant,
    beat_millis: AtomicU64,
    readiness: AtomicU8,
    health: AtomicU8,
    draining: AtomicBool,
    ready_millis: AtomicU64,
    draining_millis: AtomicU64,
    served: AtomicU64,
    rejected: AtomicU64,
    in_flight: AtomicUsize,
    live_workers: AtomicUsize,
    /// Latest sanitized operator-state text, empty until the first publish.
    /// Never larger than [`OPERATOR_STATE_MAX_BYTES`].
    operator_state: Mutex<String>,
    /// Latest statistics text, empty until the first publish. Never larger
    /// than [`STATISTICS_MAX_BYTES`].
    statistics: Mutex<String>,
}

impl DaemonStatus {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            // No beat has happened yet, so a probe that arrives before the
            // control loop's first pass must read stalled, not live.
            beat_millis: AtomicU64::new(UNSET_MILLIS),
            readiness: AtomicU8::new(READINESS_STARTING),
            health: AtomicU8::new(HEALTH_HEALTHY),
            draining: AtomicBool::new(false),
            ready_millis: AtomicU64::new(UNSET_MILLIS),
            draining_millis: AtomicU64::new(UNSET_MILLIS),
            served: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
            live_workers: AtomicUsize::new(CONNECTION_WORKERS),
            operator_state: Mutex::new(String::new()),
            statistics: Mutex::new(String::new()),
        })
    }

    fn uptime_millis(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Records that the control loop completed another pass. One clock read and
    /// one release store; the loop calls this several times per pass.
    pub(super) fn beat(&self) {
        self.beat_millis
            .store(self.uptime_millis(), Ordering::Release);
    }

    /// Publishes the authoritative `fm-server` readiness and health states.
    ///
    /// The control loop calls this on every pass, so a daemon that degrades is
    /// visible to a supervisor within one poll interval rather than being
    /// frozen at whatever startup happened to publish.
    pub(super) fn publish(&self, status: ServiceStatus) {
        let readiness = match status.readiness() {
            ReadinessState::Starting => READINESS_STARTING,
            ReadinessState::Ready => READINESS_READY,
            ReadinessState::Draining => READINESS_DRAINING,
            ReadinessState::Unhealthy => READINESS_UNHEALTHY,
        };
        self.readiness.store(readiness, Ordering::Release);
        self.health.store(
            match status.health() {
                HealthState::Healthy => HEALTH_HEALTHY,
                HealthState::Unhealthy => HEALTH_UNHEALTHY,
            },
            Ordering::Release,
        );
        if readiness == READINESS_READY {
            self.stamp(&self.ready_millis);
        }
    }

    /// Publishes the latest sanitized operator-state section.
    ///
    /// The control loop calls this at most once a second, so the caller only
    /// ever pays one bounded copy into the cell. Text longer than
    /// [`OPERATOR_STATE_MAX_BYTES`] is truncated at a UTF-8 boundary and
    /// marked rather than stored whole, so the cell can never grow past the
    /// documented bound no matter what the caller publishes.
    pub(super) fn publish_operator_state(&self, text: &str) {
        store_capped(&self.operator_state, text, OPERATOR_STATE_MAX_BYTES);
    }

    /// Publishes the latest `fm-statistics-v1` text (RC-015), bounded like
    /// [`Self::publish_operator_state`] to [`STATISTICS_MAX_BYTES`].
    pub(super) fn publish_statistics(&self, text: &str) {
        store_capped(&self.statistics, text, STATISTICS_MAX_BYTES);
    }

    /// Copies the published statistics out of the cell, or `None` when the
    /// control loop never published any.
    pub(super) fn statistics(&self) -> Option<Box<str>> {
        let slot = self
            .statistics
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (!slot.is_empty()).then(|| slot.clone().into_boxed_str())
    }

    /// Copies the published operator state out of the cell, or `None` when the
    /// control loop never published one.
    pub(super) fn operator_state(&self) -> Option<Box<str>> {
        let slot = self
            .operator_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (!slot.is_empty()).then(|| slot.clone().into_boxed_str())
    }

    /// Latches the cooperative stop request.
    ///
    /// Draining is a process-level fact the republished server status must not
    /// erase, so it is held beside the published state and folded in by
    /// [`Self::snapshot`] rather than written over the published readiness.
    pub(super) fn begin_draining(&self) {
        if !self.draining.swap(true, Ordering::AcqRel) {
            self.stamp(&self.draining_millis);
        }
    }

    /// Records that a request thread is gone, so `/healthz` shows the loss now
    /// instead of only at shutdown.
    fn retire_workers(&self, live: usize) {
        self.live_workers.store(live, Ordering::Release);
    }

    fn stamp(&self, slot: &AtomicU64) {
        let _ = slot.compare_exchange(
            UNSET_MILLIS,
            self.uptime_millis(),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn snapshot(&self) -> StatusSnapshot {
        let uptime_millis = self.uptime_millis();
        let beat_millis = self.beat_millis.load(Ordering::Acquire);
        let published = match self.readiness.load(Ordering::Acquire) {
            READINESS_READY => ReadinessState::Ready,
            READINESS_DRAINING => ReadinessState::Draining,
            READINESS_UNHEALTHY => ReadinessState::Unhealthy,
            _ => ReadinessState::Starting,
        };
        StatusSnapshot {
            uptime_millis,
            beat_millis: unset_to_none(beat_millis),
            stalled: beat_millis == UNSET_MILLIS
                || uptime_millis.saturating_sub(beat_millis) > CONTROL_STALL_LIMIT_MILLIS,
            readiness: if self.draining.load(Ordering::Acquire)
                && published == ReadinessState::Ready
            {
                ReadinessState::Draining
            } else {
                published
            },
            health: if self.health.load(Ordering::Acquire) == HEALTH_UNHEALTHY {
                HealthState::Unhealthy
            } else {
                HealthState::Healthy
            },
            ready_millis: unset_to_none(self.ready_millis.load(Ordering::Acquire)),
            draining_millis: unset_to_none(self.draining_millis.load(Ordering::Acquire)),
            served: self.served.load(Ordering::Acquire),
            rejected: self.rejected.load(Ordering::Acquire),
            in_flight: self.in_flight.load(Ordering::Acquire),
            live_workers: self.live_workers.load(Ordering::Acquire),
        }
    }
}

const fn unset_to_none(millis: u64) -> Option<u64> {
    if millis == UNSET_MILLIS {
        None
    } else {
        Some(millis)
    }
}

#[derive(Clone, Copy)]
struct StatusSnapshot {
    uptime_millis: u64,
    beat_millis: Option<u64>,
    stalled: bool,
    readiness: ReadinessState,
    health: HealthState,
    ready_millis: Option<u64>,
    draining_millis: Option<u64>,
    served: u64,
    rejected: u64,
    in_flight: usize,
    live_workers: usize,
}

impl StatusSnapshot {
    fn live(&self) -> bool {
        !self.stalled && self.health == HealthState::Healthy
    }

    fn ready(&self) -> bool {
        self.live() && self.readiness == ReadinessState::Ready
    }
}

/// The bearer token guarding the support bundle, mirroring the web gateway.
struct StatusToken {
    bytes: [u8; TOKEN_MAX_BYTES],
    length: usize,
}

impl StatusToken {
    fn from_environment() -> AppResult<Self> {
        let value = env::var(TOKEN_ENVIRONMENT).map_err(|_| {
            format!("{TOKEN_ENVIRONMENT} is required when --status-listen is enabled")
        })?;
        let bytes = value.as_bytes();
        if !(TOKEN_MIN_BYTES..=TOKEN_MAX_BYTES).contains(&bytes.len())
            || !bytes.iter().all(u8::is_ascii_graphic)
        {
            return Err(format!(
                "{TOKEN_ENVIRONMENT} must be {TOKEN_MIN_BYTES}..={TOKEN_MAX_BYTES} ASCII graphic bytes"
            )
            .into());
        }
        let mut token = [0; TOKEN_MAX_BYTES];
        token[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            bytes: token,
            length: bytes.len(),
        })
    }

    fn matches(&self, authorization: Option<&[u8]>) -> bool {
        let presented = authorization.map(bearer_credentials).unwrap_or_default();
        let mut candidate = [0; TOKEN_MAX_BYTES];
        let copied = presented.len().min(TOKEN_MAX_BYTES);
        candidate[..copied].copy_from_slice(&presented[..copied]);
        let mut difference = self.length ^ presented.len();
        for (expected, actual) in self.bytes.iter().zip(candidate) {
            difference |= usize::from(*expected ^ actual);
        }
        difference == 0
    }
}

/// Extracts the credentials of a `Bearer` challenge.
///
/// RFC 7235 makes the scheme case-insensitive and allows more than one space
/// before the credentials; the credentials themselves stay exact so the token
/// comparison in [`StatusToken::matches`] remains byte-for-byte.
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

/// A bounded HTTP/1.1 listener serving liveness, readiness, and support state.
pub(super) struct StatusListener {
    address: SocketAddr,
    cancel: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl StatusListener {
    /// Binds the loopback listener and starts its fixed thread set.
    ///
    /// # Errors
    ///
    /// Fails when the address is not loopback, the bearer token environment is
    /// missing or malformed, the socket cannot be bound, or a thread cannot be
    /// spawned. Threads already started are stopped before returning.
    pub(super) fn bind(address: SocketAddr, status: &Arc<DaemonStatus>) -> AppResult<Self> {
        if !address.ip().is_loopback() {
            return Err("--status-listen must use a loopback address".into());
        }
        let token = Arc::new(StatusToken::from_environment()?);
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let bound = listener.local_addr()?;

        let cancel = Arc::new(AtomicBool::new(false));
        let mut senders = Vec::with_capacity(CONNECTION_WORKERS);
        let mut threads = Vec::with_capacity(CONNECTION_WORKERS + 1);
        for index in 0..CONNECTION_WORKERS {
            let (sender, receiver) = mpsc::sync_channel(WORKER_QUEUE_CAPACITY);
            let worker_status = Arc::clone(status);
            match thread::Builder::new()
                .name(format!("freemixd-status-{index}"))
                .spawn(move || worker_loop(&receiver, &worker_status))
            {
                Ok(handle) => {
                    senders.push(sender);
                    threads.push(handle);
                }
                Err(error) => {
                    // A request thread runs until its sender is dropped.
                    drop(senders);
                    return Err(stop_threads(&cancel, threads, error));
                }
            }
        }

        let accept_cancel = Arc::clone(&cancel);
        let accept_status = Arc::clone(status);
        match thread::Builder::new()
            .name("freemixd-status-accept".into())
            .spawn(move || {
                accept_loop(&listener, senders, &accept_cancel, &accept_status, &token);
            }) {
            Ok(handle) => threads.insert(0, handle),
            // The failed spawn dropped its closure, and the worker senders in it.
            Err(error) => return Err(stop_threads(&cancel, threads, error)),
        }
        Ok(Self {
            address: bound,
            cancel,
            threads,
        })
    }

    pub(super) fn address(&self) -> SocketAddr {
        self.address
    }

    /// Stops accepting and joins every listener thread.
    ///
    /// The join is bounded, but the bound is a deadline *plus one socket
    /// timeout per phase*, because a deadline is only re-checked between calls
    /// and a blocking call can already be inside its own timeout. The accept
    /// thread never blocks on a socket; it notices the stop within one
    /// [`ACCEPT_POLL`] and then answers what it already admitted for at most
    /// [`SHUTDOWN_FLUSH`]. It is joined first, which drops the worker senders,
    /// so a request thread serves every bundle already queued to it and then
    /// exits: each costs at most [`RESPONSE_DEADLINE`] +
    /// [`SOCKET_WRITE_TIMEOUT`] + [`DRAIN_DEADLINE`] + [`SOCKET_READ_TIMEOUT`]
    /// — about 1.1 s worst case, and a few tens of milliseconds in practice.
    ///
    /// # Errors
    ///
    /// Returns an error if a listener thread panicked.
    pub(super) fn shutdown(mut self) -> AppResult<()> {
        if self.stop() {
            return Err("freemixd status listener thread panicked".into());
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

impl Drop for StatusListener {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn stop_threads(
    cancel: &AtomicBool,
    threads: Vec<JoinHandle<()>>,
    error: std::io::Error,
) -> Box<dyn std::error::Error> {
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

    fn answer(&mut self, response: &Response, status: &DaemonStatus, now: Instant) {
        status.served.fetch_add(1, Ordering::AcqRel);
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

/// A bundle request that has already been parsed and authorized.
struct BundleJob {
    stream: TcpStream,
    snapshot: StatusSnapshot,
}

/// What the accept thread should do with a pending socket after one step.
enum Step {
    Hold,
    Close,
    Bundle,
}

fn accept_loop(
    listener: &TcpListener,
    mut senders: Vec<SyncSender<BundleJob>>,
    cancel: &AtomicBool,
    status: &DaemonStatus,
    token: &StatusToken,
) {
    let mut pending: Vec<Pending> = Vec::with_capacity(PENDING_CAPACITY);
    while !cancel.load(Ordering::Acquire) {
        let listening = accept_available(listener, &mut pending, status);
        let progressed = advance_pending(&mut pending, &mut senders, status, token);
        if !listening {
            break;
        }
        if pending.is_empty() {
            thread::sleep(ACCEPT_POLL);
        } else if !progressed {
            thread::sleep(PENDING_POLL);
        }
    }
    finish_admitted(listener, pending, &mut senders, status, token);
}

/// Answers the connections the listener already admitted before it closes.
///
/// The daemon is draining by the time its listener stops, and a supervisor
/// polling `/readyz` then must read that, not a close it cannot tell from a
/// crash. On a loaded host the accept thread can be scheduled only after the
/// stop, holding every probe made during the drain, so those are served now:
/// the kernel backlog is admitted once more, then every held socket has until
/// [`SHUTDOWN_FLUSH`] (and its own deadlines) to finish its head and take its
/// reply. Whatever is unfinished then is closed. Nothing is admitted after
/// this, and the listening socket closes when the thread returns.
fn finish_admitted(
    listener: &TcpListener,
    mut pending: Vec<Pending>,
    senders: &mut Vec<SyncSender<BundleJob>>,
    status: &DaemonStatus,
    token: &StatusToken,
) {
    let deadline = Instant::now() + SHUTDOWN_FLUSH;
    accept_available(listener, &mut pending, status);
    while !pending.is_empty() && Instant::now() < deadline {
        if !advance_pending(&mut pending, senders, status, token) {
            thread::sleep(PENDING_POLL);
        }
    }
    for connection in pending {
        close_pending(connection, status);
    }
}

/// Fills the pending set from the accept queue. Returns false on a fatal error.
///
/// Nothing is admitted past [`PENDING_CAPACITY`]; the surplus waits in the
/// kernel backlog, where it costs the listener nothing, until a slot frees.
fn accept_available(
    listener: &TcpListener,
    pending: &mut Vec<Pending>,
    status: &DaemonStatus,
) -> bool {
    while pending.len() < PENDING_CAPACITY {
        match listener.accept() {
            Ok((stream, _)) => {
                if stream.set_nonblocking(true).is_err() {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                status.in_flight.fetch_add(1, Ordering::AcqRel);
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
    senders: &mut Vec<SyncSender<BundleJob>>,
    status: &DaemonStatus,
    token: &StatusToken,
) -> bool {
    let now = Instant::now();
    let mut progressed = false;
    let mut index = 0;
    while index < pending.len() {
        match advance_one(&mut pending[index], status, token, now, &mut progressed) {
            Step::Hold => index += 1,
            Step::Close => close_pending(pending.swap_remove(index), status),
            Step::Bundle => {
                let Pending { stream, .. } = pending.swap_remove(index);
                let job = BundleJob {
                    stream,
                    snapshot: status.snapshot(),
                };
                if let Err(stream) = dispatch_bundle(job, senders, status) {
                    status.rejected.fetch_add(1, Ordering::AcqRel);
                    status.served.fetch_add(1, Ordering::AcqRel);
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
    status: &DaemonStatus,
    token: &StatusToken,
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
            classify(connection, status, token, now)
        }
        Err(error) if error.kind() == ErrorKind::Interrupted => Step::Hold,
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            if now >= connection.deadline {
                connection.answer(&Response::request_timeout(), status, now);
            }
            Step::Hold
        }
        Err(_) => Step::Close,
    }
}

/// Routes a head as soon as it is complete, or rejects one that never ends.
fn classify(
    connection: &mut Pending,
    status: &DaemonStatus,
    token: &StatusToken,
    now: Instant,
) -> Step {
    if let Some(end) = connection
        .head
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
    {
        connection.head.truncate(end);
        match route(&connection.head, status, token) {
            Routed::Reply(response) => {
                connection.answer(&response, status, now);
                Step::Hold
            }
            Routed::Bundle => Step::Bundle,
        }
    } else if connection.head.len() >= MAX_REQUEST_BYTES {
        connection.answer(&Response::request_too_large(), status, now);
        Step::Hold
    } else {
        Step::Hold
    }
}

/// Hands an authorized bundle request to a request thread.
///
/// A disconnected worker means that thread panicked. The connection is carried
/// on to the next worker instead of being dropped into a dead channel, and the
/// sender is retired so no later connection is offered to it either.
fn dispatch_bundle(
    job: BundleJob,
    senders: &mut Vec<SyncSender<BundleJob>>,
    status: &DaemonStatus,
) -> Result<(), TcpStream> {
    let mut job = job;
    let mut index = 0;
    while index < senders.len() {
        match senders[index].try_send(job) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) => {
                job = returned;
                index += 1;
            }
            Err(TrySendError::Disconnected(returned)) => {
                job = returned;
                senders.remove(index);
                status.retire_workers(senders.len());
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

fn close_pending(mut connection: Pending, status: &DaemonStatus) {
    drain_buffered(&mut connection.stream);
    let _ = connection.stream.shutdown(Shutdown::Both);
    status.in_flight.fetch_sub(1, Ordering::AcqRel);
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

/// Serves bundles until the accept thread drops its sender, after serving
/// whatever it queued before (including during its shutdown flush).
fn worker_loop(jobs: &Receiver<BundleJob>, status: &DaemonStatus) {
    for job in jobs {
        serve_bundle(job, status);
    }
}

fn serve_bundle(job: BundleJob, status: &DaemonStatus) {
    let BundleJob {
        mut stream,
        snapshot,
    } = job;
    if prepare(&mut stream).is_ok() {
        let operator_state = status.operator_state();
        let statistics = status.statistics();
        let response = bundle_response(&snapshot, operator_state.as_deref(), statistics.as_deref());
        status.served.fetch_add(1, Ordering::AcqRel);
        write_response(&mut stream, &response);
        drain(&mut stream);
    }
    let _ = stream.shutdown(Shutdown::Both);
    status.in_flight.fetch_sub(1, Ordering::AcqRel);
}

fn prepare(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(SOCKET_READ_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_WRITE_TIMEOUT))
}

/// Consumes any request remainder so the close is graceful instead of a reset.
///
/// The deadline is only checked between reads, so the true bound is
/// [`DRAIN_DEADLINE`] plus one [`SOCKET_READ_TIMEOUT`].
fn drain(stream: &mut TcpStream) {
    let deadline = Instant::now() + DRAIN_DEADLINE;
    let mut chunk = [0_u8; 1024];
    let mut drained = 0;
    while drained < MAX_DRAIN_BYTES && Instant::now() < deadline {
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => drained += read,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

fn encode_response(response: &Response) -> Vec<u8> {
    let mut head = String::with_capacity(256);
    let _ = write!(
        head,
        "HTTP/1.1 {} {}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        response.status,
        response.reason,
        response.body.len()
    );
    if response.authenticate {
        head.push_str("WWW-Authenticate: Bearer\r\n");
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    // A HEAD reply carries the headers a GET would, and no body.
    if !response.head_only {
        bytes.extend_from_slice(response.body.as_bytes());
    }
    bytes
}

/// Writes a rendered response on a blocking socket.
///
/// The deadline is only checked between writes, so the true bound is
/// [`RESPONSE_DEADLINE`] plus one [`SOCKET_WRITE_TIMEOUT`].
fn write_response(stream: &mut TcpStream, response: &Response) {
    let bytes = encode_response(response);
    let deadline = Instant::now() + RESPONSE_DEADLINE;
    let mut offset = 0;
    while offset < bytes.len() {
        if Instant::now() >= deadline {
            break;
        }
        match stream.write(&bytes[offset..]) {
            Ok(0) => break,
            Ok(written) => offset += written,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = stream.flush();
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
        let path = target.split(['?', '#']).next().unwrap_or(target);

        let mut authorization = None;
        let mut headers = 0_usize;
        for line in lines.filter(|line| !line.is_empty()) {
            headers += 1;
            if headers > MAX_REQUEST_HEADERS {
                return Err(Response::too_many_headers());
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(Response::bad_request());
            };
            let value = value.trim();
            if name.eq_ignore_ascii_case("authorization") {
                if authorization.is_some() {
                    return Err(Response::bad_request());
                }
                authorization = Some(value.as_bytes());
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                || (name.eq_ignore_ascii_case("content-length") && value != "0")
            {
                // The listener never reads a request body.
                return Err(Response::bad_request());
            }
        }
        Ok(Self {
            method,
            path,
            authorization,
        })
    }
}

/// Either a reply the accept thread can write itself, or the one route that
/// needs a request thread.
enum Routed {
    Reply(Response),
    Bundle,
}

fn route(head: &[u8], status: &DaemonStatus, token: &StatusToken) -> Routed {
    let request = match RequestHead::parse(head) {
        Ok(request) => request,
        Err(response) => return Routed::Reply(response),
    };
    match request.path {
        HEALTH_PATH | READY_PATH => {
            let Some(head_only) = probe_method(request.method) else {
                return Routed::Reply(Response::method_not_allowed());
            };
            let snapshot = status.snapshot();
            let response = if request.path == HEALTH_PATH {
                health_response(&snapshot)
            } else {
                ready_response(&snapshot)
            };
            Routed::Reply(response.head_only(head_only))
        }
        // Authentication is evaluated before the method so an unauthenticated
        // caller learns nothing from the shape of the rejection.
        BUNDLE_PATH if !token.matches(request.authorization) => {
            Routed::Reply(Response::unauthorized())
        }
        BUNDLE_PATH if request.method != "GET" => Routed::Reply(Response::method_not_allowed()),
        BUNDLE_PATH => Routed::Bundle,
        _ => Routed::Reply(Response::not_found()),
    }
}

/// Maps a probe method to whether the reply is headers only.
fn probe_method(method: &str) -> Option<bool> {
    match method {
        "GET" => Some(false),
        "HEAD" => Some(true),
        _ => None,
    }
}

fn health_response(snapshot: &StatusSnapshot) -> Response {
    let state = if snapshot.health == HealthState::Unhealthy {
        "unhealthy"
    } else if snapshot.stalled {
        "stalled"
    } else {
        "live"
    };
    let body = format!(
        "FREEMIXD_STATUS\tv=1\tcheck=healthz\tstatus={state}\tuptime_ms={}\tlast_beat_ms={}\tworkers={}/{CONNECTION_WORKERS}\n",
        snapshot.uptime_millis,
        snapshot
            .beat_millis
            .map_or_else(|| "none".to_owned(), |millis| millis.to_string()),
        snapshot.live_workers
    );
    if snapshot.live() {
        Response::ok(body)
    } else {
        Response::unavailable(body)
    }
}

fn ready_response(snapshot: &StatusSnapshot) -> Response {
    let ready = snapshot.ready();
    let body = format!(
        "FREEMIXD_STATUS\tv=1\tcheck=readyz\tstatus={}\treadiness={}\thealth={}\tliveness={}\tuptime_ms={}\n",
        if ready { "ready" } else { "not-ready" },
        snapshot.readiness.as_str(),
        snapshot.health.as_str(),
        if snapshot.stalled { "stalled" } else { "live" },
        snapshot.uptime_millis
    );
    if ready {
        Response::ok(body)
    } else {
        Response::unavailable(body)
    }
}

/// Renders the redacted support bundle from the snapshot alone.
///
/// Nothing derived from paths, hostnames, device names, addresses, or tokens is
/// ever placed into the bundle inputs. The operator-state section arrives
/// already sanitized by the control loop and is appended verbatim; the bundle
/// never re-renders secrets.
/// Stores `text` in `cell`, truncated at a UTF-8 boundary and marked when it
/// exceeds `maximum` bytes.
fn store_capped(cell: &Mutex<String>, text: &str, maximum: usize) {
    let mut slot = cell.lock().unwrap_or_else(PoisonError::into_inner);
    if text.len() <= maximum {
        if slot.as_str() == text {
            return;
        }
        slot.clear();
        slot.push_str(text);
        return;
    }
    let mut capped = String::with_capacity(maximum);
    let prefix_bytes = maximum.saturating_sub(OPERATOR_STATE_TRUNCATION_MARKER.len());
    capped.push_str(utf8_prefix(text, prefix_bytes));
    capped.push_str(OPERATOR_STATE_TRUNCATION_MARKER);
    *slot = capped;
}

/// The operator state followed by the statistics under
/// [`STATISTICS_HEADING`], as `StatisticsSnapshot::include_in_bundle` lays
/// them out.
fn appended_sections(operator_state: Option<&str>, statistics: Option<&str>) -> Option<String> {
    let Some(statistics) = statistics else {
        return operator_state.map(str::to_owned);
    };
    let mut appended = String::new();
    if let Some(operator_state) = operator_state {
        appended.push_str(operator_state);
        if !operator_state.ends_with('\n') {
            appended.push('\n');
        }
        appended.push('\n');
    }
    appended.push_str(STATISTICS_HEADING);
    appended.push('\n');
    appended.push_str(statistics);
    Some(appended)
}

fn bundle_response(
    snapshot: &StatusSnapshot,
    operator_state: Option<&str>,
    statistics: Option<&str>,
) -> Response {
    let mut health = HealthRegistry::new();
    health.update(
        HealthCheck::new(
            "control-loop",
            !snapshot.stalled,
            snapshot.ready(),
            component_health(snapshot),
        )
        .with_detail(snapshot.readiness.as_str()),
    );
    let intact = snapshot.live_workers == CONNECTION_WORKERS;
    health.update(HealthCheck::new(
        "status-listener",
        true,
        intact,
        if intact {
            ComponentHealth::Healthy
        } else {
            ComponentHealth::Degraded
        },
    ));

    let mut metrics = MetricStore::new(METRIC_CAPACITY);
    let _ = metrics.set_gauge(
        Metric::QueueDepth,
        snapshot.uptime_millis,
        approximate(u64::try_from(snapshot.in_flight).unwrap_or(u64::MAX)),
    );
    let _ = metrics.increment_counter(
        Metric::DroppedItems,
        snapshot.uptime_millis,
        approximate(snapshot.rejected),
    );

    let mut events = EventLog::new(EVENT_CAPACITY);
    if let Some(at) = snapshot.ready_millis {
        let _ = events.record(
            at,
            Severity::Info,
            Category::Runtime,
            "control plane became ready",
            [],
        );
    }
    if let Some(at) = snapshot.draining_millis {
        let _ = events.record(
            at,
            Severity::Warning,
            Category::Runtime,
            "cooperative shutdown requested",
            [],
        );
    }
    let _ = events.record(
        snapshot.uptime_millis,
        Severity::Info,
        Category::Runtime,
        "status snapshot exported",
        [
            EventField::new("readiness", snapshot.readiness.as_str()),
            EventField::new("served", snapshot.served),
            EventField::new("rejected", snapshot.rejected),
        ],
    );

    // `CapabilityRegistry` is only reachable through `fm-observability`, so the
    // empty registry is built by inference rather than by name.
    #[allow(clippy::default_trait_access)]
    let capabilities = Default::default();
    let mut bundle = SupportBundle::new(&events, &metrics, &health, &capabilities);
    let appended = appended_sections(operator_state, statistics);
    if let Some(appended) = appended.as_deref() {
        bundle = bundle.with_operator_state(appended);
    }
    let export = bundle.export(BUNDLE_MAX_BYTES);
    let mut body = format!(
        "FREEMIXD_STATUS\tv=1\tcheck=support-bundle\tstatus=ok\tbytes={}\tuncapped_bytes={}\ttruncated={}\n",
        export.text.len(),
        export.uncapped_bytes,
        export.truncated
    );
    body.push_str(&export.text);
    Response::ok(body)
}

const fn component_health(snapshot: &StatusSnapshot) -> ComponentHealth {
    if snapshot.stalled || matches!(snapshot.health, HealthState::Unhealthy) {
        ComponentHealth::Unhealthy
    } else if matches!(snapshot.readiness, ReadinessState::Ready) {
        ComponentHealth::Healthy
    } else {
        ComponentHealth::Degraded
    }
}

/// Widens a bounded counter for metric storage without a precision-losing cast.
fn approximate(value: u64) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

/// The longest UTF-8 prefix of `value` no longer than `maximum_bytes`.
fn utf8_prefix(value: &str, maximum_bytes: usize) -> &str {
    let mut end = maximum_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

struct Response {
    status: u16,
    reason: &'static str,
    authenticate: bool,
    head_only: bool,
    body: String,
}

impl Response {
    fn new(status: u16, reason: &'static str, body: String) -> Self {
        Self {
            status,
            reason,
            authenticate: false,
            head_only: false,
            body,
        }
    }

    fn head_only(mut self, head_only: bool) -> Self {
        self.head_only = head_only;
        self
    }

    fn ok(body: String) -> Self {
        Self::new(200, "OK", body)
    }

    fn unavailable(body: String) -> Self {
        Self::new(503, "Service Unavailable", body)
    }

    fn bad_request() -> Self {
        Self::new(
            400,
            "Bad Request",
            "FREEMIXD_STATUS\tv=1\tcheck=request\tstatus=malformed\n".into(),
        )
    }

    fn unauthorized() -> Self {
        Self {
            authenticate: true,
            ..Self::new(
                401,
                "Unauthorized",
                "FREEMIXD_STATUS\tv=1\tcheck=support-bundle\tstatus=unauthorized\n".into(),
            )
        }
    }

    fn not_found() -> Self {
        Self::new(
            404,
            "Not Found",
            "FREEMIXD_STATUS\tv=1\tcheck=request\tstatus=unknown-route\n".into(),
        )
    }

    fn method_not_allowed() -> Self {
        Self::new(
            405,
            "Method Not Allowed",
            "FREEMIXD_STATUS\tv=1\tcheck=request\tstatus=method-not-allowed\n".into(),
        )
    }

    fn request_timeout() -> Self {
        Self::new(
            408,
            "Request Timeout",
            "FREEMIXD_STATUS\tv=1\tcheck=request\tstatus=timeout\n".into(),
        )
    }

    fn request_too_large() -> Self {
        Self::new(
            413,
            "Content Too Large",
            format!(
                "FREEMIXD_STATUS\tv=1\tcheck=request\tstatus=too-large\tlimit_bytes={MAX_REQUEST_BYTES}\n"
            ),
        )
    }

    fn too_many_headers() -> Self {
        Self::new(
            431,
            "Request Header Fields Too Large",
            format!(
                "FREEMIXD_STATUS\tv=1\tcheck=request\tstatus=too-many-headers\tlimit={MAX_REQUEST_HEADERS}\n"
            ),
        )
    }

    /// Admission pressure, not ill health: 503 would tell a supervisor the
    /// daemon is sick and invite a restart mid-show, so the bundle route says
    /// "too many requests" instead. Liveness and readiness never take this
    /// path — the accept thread answers them itself.
    fn busy() -> Self {
        Self::new(
            429,
            "Too Many Requests",
            format!(
                "FREEMIXD_STATUS\tv=1\tcheck=admission\tstatus=busy\tlimit={CONNECTION_WORKERS}\n"
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders the bundle body exactly as a request thread does.
    fn bundle_body(status: &DaemonStatus) -> String {
        let snapshot = status.snapshot();
        let operator_state = status.operator_state();
        let statistics = status.statistics();
        bundle_response(&snapshot, operator_state.as_deref(), statistics.as_deref()).body
    }

    #[test]
    fn support_bundle_appends_published_statistics_after_operator_state() {
        let status = DaemonStatus::new();
        assert!(!bundle_body(&status).contains(STATISTICS_HEADING));
        status.publish_operator_state("streams=0\n");
        status.publish_statistics(
            "# fm-statistics-v1\nmetric control.subscribers gauge value=1 updated_ms=0 updates=1\n",
        );
        let body = bundle_body(&status);
        let operator = body.find("streams=0").expect("operator state is kept");
        let statistics = body
            .find("## Statistics\n# fm-statistics-v1\nmetric control.subscribers")
            .expect("statistics follow under their heading");
        assert!(operator < statistics);

        let oversized = "x".repeat(STATISTICS_MAX_BYTES + 1);
        status.publish_statistics(&oversized);
        let stored = status.statistics().expect("statistics were published");
        assert!(stored.len() <= STATISTICS_MAX_BYTES);
        assert!(stored.ends_with(OPERATOR_STATE_TRUNCATION_MARKER));
    }

    #[test]
    fn support_bundle_includes_published_operator_state() {
        let status = DaemonStatus::new();
        let published = "streams=1\nstream target=7001 name=Destination 1 desired=true\nrecorders=desired=true,false\n";
        status.publish_operator_state(published);

        let body = bundle_body(&status);
        assert!(body.contains("## Operator state"));
        assert!(body.contains(published));
    }

    #[test]
    fn oversized_operator_state_is_truncated_with_a_marker() {
        let status = DaemonStatus::new();
        // One ASCII byte first, then multi-byte characters, so the cap lands
        // inside a character and the boundary back-off is exercised.
        let oversized = format!("x{}", "€".repeat(OPERATOR_STATE_MAX_BYTES));
        status.publish_operator_state(&oversized);

        let stored = status.operator_state().expect("state was published");
        assert!(stored.len() <= OPERATOR_STATE_MAX_BYTES);
        assert!(stored.ends_with(OPERATOR_STATE_TRUNCATION_MARKER));
        assert!(bundle_body(&status).contains("## Operator state"));
    }

    #[test]
    fn unpublished_operator_state_renders_no_section() {
        let status = DaemonStatus::new();
        assert!(status.operator_state().is_none());
        assert!(!bundle_body(&status).contains("## Operator state"));
    }

    /// A probe the daemon admitted while draining is answered `draining`
    /// even when the accept thread first runs after the stop: a loaded host
    /// can schedule it that late, and a bare close then reads as a crash.
    #[test]
    fn a_stopping_listener_answers_probes_it_already_admitted() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let status = DaemonStatus::new();
        status.beat();
        status.readiness.store(READINESS_READY, Ordering::Release);
        status.begin_draining();
        let mut probe = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        probe
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        probe
            .write_all(b"GET /readyz HTTP/1.1\r\nHost: status\r\n\r\n")
            .unwrap();
        let token = StatusToken {
            bytes: [b'x'; TOKEN_MAX_BYTES],
            length: TOKEN_MIN_BYTES,
        };

        // Stopped before its first pass, as when the stop wins the race.
        accept_loop(
            &listener,
            Vec::new(),
            &AtomicBool::new(true),
            &status,
            &token,
        );
        drop(listener);

        let mut response = Vec::new();
        let _ = probe.read_to_end(&mut response);
        let response = String::from_utf8_lossy(&response);
        assert!(
            response.starts_with("HTTP/1.1 503 Service Unavailable\r\n"),
            "{response:?}"
        );
        assert!(
            response.contains("status=not-ready\treadiness=draining\t"),
            "{response:?}"
        );
        assert_eq!(status.in_flight.load(Ordering::Acquire), 0);
    }

    #[test]
    fn republishing_replaces_the_stored_state() {
        let status = DaemonStatus::new();
        status.publish_operator_state("streams=1\n");
        status.publish_operator_state("streams=2\n");
        assert_eq!(status.operator_state().as_deref(), Some("streams=2\n"));
    }
}
