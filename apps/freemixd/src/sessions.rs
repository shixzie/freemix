//! The daemon's one control-session scheduler, shared by the simulated and
//! the native-media daemon.
//!
//! A single thread serves every control session: up to [`MAX_PEERS`] raw TCP
//! peers plus up to [`MAX_WEB_CLIENTS`] Web peers from the gateway. Each peer
//! is its own session (handshake, granted role, heartbeat, live
//! subscription, rate limits, outbound queue, and lossy side channels), and
//! no peer ever blocks the loop: sockets are non-blocking, Web records are
//! handed over with `try_send`, and every queue is bounded. A native daemon
//! realizes its Program frames from the same loop, so it dispatches at most
//! one command per pass (each realizes a frame) and serves every peer's
//! reads, heartbeats, and writes between commands.

use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use fm_auth::Principal;
use fm_control::{LiveEvent, SubscriberFailureReason, Subscription};
use fm_persistence::StoredProject;
use fm_protocol::{
    AudioMetersMessage, ClientType, CommandPayload, CommandResult, ErrorMessage,
    HandshakeOutcome as ProtocolHandshakeOutcome, HandshakeResponse,
    HeartbeatAcknowledgementMessage, LineDecoder, MonitorTap, ServerIdentity, StreamStatusMessage,
    StructuredError, WireMessage, encode_line,
};
use fm_server::{Server, Session, SessionError, SyncPayload};

use super::{
    AppResult, CLIENT_READ_POLL_INTERVAL, CLIENT_WRITE_TIMEOUT, CommandDelivery, ControlHandle,
    DaemonShutdownReason, NativeDaemon, PendingWrite, ProcessShutdown, SharedControl,
    current_handshake, diagnostics_response, error_message, execute_session_command,
    handshake_code, handshake_response, is_client_session_termination, now_millis,
    reconciled_handshake_outcome, record_heartbeat, rejected_handshake_response,
    requested_daemon_shutdown, server_identity, shutdown_message, structured_session_error,
};
use crate::input_thumbnails::{ThumbnailOutbox, ThumbnailWork, ThumbnailWorker};
use crate::journal::DurableStore;
use crate::latest_record::LatestRecord;
use crate::monitor_tap::SimulatedMonitorWorker;
use crate::title_render::TitleAssets;
use crate::web::{ClientSlot, MAX_WEB_CLIENTS, WebEvent, WebGateway};

/// The most raw TCP control sessions served at once, by the simulated and
/// the native daemon alike (Studio, CLI and automation peers). A further raw
/// connection is accepted only to be refused: its handshake is answered with
/// a retryable `resource_exhausted` rejection (see
/// [`MAX_REFUSING_PEERS`]). Web peers are bounded separately by
/// [`MAX_WEB_CLIENTS`].
const MAX_PEERS: usize = 8;
/// The most raw connections beyond [`MAX_PEERS`] held at once while their
/// handshake is read and refused, each for at most [`REFUSAL_DEADLINE`]. A
/// connection beyond this bound too is closed at once, unanswered, so no
/// client ever waits on an unaccepted connection.
const MAX_REFUSING_PEERS: usize = 8;
/// How long a raw connection beyond [`MAX_PEERS`] may take to send the
/// handshake it is refused.
const REFUSAL_DEADLINE: Duration = Duration::from_millis(500);
const INBOUND_CAPACITY: usize = 8;
const OUTBOUND_CAPACITY: usize = 32;
/// Outbound slots live events never take, so the reply to the message being
/// dispatched (a command result above all) normally queues at once instead
/// of waiting behind a queue filled with events.
const REPLY_RESERVE: usize = 1;
const LIVE_EVENTS_PER_PASS: usize = 8;
/// The most bytes one raw peer is written per loop pass. A pass writes
/// records until the socket would block or this budget is spent, so session
/// throughput never depends on how long the native frame realized in the
/// same pass took: a slow frame (a loaded GPU, display outputs) must not
/// starve command results, heartbeat acknowledgements, or the lossy meter
/// and monitor slots.
const WRITE_BUDGET_PER_PASS: u64 = 1024 * 1024;
/// The handshake permission that makes a Studio peer monitor-frame eligible.
const VIEW_STATUS_PERMISSION: &str = "view_status";

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    AwaitHandshake,
    Syncing,
    Active,
    Closing,
}

#[derive(Clone, Copy)]
enum Accounting {
    Raw,
    Session,
}

#[allow(clippy::struct_excessive_bools)]
struct Outbound {
    write: OutboundWrite,
    accounting: Accounting,
    handshake_response: bool,
    command_result: bool,
    accounted: bool,
    channel_sent: bool,
}

enum OutboundWrite {
    Raw(PendingWrite),
    Web(Vec<u8>),
}

struct WebTransport {
    inbound: Receiver<WireMessage>,
    outbound: SyncSender<Vec<u8>>,
    acknowledgements: Receiver<()>,
    cancel: Arc<AtomicBool>,
    /// The gateway client slot this session holds (absent in unit tests);
    /// released when the session is dropped.
    _slot: Option<ClientSlot>,
}

impl Drop for WebTransport {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

enum Transport {
    Raw(TcpStream),
    Web(WebTransport),
}

impl Transport {
    fn is_web(&self) -> bool {
        matches!(self, Self::Web(_))
    }
}

#[allow(clippy::struct_excessive_bools)] // each flag is a distinct per-peer transport fact
struct Peer {
    id: u64,
    transport: Transport,
    principal: Principal,
    decoder: LineDecoder,
    inbound: VecDeque<WireMessage>,
    /// The raw peer half-closed its write side; what it already sent is still
    /// owed an answer.
    read_closed: bool,
    handshake_deadline: Instant,
    phase: Phase,
    handshake_written: bool,
    session: Option<Session>,
    identity: Option<ServerIdentity>,
    subscription: Option<Subscription>,
    initial_sync: VecDeque<WireMessage>,
    outbound: VecDeque<Outbound>,
    audio_meters: bool,
    stream_status: bool,
    monitor_frames: bool,
    /// The peer's input thumbnail feed while it is subscribed (Protocol
    /// 2.59).
    thumbnails: Option<ThumbnailOutbox>,
    lossy: LossyRecords,
    /// A reply the peer is owed but cannot be queued yet: an accepted
    /// `RecordStart` acknowledgement held until the native runtime has
    /// realized its segment, or any reply (a command result above all) that
    /// met transient backpressure — a full outbound queue or a spent outbound
    /// rate window. A command result is durable and must never be lost, and
    /// backpressure alone must never disconnect the peer, so the reply is
    /// retried every pass. While it is held the peer's later messages and
    /// live events wait behind it, so the peer observes the order it would
    /// have had the reply queued at once; every other peer, and this peer's
    /// lossy side channels, keep being served. At most one reply is ever
    /// held, because dispatch stops while one is. A peer that never drains
    /// is still closed: its heartbeats wait unanswered behind the held reply
    /// and the session's heartbeat timeout expires.
    held_result: Option<HeldResult>,
    /// A live event taken from the subscription that met transient
    /// backpressure; it is queued first on a later pass.
    deferred_live: Option<WireMessage>,
    /// Bytes written to the raw socket so far, so a pass can tell whether
    /// the last write made progress.
    written: u64,
    /// A raw connection beyond [`MAX_PEERS`]: its handshake is refused and
    /// it never becomes a session.
    over_capacity: bool,
}

/// Counts the bytes a raw peer's socket accepts.
struct Counted<'a, W> {
    inner: &'a mut W,
    written: &'a mut u64,
}

impl<W: Write> Write for Counted<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(bytes)?;
        *self.written = self
            .written
            .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// A reply waiting to be queued.
struct HeldResult {
    message: WireMessage,
    /// The recorder slot that must finish realizing first, if any.
    slot: Option<usize>,
}

/// Why a message could not be queued for a peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueRefusal {
    /// The bounded outbound queue is full; retry once the peer drains it.
    Full,
    /// The session's outbound rate window is spent. fm-server documents this
    /// as retryable and never disconnecting.
    RateLimited,
    /// The peer cannot take the message at all and must be closed.
    Fatal,
}

impl QueueRefusal {
    const fn transient(self) -> bool {
        matches!(self, Self::Full | Self::RateLimited)
    }
}

/// One independent latest-wins side-channel slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LossySlot {
    Meter,
    StreamStatus,
    PreviewFrame,
    ProgramFrame,
    /// The latest exact title or scene preview (`title_preview_frame` or
    /// `scene_preview_frame` parts).
    TitlePreview,
    /// The input thumbnail being written (Protocol 2.59); the next one waits
    /// in the peer's [`ThumbnailOutbox`] until this slot is empty.
    Thumbnail,
}

impl LossySlot {
    const COUNT: usize = 6;

    const fn index(self) -> usize {
        match self {
            Self::Meter => 0,
            Self::StreamStatus => 1,
            Self::PreviewFrame => 2,
            Self::ProgramFrame => 3,
            Self::TitlePreview => 4,
            Self::Thumbnail => 5,
        }
    }

    const fn monitor(tap: MonitorTap) -> Self {
        match tap {
            MonitorTap::Preview => Self::PreviewFrame,
            MonitorTap::Program => Self::ProgramFrame,
        }
    }
}

/// Every lossy slot of one raw peer plus the round-robin cursor that drains
/// them.
///
/// At most one slot is ever in flight: a started record is always finished
/// before any other slot (or queued record) writes a byte, so lossy lines —
/// including multi-line monitor frame groups — can never interleave. When no
/// slot is in flight, at most one slot starts per pass, chosen round-robin
/// after the last started slot so a busy slot cannot starve the others.
#[derive(Default)]
struct LossyRecords {
    slots: [LatestRecord; LossySlot::COUNT],
    cursor: usize,
}

impl LossyRecords {
    fn replace(&mut self, slot: LossySlot, bytes: Vec<u8>) {
        self.slots[slot.index()].replace(bytes);
    }

    #[cfg(test)]
    fn slot(&self, slot: LossySlot) -> &LatestRecord {
        &self.slots[slot.index()]
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.slots.iter().all(LatestRecord::is_empty)
    }

    fn started(&self) -> bool {
        self.slots.iter().any(LatestRecord::started)
    }

    fn slot_is_empty(&self, slot: LossySlot) -> bool {
        self.slots[slot.index()].is_empty()
    }

    fn discard_unstarted(&mut self) {
        for slot in &mut self.slots {
            slot.discard_unstarted();
        }
    }

    /// Drops one slot's records that have not started writing.
    fn discard_slot(&mut self, slot: LossySlot) {
        self.slots[slot.index()].discard_unstarted();
    }

    fn write_once(&mut self, writer: &mut impl Write) -> std::io::Result<()> {
        if let Some(record) = self.slots.iter_mut().find(|record| record.started()) {
            return record.write_once(writer);
        }
        for offset in 0..LossySlot::COUNT {
            let index = (self.cursor + offset) % LossySlot::COUNT;
            if !self.slots[index].is_empty() {
                self.cursor = (index + 1) % LossySlot::COUNT;
                return self.slots[index].write_once(writer);
            }
        }
        Ok(())
    }
}

impl Peer {
    fn new(
        id: u64,
        transport: Transport,
        principal: Principal,
        handshake_timeout: Duration,
    ) -> Option<Self> {
        let handshake_deadline = Instant::now().checked_add(handshake_timeout)?;
        Some(Self {
            id,
            transport,
            principal,
            decoder: LineDecoder::new(),
            inbound: VecDeque::new(),
            read_closed: false,
            handshake_deadline,
            phase: Phase::AwaitHandshake,
            handshake_written: false,
            session: None,
            identity: None,
            subscription: None,
            initial_sync: VecDeque::new(),
            outbound: VecDeque::new(),
            audio_meters: false,
            stream_status: false,
            monitor_frames: false,
            thumbnails: None,
            lossy: LossyRecords::default(),
            held_result: None,
            deferred_live: None,
            written: 0,
            over_capacity: false,
        })
    }

    fn queue(&mut self, message: &WireMessage, accounting: Accounting) -> Result<(), QueueRefusal> {
        if self.outbound.len() >= OUTBOUND_CAPACITY {
            return Err(QueueRefusal::Full);
        }
        let bytes = encode_line(message)
            .map_err(|_| QueueRefusal::Fatal)?
            .into_bytes();
        let byte_len = bytes.len();
        let (write, accounted) = match &self.transport {
            Transport::Raw(_) => (OutboundWrite::Raw(PendingWrite::new(bytes)), true),
            Transport::Web(_) => (OutboundWrite::Web(bytes), false),
        };
        if matches!(accounting, Accounting::Session) && accounted {
            let now = now_millis().map_err(|_| QueueRefusal::Fatal)?;
            self.session
                .as_mut()
                .ok_or(QueueRefusal::Fatal)?
                .queue_outbound(byte_len, now)
                .map_err(|error| match error {
                    SessionError::OutboundRateLimited => QueueRefusal::RateLimited,
                    _ => QueueRefusal::Fatal,
                })?;
        }
        self.outbound.push_back(Outbound {
            write,
            accounting,
            handshake_response: matches!(message, WireMessage::HandshakeResponse(_)),
            command_result: matches!(message, WireMessage::CommandResult(_)),
            accounted,
            channel_sent: false,
        });
        Ok(())
    }

    fn feed_initial_sync(&mut self) -> Result<(), ()> {
        while self.outbound.len() < OUTBOUND_CAPACITY {
            let Some(message) = self.initial_sync.pop_front() else {
                break;
            };
            match self.queue(&message, Accounting::Session) {
                Ok(()) => {}
                Err(refusal) if refusal.transient() => {
                    self.initial_sync.push_front(message);
                    break;
                }
                Err(_) => return Err(()),
            }
        }
        Ok(())
    }

    fn close_after(&mut self, message: &WireMessage) -> Result<(), ()> {
        self.queue(message, Accounting::Raw).map_err(|_| ())?;
        self.phase = Phase::Closing;
        Ok(())
    }

    /// Queues the reply to a dispatched message, holding it for a later pass
    /// when transient backpressure refuses it. Only a peer that can never
    /// take the reply is refused.
    fn reply(&mut self, message: WireMessage) -> Result<(), QueueRefusal> {
        debug_assert!(self.held_result.is_none(), "dispatch stops while held");
        match self.queue(&message, Accounting::Session) {
            Err(refusal) if refusal.transient() => {
                self.held_result = Some(HeldResult {
                    message,
                    slot: None,
                });
                Ok(())
            }
            outcome => outcome,
        }
    }

    /// Queues a reply still held when the daemon shuts down. Shutdown no
    /// longer waits for capacity or the rate window, so it makes room by
    /// dropping the newest record that is not a command result and has not
    /// started, and skips rate accounting; the held reply is owed exactly
    /// like a result already queued.
    fn force_held_reply(&mut self) {
        let Some(held) = self.held_result.take() else {
            return;
        };
        self.preserve_outbound_for_shutdown();
        let _ = self.queue(&held.message, Accounting::Raw);
    }

    fn replace_meter(&mut self, bytes: Vec<u8>) {
        if self.audio_meters && self.phase == Phase::Active {
            self.lossy.replace(LossySlot::Meter, bytes);
        }
    }

    /// Coalesces the latest stream status independently of the meter slot so
    /// neither lossy record can evict the other.
    fn replace_stream_status(&mut self, bytes: Vec<u8>) {
        if self.stream_status && self.phase == Phase::Active {
            self.lossy.replace(LossySlot::StreamStatus, bytes);
        }
    }

    /// Coalesces one tap's latest encoded monitor frame group in its own slot.
    fn replace_monitor_frame(&mut self, tap: MonitorTap, bytes: &[u8]) {
        if self.wants_monitor_frames() {
            self.lossy.replace(LossySlot::monitor(tap), bytes.to_vec());
        }
    }

    /// Coalesces the latest exact title or scene preview answer (its
    /// `title_preview_frame` or `scene_preview_frame` parts, or the error its
    /// render met) in its own slot: a newer preview replaces one that has
    /// not started writing. Only raw peers are handed previews to render.
    fn replace_preview(
        &mut self,
        answer: crate::title_preview::PreviewAnswer,
        current_revision: u64,
    ) -> Result<(), QueueRefusal> {
        let mut bytes = Vec::new();
        let messages = match answer.parts {
            Ok(parts) => parts
                .into_iter()
                .map(match answer.kind {
                    crate::title_preview::PreviewKind::Title => WireMessage::TitlePreviewFrame,
                    crate::title_preview::PreviewKind::Scene => WireMessage::ScenePreviewFrame,
                })
                .collect(),
            Err(error) => vec![WireMessage::Error(ErrorMessage {
                request_id: Some(answer.request_id),
                current_revision: Some(current_revision),
                error,
            })],
        };
        for message in &messages {
            let line = encode_line(message).map_err(|_| QueueRefusal::Fatal)?;
            bytes.extend_from_slice(line.as_bytes());
        }
        self.lossy.replace(LossySlot::TitlePreview, bytes);
        Ok(())
    }

    fn wants_monitor_frames(&self) -> bool {
        self.monitor_frames && self.phase == Phase::Active
    }

    fn wants_thumbnails(&self) -> bool {
        self.thumbnails.is_some() && self.phase == Phase::Active
    }

    /// Queues one input's newest thumbnail for a subscribed peer.
    fn offer_thumbnail(&mut self, input: fm_types::InputId, bytes: &[u8]) {
        if self.phase == Phase::Active
            && let Some(outbox) = self.thumbnails.as_mut()
        {
            outbox.offer(input, bytes.to_vec());
        }
    }

    /// Moves the next admitted thumbnail into the lossy slot once the
    /// previous one was fully written.
    fn feed_thumbnail(&mut self, now: Instant) {
        if self.phase != Phase::Active || !self.lossy.slot_is_empty(LossySlot::Thumbnail) {
            return;
        }
        if let Some(bytes) = self.thumbnails.as_mut().and_then(|outbox| outbox.next(now)) {
            self.lossy.replace(LossySlot::Thumbnail, bytes);
        }
    }

    fn replace_outbound_for_shutdown(&mut self) {
        // Records already handed to a Web relay (a prefix of the queue) or
        // partly written to a raw socket are on their way and are kept.
        let retained = self
            .outbound
            .iter()
            .take_while(|record| {
                record.channel_sent
                    || matches!(&record.write, OutboundWrite::Raw(write) if write.started())
            })
            .count();
        while self.outbound.len() > retained {
            self.discard_outbound(self.outbound.len() - 1);
        }
    }

    fn preserve_outbound_for_shutdown(&mut self) {
        if self.outbound.len() < OUTBOUND_CAPACITY {
            return;
        }
        let Some(index) = self
            .outbound
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, record)| {
                (!record.command_result
                    && !record.channel_sent
                    && !matches!(
                        &record.write,
                        OutboundWrite::Raw(write) if write.started()
                    ))
                .then_some(index)
            })
        else {
            return;
        };
        self.discard_outbound(index);
    }

    fn discard_outbound(&mut self, index: usize) {
        if let Some(record) = self.outbound.remove(index)
            && record.accounted
            && matches!(record.accounting, Accounting::Session)
        {
            let _ = self
                .session
                .as_mut()
                .and_then(|session| session.discard_outbound(index).ok());
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
pub(super) fn run(
    listener: &TcpListener,
    server: &Server<ControlHandle>,
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    native: Option<&mut NativeDaemon>,
    authority: &ServerIdentity,
    titles: &Arc<TitleAssets>,
    process_shutdown: &ProcessShutdown,
    once: bool,
    web: Option<&WebGateway>,
) -> AppResult<DaemonShutdownReason> {
    let handshake_timeout =
        Duration::from_millis(server.config().session_limits.heartbeat_timeout_ms);
    let mut peers = Vec::with_capacity(MAX_PEERS + web.map_or(0, |_| MAX_WEB_CLIENTS));
    let mut next_peer_id = 0_u64;
    // Where the next pass starts dispatching, so no peer is always last.
    let mut dispatch_cursor = 0_usize;
    let mut once_peer = None;
    // Exact title previews render off this loop, as simulated monitor
    // frames do.
    let title_previews = crate::title_preview::spawn_preview_worker(Arc::clone(titles))?;
    let runtime = Runtime {
        server,
        authority,
        control,
        store,
        titles,
        title_previews: &title_previews,
        process_shutdown,
        playlist_clock: std::cell::RefCell::new(crate::playlist_clock::PlaylistClock::default()),
        cue_clock: std::cell::RefCell::new(crate::cue_clock::CueClock::default()),
        media_clock: std::cell::RefCell::new(crate::media_clock::MediaClock::default()),
        grade_cache: std::cell::RefCell::new(crate::grading::GradeCache::default()),
        thumbnail_refresh: std::cell::RefCell::new(false),
    };
    let mut native = native;
    // Only the native pacer realizes frames between commands, so only it has
    // idle progress to record.
    let mut progress = native
        .is_some()
        .then(|| crate::NativeProgress::new(durable));
    // Simulated monitor frames are composed off this loop so a slow Scene
    // composition never delays command results, heartbeats, or events.
    // Input thumbnails render off this loop too (Protocol 2.59).
    let mut thumbnails = ThumbnailWorker::spawn(authority.clone(), Arc::clone(titles))?;
    let mut thumbnail_grades = crate::grading::GradeCache::default();
    let mut simulated_monitor = if native.is_none() {
        Some(SimulatedMonitorWorker::spawn(
            authority.clone(),
            Arc::clone(titles),
        )?)
    } else {
        None
    };

    loop {
        if let Some(reason) = requested_daemon_shutdown(native.as_deref(), Some(process_shutdown)) {
            return Ok(shutdown_for_reason(reason, &mut peers, control));
        }

        if let Some(web) = web {
            loop {
                let Ok(Some(event)) = web.try_event() else {
                    break;
                };
                match event {
                    // Every Web client is its own session: its own principal
                    // session id, granted role, heartbeat, subscription, and
                    // outbound queue. The gateway's slots already bound them
                    // to MAX_WEB_CLIENTS; this check only keeps that bound
                    // local to the scheduler too.
                    WebEvent::Connected(connection) if web_peer_count(&peers) < MAX_WEB_CLIENTS => {
                        let web_principal = Principal::authenticated(
                            fm_auth::UserId::new("web-token").expect("stable web user is valid"),
                            fm_auth::SessionId::new(format!("web-{next_peer_id}"))
                                .expect("scheduler web session id is valid"),
                            [fm_auth::Role::Admin],
                        );
                        let transport = Transport::Web(WebTransport {
                            inbound: connection.inbound,
                            outbound: connection.outbound,
                            acknowledgements: connection.acknowledgements,
                            cancel: connection.cancel,
                            _slot: Some(connection.slot),
                        });
                        let Some(peer) = Peer::new(
                            next_peer_id,
                            transport,
                            web_principal,
                            handshake_timeout.min(Duration::from_millis(500)),
                        ) else {
                            close_all(&mut peers, control);
                            return Err("handshake deadline exceeds Instant range".into());
                        };
                        next_peer_id = next_peer_id.wrapping_add(1);
                        peers.push(peer);
                    }
                    WebEvent::Connected(connection) => {
                        connection.cancel.store(true, Ordering::Release);
                    }
                }
            }
        }

        if once_peer.is_none() && (!once || peers.is_empty()) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let over_capacity = raw_peer_count(&peers) >= MAX_PEERS;
                    if over_capacity && refusing_peer_count(&peers) >= MAX_REFUSING_PEERS {
                        // Closed unanswered: the client sees the connection
                        // end now instead of waiting on an unaccepted one.
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                    } else if stream.set_nonblocking(true).is_ok()
                        && stream.set_nodelay(true).is_ok()
                    {
                        let Some(mut peer) = Peer::new(
                            next_peer_id,
                            Transport::Raw(stream),
                            raw_principal(principal, next_peer_id),
                            if over_capacity {
                                handshake_timeout.min(REFUSAL_DEADLINE)
                            } else {
                                handshake_timeout
                            },
                        ) else {
                            close_all(&mut peers, control);
                            return Err("handshake deadline exceeds Instant range".into());
                        };
                        peer.over_capacity = over_capacity;
                        next_peer_id = next_peer_id.wrapping_add(1);
                        peers.push(peer);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => {
                    close_all(&mut peers, control);
                    return Err(error.into());
                }
            }
        }

        let mut close = vec![false; peers.len()];
        for (index, peer) in peers.iter_mut().enumerate() {
            close[index] = read_peer(peer);
            if !close[index]
                && peer.phase == Phase::AwaitHandshake
                && Instant::now() >= peer.handshake_deadline
            {
                close[index] = dropping(peer.id, "handshake_timeout");
            }
            if !close[index]
                && let Some(session) = peer.session.as_mut()
                && !super::session_heartbeat_active(session).unwrap_or(false)
            {
                close[index] = dropping(peer.id, "heartbeat_timeout");
            }
            if !close[index]
                && let Some(failure) = peer.subscription.as_ref().and_then(Subscription::failure)
            {
                close[index] = dropping(
                    peer.id,
                    match failure {
                        SubscriberFailureReason::SlowClient => "subscription_slow_client",
                        SubscriberFailureReason::Disconnected => "subscription_disconnected",
                    },
                );
            }
        }

        let mut live_budget = vec![LIVE_EVENTS_PER_PASS; peers.len()];
        // A native command waits for and realizes the pacer's next frame, so
        // a pass dispatches at most one: the next peer's command waits for
        // the next pass, after every peer was read and written. Throughput
        // stays one command per frame, and no peer's commands delay another
        // peer's heartbeats or records by more than one frame.
        // The command turn goes to the first peer with a command ready after
        // the one that had the last turn, so peers with commands waiting
        // alternate strictly however many idle peers sit between them.
        //
        // A command is made durable (journal append and fsync) before the
        // frame that realizes it, so one dispatched just before a frame
        // deadline delays that frame by its fsync. A command that finds less
        // of the frame interval left than recent appends took waits for the
        // frame to be realized first and takes the next one, keeping its
        // turn: its latency grows by at most a frame, and its fsync no longer
        // makes the frame late (see `NativeDaemon::command_window`).
        let start = dispatch_cursor % peers.len().max(1);
        let ready_turn = native
            .is_some()
            .then(|| command_turn(&peers, &close, start))
            .flatten();
        // A command whose append would queue behind a background checkpoint
        // or progress marker on the journal's worker waits for it too, for
        // the same reason: the journal folds in the background so that no
        // frame waits for the manifest rewrite.
        let command_turn = ready_turn.filter(|_| {
            !store.busy()
                && native
                    .as_deref_mut()
                    .is_some_and(|native| native.command_window(store.recent_append_peak()))
        });
        dispatch_cursor = match (ready_turn, command_turn) {
            (Some(_), None) => start,
            _ => command_turn.unwrap_or(start).wrapping_add(1),
        };
        for offset in 0..peers.len() {
            let index = (start + offset) % peers.len();
            if close[index] || peers[index].held_result.is_some() {
                continue;
            }
            let ready = match peers[index].phase {
                Phase::AwaitHandshake | Phase::Active => peers[index].inbound.front(),
                Phase::Syncing | Phase::Closing => None,
            };
            let Some(ready) = ready else {
                continue;
            };
            if native.is_some()
                && peers[index].phase == Phase::Active
                && matches!(ready, WireMessage::Command(_))
                && command_turn != Some(index)
            {
                continue;
            }
            let Some(message) = peers[index].inbound.pop_front() else {
                continue;
            };
            match runtime.dispatch(&mut peers[index], message, durable, native.as_deref_mut()) {
                Ok(()) => {}
                Err(DispatchError::Peer) => {
                    close[index] = dropping(peers[index].id, "dispatch_refused");
                }
                Err(DispatchError::Daemon(error)) => {
                    shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
                    return Err(error);
                }
            }
        }
        if let Some(reason) = requested_daemon_shutdown(native.as_deref(), Some(process_shutdown)) {
            return Ok(shutdown_for_reason(reason, &mut peers, control));
        }
        // The native command just realized its frame: hand the journal fold
        // to the persistence worker now, so it runs while the loop realizes
        // the next frames. Every mutation is durable already, so a failure is
        // only reported.
        if command_turn.is_some()
            && let Err(error) = store.checkpoint_if_due(durable)
        {
            eprintln!("FREEMIXD_JOURNAL\tv=1\tcheckpoint=deferred\terror={error}");
        }
        let previews = title_previews.take_finished();
        if !previews.is_empty() {
            let current_revision = control.borrow().diagnostics().current_revision;
            for (id, answer) in previews {
                if let Some(index) = peers.iter().position(|peer| peer.id == id)
                    && !close[index]
                    && peers[index]
                        .replace_preview(answer, current_revision)
                        .is_err()
                {
                    close[index] = true;
                }
            }
        }
        // The playlist clock runs before the native frame is realized, so a
        // boundary that is the pacer's next frame is cut by its advance.
        if let Err(error) = runtime.playlist_clock.borrow_mut().poll(
            control,
            store,
            durable,
            authority,
            native.as_deref_mut(),
            Some(process_shutdown),
            Instant::now(),
        ) {
            shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
            return Err(error);
        }
        if let Err(error) = runtime.cue_clock.borrow_mut().poll(
            control,
            store,
            durable,
            authority,
            native.as_deref_mut(),
            Some(process_shutdown),
            Instant::now(),
        ) {
            shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
            return Err(error);
        }
        if native.is_none()
            && let Err(error) = runtime.media_clock.borrow_mut().poll(
                &mut control.borrow_mut(),
                authority,
                durable.project().settings().frame_rate,
                Instant::now(),
            )
        {
            shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
            return Err(error);
        }
        // The control borrow ends before a failure shuts the peers down,
        // which borrows it again.
        let ticked = native
            .as_deref_mut()
            .map(|native| native.tick_if_due(&mut control.borrow_mut(), authority));
        if let Some(Err(error)) = ticked {
            shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
            return Err(error);
        }
        if let Err(error) = crate::media_clock::feed_end_triggers(
            control,
            store,
            durable,
            authority,
            native.as_deref_mut(),
            Some(process_shutdown),
        ) {
            shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
            return Err(error);
        }
        // Display outputs follow the project's slots after every command
        // and frame, and their presenter's state flows back as runtime
        // events (OR-011).
        let synced = native.as_deref_mut().map(|native| {
            native.sync_display_outputs(&mut control.borrow_mut(), authority)?;
            // Network stream receive status flows back the same way
            // (IN-015).
            native.report_network_streams(&mut control.borrow_mut(), authority)
        });
        if let Some(Err(error)) = synced {
            shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
            return Err(error);
        }
        if let Some(native) = native.as_deref_mut() {
            if let Some(progress) = progress.as_mut() {
                progress.poll(control, store, durable);
            }
            native.advance_recordings();
            if let Some(meters) = native.take_audio_meters() {
                publish_audio_meters(&mut peers, meters);
            }
            if let Some(status) = native.take_stream_status() {
                publish_stream_status(&mut peers, status);
            }
        }
        let realizing = native.as_deref();
        release_held_results(&mut peers, &mut close, |slot| {
            realizing.is_some_and(|native| native.recording_settling(slot))
        });
        let monitor_demand = peers.iter().any(Peer::wants_monitor_frames);
        if let Some(native) = native.as_deref_mut() {
            native.set_monitor_demand(monitor_demand);
            if let Some((tap, bytes)) = native.take_monitor_frame() {
                publish_monitor_frame(&mut peers, tap, &bytes);
            }
        } else if let Some(monitor) = simulated_monitor.as_mut() {
            let frames = monitor.poll(&control.borrow(), durable.project(), monitor_demand);
            for (tap, bytes) in frames {
                publish_monitor_frame(&mut peers, tap, &bytes);
            }
        }
        let thumbnail_demand = peers.iter().any(Peer::wants_thumbnails);
        let thumbnail_refresh = runtime.thumbnail_refresh.replace(false);
        let now = Instant::now();
        let records = thumbnails.poll(thumbnail_demand, thumbnail_refresh, now, |worker| {
            let control = control.borrow();
            thumbnail_work(
                worker,
                &control,
                &mut thumbnail_grades,
                durable.project(),
                native.as_deref().map(NativeDaemon::live_source_frames),
            )
        });
        for (input, bytes) in records {
            for peer in &mut peers {
                peer.offer_thumbnail(input, &bytes);
            }
        }
        for peer in &mut peers {
            peer.feed_thumbnail(now);
        }
        if let Some(reason) = requested_daemon_shutdown(native.as_deref(), Some(process_shutdown)) {
            return Ok(shutdown_for_reason(reason, &mut peers, control));
        }
        drain_live(&mut peers, &mut close, &mut live_budget);

        for (index, peer) in peers.iter_mut().enumerate() {
            if close[index] {
                continue;
            }
            if peer.phase == Phase::Syncing && peer.feed_initial_sync().is_err() {
                close[index] = dropping(peer.id, "initial_sync_refused");
                continue;
            }
            match write_peer_pass(peer) {
                WriteOutcome::Pending => {}
                WriteOutcome::HandshakeResponseWritten => {
                    if once && once_peer.is_none() {
                        once_peer = Some(peer.id);
                    }
                }
                WriteOutcome::Failed => close[index] = dropping(peer.id, "write_failed"),
            }
            if peer.phase == Phase::Syncing
                && peer.initial_sync.is_empty()
                && peer.outbound.is_empty()
            {
                peer.phase = Phase::Active;
            }
            if peer.phase == Phase::Closing
                && peer.outbound.is_empty()
                && peer.held_result.is_none()
            {
                close[index] = true;
            }
        }

        for index in (0..peers.len()).rev() {
            if close[index] {
                close_peer(peers.swap_remove(index), control);
            }
        }

        if let Some(id) = once_peer
            && !peers.iter().any(|peer| peer.id == id)
        {
            close_all(&mut peers, control);
            return Ok(DaemonShutdownReason::Once);
        }
        if let Some(reason) = requested_daemon_shutdown(native.as_deref(), Some(process_shutdown)) {
            return Ok(shutdown_for_reason(reason, &mut peers, control));
        }
        thread::sleep(CLIENT_READ_POLL_INTERVAL);
    }
}

/// The work one thumbnail step renders from: the realized revision's
/// project (shared per revision), its compiled grades, the poses of the
/// Scene inputs and titles on air, the media playheads, and a native
/// daemon's newest source frames.
fn thumbnail_work(
    worker: &mut ThumbnailWorker,
    control: &fm_control::ControlService<fm_auth::Policy>,
    grades: &mut crate::grading::GradeCache,
    project: &fm_model::Project,
    live: Option<crate::scene_preview::LiveSourceFrames>,
) -> ThumbnailWork {
    let revision = control.snapshot().snapshot.revision;
    ThumbnailWork {
        revision,
        frame: control.frame_cursor(),
        project: worker.project_at(revision, project),
        grades: grades.at(control),
        scenes: control
            .scenes_on_air()
            .into_iter()
            .map(|scene| (scene.input, scene.frame.sample))
            .collect(),
        titles: control
            .titles_on_air()
            .into_iter()
            .map(|title| (title.input, title.frame.sample))
            .collect(),
        media: control
            .media_statuses()
            .into_iter()
            .map(|media| (media.input, media.position, media.playable_end))
            .collect(),
        live,
    }
}

/// Queues every held reply whose recorder slot (if any) is no longer
/// settling. A reply that still meets transient backpressure stays held for
/// the next pass; only a peer that can never take it is closed.
fn release_held_results(
    peers: &mut [Peer],
    close: &mut [bool],
    mut settling: impl FnMut(usize) -> bool,
) {
    for (index, peer) in peers.iter_mut().enumerate() {
        let Some(held) = peer.held_result.as_ref() else {
            continue;
        };
        if held.slot.is_some_and(&mut settling) {
            continue;
        }
        let held = peer.held_result.take().expect("checked above");
        match peer.queue(&held.message, Accounting::Session) {
            Ok(()) => {}
            Err(refusal) if refusal.transient() => {
                peer.held_result = Some(HeldResult {
                    message: held.message,
                    slot: None,
                });
            }
            Err(_) => close[index] = dropping(peer.id, "reply_refused"),
        }
    }
}

/// The recorder slot an accepted command's acknowledgement may have to wait
/// for: only a `RecordStart` realizes anything before it is acknowledged.
fn recording_start_slot(command: &CommandPayload, result: &CommandResult) -> Option<usize> {
    match (command, result) {
        (CommandPayload::RecordStart { recorder }, CommandResult::Accepted { .. }) => {
            Some(usize::from(recorder.get()) - 1)
        }
        _ => None,
    }
}

fn read_peer(peer: &mut Peer) -> bool {
    if let Transport::Web(web) = &mut peer.transport {
        let budget = INBOUND_CAPACITY.saturating_sub(peer.inbound.len());
        for _ in 0..budget {
            match web.inbound.try_recv() {
                Ok(message) => peer.inbound.push_back(message),
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => return true,
            }
        }
        return false;
    }
    if peer.read_closed {
        return finish_after_read_close(peer);
    }
    // A peer that sends faster than it is served (a native daemon dispatches
    // one command per frame across every peer) is not read until its queue
    // has room, so TCP backpressure slows it instead of the daemon dropping
    // it. Every message one read decodes is kept, so the queue holds at most
    // INBOUND_CAPACITY - 1 messages plus one read's worth of lines.
    if peer.inbound.len() >= INBOUND_CAPACITY {
        return false;
    }
    let mut chunk = [0_u8; 8 * 1024];
    let Transport::Raw(stream) = &mut peer.transport else {
        unreachable!("web transport returned above")
    };
    match stream.read(&mut chunk) {
        Ok(0) => {
            let decoder = std::mem::replace(&mut peer.decoder, LineDecoder::new());
            let _ = decoder.finish();
            peer.read_closed = true;
            finish_after_read_close(peer)
        }
        Ok(read) => match peer.decoder.push(&chunk[..read]) {
            Ok(messages) => {
                peer.inbound.extend(messages);
                false
            }
            Err(_) => dropping(peer.id, "undecodable_inbound"),
        },
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
            ) =>
        {
            false
        }
        Err(_) => true,
    }
}

/// Decides what a half-closed raw peer still needs: every message it sent
/// before the close is dispatched and answered, then the peer closes once its
/// queued answers are written. Returns whether the peer closes now.
fn finish_after_read_close(peer: &mut Peer) -> bool {
    if !peer.inbound.is_empty() || peer.phase == Phase::Syncing || peer.held_result.is_some() {
        return false;
    }
    if peer.phase == Phase::AwaitHandshake || peer.outbound.is_empty() {
        return true;
    }
    peer.phase = Phase::Closing;
    false
}

/// Logs why the daemon drops a peer that did not close its own connection,
/// so a client that sees the connection end can be matched to the policy
/// that ended it. Returns `true`, the peer's close flag.
fn dropping(peer: u64, reason: &str) -> bool {
    eprintln!("FREEMIXD_SESSION\tv=1\tpeer={peer}\tclosed={reason}");
    true
}

/// The peer whose command a native pass dispatches: the first, from
/// `start` round the peer list, that is active, open, holds no reply, and has
/// a command at the front of its queue.
fn command_turn(peers: &[Peer], close: &[bool], start: usize) -> Option<usize> {
    (0..peers.len())
        .map(|offset| (start + offset) % peers.len())
        .find(|&index| {
            let peer = &peers[index];
            !close[index]
                && peer.held_result.is_none()
                && peer.phase == Phase::Active
                && matches!(peer.inbound.front(), Some(WireMessage::Command(_)))
        })
}

fn web_peer_count(peers: &[Peer]) -> usize {
    peers.iter().filter(|peer| peer.transport.is_web()).count()
}

/// Raw sessions counted against [`MAX_PEERS`]; connections being refused
/// are not sessions.
fn raw_peer_count(peers: &[Peer]) -> usize {
    peers
        .iter()
        .filter(|peer| matches!(peer.transport, Transport::Raw(_)) && !peer.over_capacity)
        .count()
}

fn refusing_peer_count(peers: &[Peer]) -> usize {
    peers.iter().filter(|peer| peer.over_capacity).count()
}

/// The principal one raw peer's session runs as: the daemon's principal
/// (the development principal), with a session id of its own. Every raw
/// session is the same local operator, but no two share a session id, as no
/// two Web sessions do.
fn raw_principal(principal: &Principal, peer: u64) -> Principal {
    let Ok(session) = fm_auth::SessionId::new(format!("{}-{peer}", principal.session_id())) else {
        return principal.clone();
    };
    let roles = principal.roles().iter().copied();
    match principal.kind() {
        fm_auth::PrincipalKind::Authenticated => {
            Principal::authenticated(principal.user_id().clone(), session, roles)
        }
        fm_auth::PrincipalKind::DevelopmentOnly => {
            Principal::development(principal.user_id().clone(), session, roles)
        }
    }
}

enum DispatchError {
    Peer,
    Daemon(Box<dyn std::error::Error>),
}

impl From<()> for DispatchError {
    fn from((): ()) -> Self {
        Self::Peer
    }
}

impl From<QueueRefusal> for DispatchError {
    fn from(_: QueueRefusal) -> Self {
        Self::Peer
    }
}

/// One exact preview request a raw peer sent.
enum PreviewRequest {
    Title(fm_protocol::TitlePreviewRequest),
    Scene(fm_protocol::ScenePreviewRequest),
}

impl PreviewRequest {
    fn request_id(&self) -> &str {
        match self {
            Self::Title(request) => &request.request_id,
            Self::Scene(request) => &request.request_id,
        }
    }

    /// The request as it arrived, to measure against the command size cap.
    fn wire(&self) -> WireMessage {
        match self {
            Self::Title(request) => WireMessage::TitlePreviewRequest(request.clone()),
            Self::Scene(request) => WireMessage::ScenePreviewRequest(request.clone()),
        }
    }
}

struct Runtime<'a> {
    server: &'a Server<ControlHandle>,
    /// The daemon identity engine ticks run under.
    authority: &'a ServerIdentity,
    control: &'a SharedControl,
    store: &'a dyn DurableStore,
    /// Templates and fonts for simulated monitor frames and exact title
    /// previews, kept in step with the engine's templates.
    titles: &'a Arc<TitleAssets>,
    /// Renders exact title previews off this loop.
    title_previews: &'a crate::title_preview::TitlePreviewWorker,
    process_shutdown: &'a ProcessShutdown,
    /// The playlist clock; simulated snapshots read its wall-clock frame.
    playlist_clock: std::cell::RefCell<crate::playlist_clock::PlaylistClock>,
    /// The cue clock: runs waiting cue actions on their frames (RC-017).
    cue_clock: std::cell::RefCell<crate::cue_clock::CueClock>,
    /// Simulated runtime: plays media transports in wall time.
    media_clock: std::cell::RefCell<crate::media_clock::MediaClock>,
    /// Compiled grades per engine revision for exact scene previews.
    grade_cache: std::cell::RefCell<crate::grading::GradeCache>,
    /// A peer subscribed to input thumbnails since the last poll, so every
    /// thumbnail is rendered and sent again at once (Protocol 2.59).
    thumbnail_refresh: std::cell::RefCell<bool>,
}

impl Runtime<'_> {
    /// Simulated runtime: the engine's cursor trails the show's wall clock,
    /// so a client's playlist control first catches up to the show's frame
    /// (a Pause then holds the entry where it had played to). The native
    /// pacer's cursor is the show's frame already.
    fn catch_up_playlist(
        &self,
        command: &fm_protocol::CommandMessage,
        durable: &StoredProject,
        simulated: bool,
    ) -> Result<(), DispatchError> {
        let cue = matches!(command.payload, CommandPayload::CueControl { .. });
        // A script run may start or stop the playlist too (RC-011).
        let playlist = matches!(
            command.payload,
            CommandPayload::PlaylistControl { .. } | CommandPayload::RunScript { .. }
        );
        if !simulated || !(cue || playlist) {
            return Ok(());
        }
        let rate = durable.project().settings().frame_rate;
        let now = Instant::now();
        // A cue control catches up to a waiting cue's wall-clock frame as a
        // playlist control does to the playlist's (RC-017); a cue may also
        // start or stop the playlist, so it honours both.
        if cue {
            self.cue_clock
                .borrow()
                .catch_up(&mut self.control.borrow_mut(), self.authority, rate, now)
                .map_err(DispatchError::Daemon)?;
        }
        self.playlist_clock
            .borrow()
            .catch_up(&mut self.control.borrow_mut(), self.authority, rate, now)
            .map_err(DispatchError::Daemon)
    }

    #[allow(clippy::too_many_lines)] // one match over every client message
    fn dispatch(
        &self,
        peer: &mut Peer,
        message: WireMessage,
        durable: &mut StoredProject,
        native: Option<&mut NativeDaemon>,
    ) -> Result<(), DispatchError> {
        if peer.phase == Phase::AwaitHandshake {
            return self.handshake(peer, message, durable);
        }
        let mut native = native;
        match message {
            WireMessage::Command(command) => {
                self.catch_up_playlist(&command, durable, native.is_none())?;
                let delivery = execute_session_command(
                    peer.session.as_mut().expect("active peers have sessions"),
                    self.control,
                    self.store,
                    durable,
                    &peer.principal,
                    peer.identity
                        .as_ref()
                        .expect("active peers have identities"),
                    &command,
                    native.as_deref_mut(),
                    Some(self.process_shutdown),
                )
                .map_err(|error| {
                    if is_client_session_termination(error.as_ref()) {
                        DispatchError::Peer
                    } else {
                        DispatchError::Daemon(error)
                    }
                })?;
                self.sync_title_templates();
                let CommandDelivery { result, .. } = delivery;
                if let Some(slot) = recording_start_slot(&command.payload, &result)
                    && native
                        .as_deref()
                        .is_some_and(|native| native.recording_settling(slot))
                {
                    peer.held_result = Some(HeldResult {
                        message: WireMessage::CommandResult(result),
                        slot: Some(slot),
                    });
                    return Ok(());
                }
                peer.reply(WireMessage::CommandResult(result))?;
                Ok(())
            }
            WireMessage::Heartbeat(heartbeat) => {
                let identity = peer
                    .identity
                    .as_ref()
                    .expect("active peers have identities");
                let message = match record_heartbeat(
                    peer.session.as_mut().expect("active peers have sessions"),
                    self.control,
                    identity,
                    &heartbeat,
                ) {
                    Ok(received_at_ms) => {
                        WireMessage::HeartbeatAcknowledgement(HeartbeatAcknowledgementMessage {
                            server: identity.clone(),
                            heartbeat_sequence: heartbeat.sequence,
                            received_at_ms,
                        })
                    }
                    Err(message) => error_message("invalid_heartbeat", &message),
                };
                peer.reply(message)?;
                Ok(())
            }
            WireMessage::DiagnosticsRequest(request) => {
                let encoded_bytes = encode_line(&WireMessage::DiagnosticsRequest(request.clone()))
                    .map_err(|_| ())?
                    .len();
                let session = peer.session.as_mut().expect("active peers have sessions");
                let response = match session.admit_diagnostics(
                    &request,
                    encoded_bytes,
                    now_millis().map_err(|_| ())?,
                ) {
                    Ok(()) => WireMessage::DiagnosticsResponse(
                        diagnostics_response(self.control, request).map_err(|_| ())?,
                    ),
                    Err(error) => WireMessage::Error(ErrorMessage {
                        request_id: Some(request.request_id),
                        current_revision: Some(
                            self.control.borrow().diagnostics().current_revision,
                        ),
                        error: structured_session_error(&error),
                    }),
                };
                peer.reply(response)?;
                Ok(())
            }
            // The input thumbnail feed (Protocol 2.59) goes to the peers
            // monitor frames go to; others are told it is unavailable.
            WireMessage::InputThumbnailSubscription(subscription) => {
                if !peer.monitor_frames {
                    peer.reply(error_message(
                        "input_thumbnails_unavailable",
                        "input thumbnails go only to raw Studio sessions with view_status",
                    ))?;
                } else if !subscription.enabled {
                    peer.thumbnails = None;
                    peer.lossy.discard_slot(LossySlot::Thumbnail);
                } else if peer.thumbnails.is_none() {
                    peer.thumbnails = Some(ThumbnailOutbox::new(Instant::now()));
                    *self.thumbnail_refresh.borrow_mut() = true;
                }
                Ok(())
            }
            message @ (WireMessage::TitlePreviewRequest(_)
            | WireMessage::ScenePreviewRequest(_)) => {
                self.exact_preview(peer, message, durable, native.as_deref())
            }
            // The presenter's last inventory (OR-011); a daemon without one
            // answers that it has no presenter.
            WireMessage::DisplayInventoryRequest(request) => {
                let inventory = self.control.borrow().display_inventory().clone();
                peer.reply(WireMessage::DisplayInventory(
                    fm_protocol::DisplayInventoryMessage {
                        request_id: request.request_id,
                        inventory,
                    },
                ))?;
                Ok(())
            }
            // The media plane's last capture device inventory (IN-001); a
            // daemon without capture answers with no backend and no devices.
            WireMessage::CaptureDevicesRequest(request) => {
                let inventory = self.control.borrow().capture_inventory().clone();
                peer.reply(WireMessage::CaptureDevices(
                    fm_protocol::CaptureDevicesMessage {
                        request_id: request.request_id,
                        inventory,
                    },
                ))?;
                Ok(())
            }
            _ => {
                peer.reply(error_message(
                    "unexpected_message",
                    "only command and heartbeat messages are accepted after the handshake",
                ))?;
                Ok(())
            }
        }
    }

    /// Hands the engine's title templates to the title renderers: an
    /// accepted animation edit changes what titles draw.
    fn sync_title_templates(&self) {
        self.titles
            .sync_templates(&self.control.borrow().title_templates());
    }

    /// Admits and resolves one exact title or scene preview request and
    /// hands it to the preview worker (see [`crate::title_preview`] and
    /// [`crate::scene_preview`]), which renders it off this loop. A refused
    /// preview is answered with an error naming its `request_id`; only raw
    /// peers have the lossy slot previews use. A native daemon's scene
    /// previews compose from a snapshot of its newest source frames.
    fn exact_preview(
        &self,
        peer: &mut Peer,
        message: WireMessage,
        durable: &StoredProject,
        native: Option<&NativeDaemon>,
    ) -> Result<(), DispatchError> {
        let request = &match message {
            WireMessage::TitlePreviewRequest(request) => PreviewRequest::Title(request),
            WireMessage::ScenePreviewRequest(request) => PreviewRequest::Scene(request),
            _ => return Ok(()),
        };
        let request_id = request.request_id().to_owned();
        if peer.transport.is_web() {
            peer.reply(WireMessage::Error(ErrorMessage {
                request_id: Some(request_id),
                current_revision: None,
                error: StructuredError {
                    code: "unsupported".into(),
                    message: "exact previews are served to raw protocol sessions only".into(),
                    fields: Vec::new(),
                    retryable: false,
                },
            }))?;
            return Ok(());
        }
        let encoded_bytes = encode_line(&request.wire()).map_err(|_| ())?.len();
        let session = peer.session.as_mut().expect("active peers have sessions");
        let now = now_millis().map_err(|_| ())?;
        let admitted = match request {
            PreviewRequest::Title(request) => {
                session.admit_title_preview(request, encoded_bytes, now)
            }
            PreviewRequest::Scene(request) => {
                session.admit_scene_preview(request, encoded_bytes, now)
            }
        };
        let server = peer
            .identity
            .as_ref()
            .expect("active peers have identities");
        let prepared = admitted
            .map_err(|error| structured_session_error(&error))
            .and_then(|()| {
                let control = self.control.borrow();
                match request {
                    PreviewRequest::Title(request) => crate::title_preview::prepare_preview(
                        &control,
                        durable.project(),
                        server,
                        request,
                    )
                    .map(|job| crate::title_preview::PreviewWork::Title(Box::new(job))),
                    PreviewRequest::Scene(request) => crate::scene_preview::prepare_scene_preview(
                        &control,
                        durable.project(),
                        server,
                        request,
                        self.grade_cache.borrow_mut().at(&control),
                        native.map(NativeDaemon::live_source_frames),
                    )
                    .map(|job| crate::title_preview::PreviewWork::Scene(Box::new(job))),
                }
            });
        match prepared {
            Ok(job) => self.title_previews.submit(peer.id, job),
            Err(error) => peer.reply(WireMessage::Error(ErrorMessage {
                request_id: Some(request_id),
                current_revision: Some(self.control.borrow().diagnostics().current_revision),
                error,
            }))?,
        }
        Ok(())
    }

    /// Completes a queued snapshot with one `title_state` per title input,
    /// in input order, then the `playlist_state` and the `grade_state`
    /// records it announced, then one `title_animation_state` and then one
    /// `title_layout_state` per title input, the scene animation and layout
    /// states, and the `animation_preset_state` records, before anything
    /// else reaches the peer. The playlist state's frame is the
    /// clock's wall-clock frame: the simulated engine renders only while it
    /// executes commands, so its cursor lags wall time mid-entry. The native
    /// pacer realizes every frame, so there it is the engine cursor itself.
    fn complete_snapshot(&self, peer: &mut Peer) {
        if matches!(peer.initial_sync.front(), Some(WireMessage::Snapshot(_))) {
            let control = self.control.borrow();
            peer.initial_sync.extend(
                control
                    .title_states()
                    .into_iter()
                    .map(WireMessage::TitleState),
            );
            let rate = control.snapshot().snapshot.frame_rate;
            peer.initial_sync
                .extend(control.playlist_state().map(|mut state| {
                    state.frame = self.playlist_clock.borrow().observed_frame(
                        state.frame,
                        rate,
                        Instant::now(),
                    );
                    WireMessage::PlaylistState(state)
                }));
            peer.initial_sync.extend(
                control
                    .grade_states()
                    .into_iter()
                    .map(WireMessage::GradeState),
            );
            peer.initial_sync.extend(
                control
                    .title_animation_states()
                    .into_iter()
                    .map(WireMessage::TitleAnimationState),
            );
            peer.initial_sync.extend(
                control
                    .title_layout_states()
                    .into_iter()
                    .map(WireMessage::TitleLayoutState),
            );
            peer.initial_sync.extend(
                control
                    .scene_animation_states()
                    .into_iter()
                    .map(WireMessage::SceneAnimationState),
            );
            peer.initial_sync.extend(
                control
                    .scene_layout_states()
                    .into_iter()
                    .map(WireMessage::SceneLayoutState),
            );
            peer.initial_sync.extend(
                control
                    .animation_preset_states()
                    .into_iter()
                    .map(WireMessage::AnimationPresetState),
            );
            peer.initial_sync.extend(
                control
                    .media_states()
                    .into_iter()
                    .map(WireMessage::MediaState),
            );
            // A waiting cue's countdown reads the show's wall-clock frame on
            // the simulated daemon, as the playlist state does.
            peer.initial_sync
                .extend(control.cue_list_states().into_iter().map(|mut state| {
                    state.frame =
                        self.cue_clock
                            .borrow()
                            .observed_frame(state.frame, rate, Instant::now());
                    WireMessage::CueListState(state)
                }));
            peer.initial_sync.extend(
                control
                    .script_states()
                    .into_iter()
                    .map(WireMessage::ScriptState),
            );
        }
    }

    /// Answers a raw connection beyond [`MAX_PEERS`] with a retryable
    /// `resource_exhausted` rejection and closes it once that is written.
    fn refuse_over_capacity(
        &self,
        peer: &mut Peer,
        hello: &fm_protocol::HandshakeRequest,
        project_id: fm_types::ProjectId,
    ) -> Result<(), DispatchError> {
        let mut response = rejected_handshake_response(
            self.server,
            self.control,
            project_id,
            hello,
            "resource_exhausted",
            &format!(
                "the daemon already serves {MAX_PEERS} raw control sessions; retry once one leaves"
            ),
        );
        make_retryable(&mut response);
        peer.close_after(&WireMessage::HandshakeResponse(response))?;
        Ok(())
    }

    fn handshake(
        &self,
        peer: &mut Peer,
        message: WireMessage,
        durable: &StoredProject,
    ) -> Result<(), DispatchError> {
        let WireMessage::HandshakeRequest(request) = message else {
            peer.close_after(&error_message(
                "handshake_required",
                "first message must be handshake_request",
            ))?;
            return Ok(());
        };
        let project_id = durable.project().id();
        let (hello, outcome) = current_handshake(&request, self.control, project_id);
        if peer.over_capacity {
            return self.refuse_over_capacity(peer, &hello, project_id);
        }
        let handshake = match self.server.handshake(
            &hello,
            &peer.principal,
            now_millis().map_err(DispatchError::Daemon)?,
        ) {
            Ok(handshake) => handshake,
            Err(error) => {
                let response = rejected_handshake_response(
                    self.server,
                    self.control,
                    project_id,
                    &hello,
                    handshake_code(&error),
                    &error.to_string(),
                );
                peer.close_after(&WireMessage::HandshakeResponse(response))?;
                return Ok(());
            }
        };

        let subscription = match self.control.borrow_mut().subscribe() {
            Ok(subscription) => subscription,
            Err(error) => {
                let mut response = rejected_handshake_response(
                    self.server,
                    self.control,
                    project_id,
                    &hello,
                    "unavailable",
                    &error.to_string(),
                );
                make_retryable(&mut response);
                peer.close_after(&WireMessage::HandshakeResponse(response))?;
                return Ok(());
            }
        };

        let identity = server_identity(&handshake.server_hello, project_id);
        let response = handshake_response(
            &handshake.server_hello,
            identity.clone(),
            reconciled_handshake_outcome(outcome, &handshake.sync),
        );
        match handshake.sync {
            SyncPayload::Snapshot(snapshot) => peer
                .initial_sync
                .push_back(WireMessage::Snapshot(*snapshot)),
            SyncPayload::Resume(events) => peer
                .initial_sync
                .extend(events.into_iter().map(WireMessage::Event)),
        }
        let sync_limit = self
            .control
            .borrow()
            .diagnostics()
            .limits
            .retained_events
            .max(1);
        if peer.initial_sync.len() > sync_limit {
            self.control.borrow_mut().unsubscribe(subscription.id());
            return Err(DispatchError::Peer);
        }
        self.complete_snapshot(peer);
        peer.session = Some(handshake.session);
        peer.identity = Some(identity);
        peer.subscription = Some(subscription);
        peer.audio_meters = !peer.transport.is_web() && request.client_type == ClientType::Studio;
        peer.stream_status = peer.audio_meters;
        peer.monitor_frames = monitor_frames_eligible(
            peer.transport.is_web(),
            request.client_type,
            &handshake.server_hello.permissions,
        );
        if peer
            .queue(
                &WireMessage::HandshakeResponse(response),
                Accounting::Session,
            )
            .is_err()
        {
            let subscription = peer.subscription.take().expect("set above");
            self.control.borrow_mut().unsubscribe(subscription.id());
            return Err(DispatchError::Peer);
        }
        peer.phase = Phase::Syncing;
        Ok(())
    }
}

/// Monitor frames go only to raw (non-Web) Studio peers whose granted role
/// carries `view_status`.
fn monitor_frames_eligible(web: bool, client_type: ClientType, permissions: &[String]) -> bool {
    !web && client_type == ClientType::Studio
        && permissions
            .iter()
            .any(|permission| permission == VIEW_STATUS_PERMISSION)
}

fn make_retryable(response: &mut HandshakeResponse) {
    if let ProtocolHandshakeOutcome::Rejected {
        error: StructuredError { retryable, .. },
    } = &mut response.outcome
    {
        *retryable = true;
    }
}

fn drain_live(peers: &mut [Peer], close: &mut [bool], budgets: &mut [usize]) {
    for (index, peer) in peers.iter_mut().enumerate() {
        // A held acknowledgement precedes the events its command published.
        if close[index] || peer.phase != Phase::Active || peer.held_result.is_some() {
            continue;
        }
        while budgets[index] > 0 && peer.outbound.len() < OUTBOUND_CAPACITY - REPLY_RESERVE {
            let message = if let Some(message) = peer.deferred_live.take() {
                message
            } else {
                match peer
                    .subscription
                    .as_ref()
                    .expect("active peers have subscriptions")
                    .try_recv()
                {
                    Ok(LiveEvent::Durable(event)) => WireMessage::Event(event),
                    Ok(LiveEvent::Runtime(event)) => WireMessage::RuntimeEvent(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        close[index] = true;
                        break;
                    }
                }
            };
            match peer.queue(&message, Accounting::Session) {
                Ok(()) => {}
                // Bounded: the subscription keeps (and bounds) every later
                // event while this one waits.
                Err(refusal) if refusal.transient() => {
                    peer.deferred_live = Some(message);
                    break;
                }
                Err(_) => {
                    close[index] = dropping(peer.id, "live_event_refused");
                    break;
                }
            }
            budgets[index] -= 1;
        }
    }
}

enum WriteOutcome {
    Pending,
    HandshakeResponseWritten,
    Failed,
}

/// Serves a Web peer's queue for one pass: retires every record the relay
/// acknowledged, then hands it every queued record not yet handed over, in
/// order, until the relay channel is full or the session's outbound rate
/// window is spent.
///
/// Records are pipelined: the relay writes them while the scheduler keeps
/// queueing, so a Web peer's throughput is the relay's, not one record per
/// relay poll. Handed-over records stay at the front of the queue (they still
/// count against its bound) until acknowledged, in order, so the queue never
/// holds more records in flight than the relay channel and its
/// acknowledgement channel can take.
fn write_web_peer(peer: &mut Peer) -> WriteOutcome {
    let Peer {
        transport,
        outbound,
        session,
        handshake_written,
        ..
    } = peer;
    let Transport::Web(web) = transport else {
        unreachable!("only Web peers are written here")
    };
    let mut handshake = false;
    loop {
        match web.acknowledgements.try_recv() {
            Ok(()) => {
                // An acknowledgement always retires the oldest handed-over
                // record; one for a record never handed over is a relay bug.
                if !outbound.front().is_some_and(|record| record.channel_sent) {
                    return WriteOutcome::Failed;
                }
                let record = outbound.pop_front().expect("checked above");
                if record.accounted
                    && matches!(record.accounting, Accounting::Session)
                    && session
                        .as_mut()
                        .expect("accounted records have sessions")
                        .outbound_delivered()
                        .is_err()
                {
                    return WriteOutcome::Failed;
                }
                if record.handshake_response {
                    *handshake_written = true;
                    handshake = true;
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return WriteOutcome::Failed,
        }
    }
    for record in outbound.iter_mut().filter(|record| !record.channel_sent) {
        let OutboundWrite::Web(bytes) = &record.write else {
            unreachable!("web peers only queue web writes")
        };
        if matches!(record.accounting, Accounting::Session) && !record.accounted {
            let Ok(now) = now_millis() else {
                return WriteOutcome::Failed;
            };
            match session
                .as_mut()
                .expect("session accounting has a session")
                .queue_outbound(bytes.len(), now)
            {
                Ok(()) => {}
                // Retryable and never disconnecting: send on a later pass.
                Err(SessionError::OutboundRateLimited) => break,
                Err(_) => return WriteOutcome::Failed,
            }
            record.accounted = true;
        }
        match web.outbound.try_send(bytes.clone()) {
            Ok(()) => record.channel_sent = true,
            // The relay has not taken earlier records yet; later passes
            // hand this one over in order.
            Err(TrySendError::Full(_)) => break,
            Err(TrySendError::Disconnected(_)) => return WriteOutcome::Failed,
        }
    }
    if handshake {
        WriteOutcome::HandshakeResponseWritten
    } else {
        WriteOutcome::Pending
    }
}

fn write_peer(peer: &mut Peer) -> WriteOutcome {
    if peer.transport.is_web() {
        write_web_peer(peer)
    } else {
        // Continue any in-flight lossy record before touching the queue; an
        // idle queue starts at most one lossy slot per pass.
        if peer.lossy.started() {
            return write_lossy(peer);
        }
        let Some(record) = peer.outbound.front_mut() else {
            return write_lossy(peer);
        };
        let Transport::Raw(stream) = &mut peer.transport else {
            unreachable!("raw transport checked above")
        };
        let mut stream = Counted {
            inner: stream,
            written: &mut peer.written,
        };
        let complete = match &mut record.write {
            OutboundWrite::Raw(write) => write.write_once(&mut stream),
            OutboundWrite::Web(_) => unreachable!("raw peers only queue raw writes"),
        };
        match complete {
            Ok(true) => finish_outbound(peer),
            Ok(false) => WriteOutcome::Pending,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) =>
            {
                WriteOutcome::Pending
            }
            Err(_) => WriteOutcome::Failed,
        }
    }
}

/// Writes `peer` for one loop pass: a web peer once (see
/// [`write_web_peer`]); a raw
/// peer record after record (queued records first, then the lossy slots,
/// each started record finished before another starts) until its socket
/// would block, nothing is left, or [`WRITE_BUDGET_PER_PASS`] is spent.
///
/// A pass also ends as soon as the handshake response is written, so the
/// caller always observes [`WriteOutcome::HandshakeResponseWritten`]: a later
/// write failing in the same pass must not hide it, or a `--once` daemon never
/// learns its one peer and keeps waiting for another after that peer closes.
fn write_peer_pass(peer: &mut Peer) -> WriteOutcome {
    let started = peer.written;
    loop {
        let before = peer.written;
        match write_peer(peer) {
            WriteOutcome::Pending => {}
            outcome @ (WriteOutcome::HandshakeResponseWritten | WriteOutcome::Failed) => {
                return outcome;
            }
        }
        if peer.transport.is_web()
            || peer.written == before
            || peer.written - started >= WRITE_BUDGET_PER_PASS
        {
            return WriteOutcome::Pending;
        }
    }
}

fn finish_outbound(peer: &mut Peer) -> WriteOutcome {
    let record = peer.outbound.pop_front().expect("written record exists");
    if record.accounted
        && matches!(record.accounting, Accounting::Session)
        && peer
            .session
            .as_mut()
            .expect("accounted records have sessions")
            .outbound_delivered()
            .is_err()
    {
        return WriteOutcome::Failed;
    }
    if record.handshake_response {
        peer.handshake_written = true;
        WriteOutcome::HandshakeResponseWritten
    } else {
        WriteOutcome::Pending
    }
}

fn write_lossy(peer: &mut Peer) -> WriteOutcome {
    let Transport::Raw(stream) = &mut peer.transport else {
        return WriteOutcome::Failed;
    };
    let mut stream = Counted {
        inner: stream,
        written: &mut peer.written,
    };
    match peer.lossy.write_once(&mut stream) {
        Ok(()) => WriteOutcome::Pending,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
            ) =>
        {
            WriteOutcome::Pending
        }
        Err(_) => WriteOutcome::Failed,
    }
}

fn publish_audio_meters(peers: &mut [Peer], meters: AudioMetersMessage) {
    let Ok(bytes) = encode_line(&WireMessage::AudioMeters(meters)).map(String::into_bytes) else {
        return;
    };
    for peer in peers {
        peer.replace_meter(bytes.clone());
    }
}

fn publish_stream_status(peers: &mut [Peer], status: StreamStatusMessage) {
    let Ok(bytes) = encode_line(&WireMessage::StreamStatus(status)).map(String::into_bytes) else {
        return;
    };
    for peer in peers {
        peer.replace_stream_status(bytes.clone());
    }
}

/// Publishes one tap's atomic encoded frame group to every eligible peer.
fn publish_monitor_frame(peers: &mut [Peer], tap: MonitorTap, bytes: &[u8]) {
    for peer in peers {
        peer.replace_monitor_frame(tap, bytes);
    }
}

/// Drops lossy records that never reached the wire so shutdown cannot flush
/// stale telemetry or monitor frames after its goodbye; in-flight records keep
/// draining.
fn discard_unstarted_lossy_records(peer: &mut Peer) {
    peer.lossy.discard_unstarted();
}

enum ShutdownQueue {
    Replace,
    PreserveCommandResult,
}

fn shutdown_peers(peers: &mut Vec<Peer>, control: &SharedControl, queue: &ShutdownQueue) {
    for index in (0..peers.len()).rev() {
        if peers[index].session.is_none() || !peers[index].handshake_written {
            close_peer(peers.swap_remove(index), control);
        }
    }
    if peers.is_empty() {
        return;
    }
    for peer in peers.iter_mut() {
        discard_unstarted_lossy_records(peer);
        if matches!(queue, ShutdownQueue::Replace) {
            peer.replace_outbound_for_shutdown();
        } else {
            peer.preserve_outbound_for_shutdown();
        }
        peer.initial_sync.clear();
        peer.inbound.clear();
        let notice = shutdown_message();
        if peer.queue(&notice, Accounting::Session).is_err() {
            let _ = peer.queue(&notice, Accounting::Raw);
        }
        peer.phase = Phase::Closing;
    }

    let deadline = Instant::now() + CLIENT_WRITE_TIMEOUT;
    while !peers.is_empty() && Instant::now() < deadline {
        for index in (0..peers.len()).rev() {
            let outcome = write_peer(&mut peers[index]);
            let complete = if peers[index].transport.is_web() {
                matches!(outcome, WriteOutcome::Failed) || peers[index].outbound.is_empty()
            } else {
                !matches!(outcome, WriteOutcome::Pending) || peers[index].outbound.is_empty()
            };
            if complete {
                close_peer(peers.swap_remove(index), control);
            }
        }
        if !peers.is_empty() {
            thread::sleep(CLIENT_READ_POLL_INTERVAL);
        }
    }
    close_all(peers, control);
}

fn close_all(peers: &mut Vec<Peer>, control: &SharedControl) {
    while let Some(peer) = peers.pop() {
        close_peer(peer, control);
    }
}

fn shutdown_for_reason(
    reason: DaemonShutdownReason,
    peers: &mut Vec<Peer>,
    control: &SharedControl,
) -> DaemonShutdownReason {
    // Shutdown stops waiting for realization: a held acknowledgement is owed
    // to its peer exactly like any other queued result.
    let mut close = vec![false; peers.len()];
    release_held_results(peers, &mut close, |_| false);
    for peer in peers.iter_mut() {
        peer.force_held_reply();
    }
    match reason {
        DaemonShutdownReason::ProcessSignal => {
            let queue = if peers
                .iter()
                .any(|peer| peer.outbound.iter().any(|record| record.command_result))
            {
                ShutdownQueue::PreserveCommandResult
            } else {
                ShutdownQueue::Replace
            };
            shutdown_peers(peers, control, &queue);
        }
        DaemonShutdownReason::ProgramSurface => close_all(peers, control),
        DaemonShutdownReason::Once => unreachable!("once is handled below"),
    }
    reason
}

fn close_peer(mut peer: Peer, control: &SharedControl) {
    if let Some(subscription) = peer.subscription.take() {
        control.borrow_mut().unsubscribe(subscription.id());
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;
    use std::sync::mpsc;

    use fm_auth::{Role, SessionId, UserId};
    use fm_protocol::{
        AudioMeterChannel, StreamRealizedState, StreamStatusMessage, StreamStatusSample,
        WireStreamTargetId,
    };

    use super::*;

    fn identity() -> ServerIdentity {
        ServerIdentity {
            engine_id: "engine".to_owned(),
            project_id: "project".to_owned(),
            state_epoch: 1,
            log_id: "log".to_owned(),
        }
    }

    fn meters_message() -> AudioMetersMessage {
        AudioMetersMessage {
            server: identity(),
            sequence: 7,
            frame: 9,
            start_sample: 100,
            end_sample: 200,
            master: vec![AudioMeterChannel {
                peak_millionths: 500_000,
                rms_millionths: 250_000,
            }],
            inputs: Vec::new(),
            monitor: None,
        }
    }

    fn status_message() -> StreamStatusMessage {
        StreamStatusMessage {
            server: identity(),
            sequence: 1,
            samples: vec![StreamStatusSample {
                target: WireStreamTargetId::new(NonZeroU128::new(5).unwrap()),
                realized: StreamRealizedState::Live,
                connected: true,
                muxed_bytes: 2_048,
                enqueued_pairs: 12,
                dropped_pairs: 1,
                reconnects: 2,
                uptime_ms: 4_000,
                bitrate_kbps: 4_500,
                failure: None,
                local: None,
            }],
        }
    }

    /// An eligible active raw peer plus the far end of its transport.
    fn raw_peer() -> (Peer, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        stream.set_nonblocking(true).unwrap();
        let principal = Principal::authenticated(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            [Role::Admin],
        );
        let peer = Peer::new(0, Transport::Raw(stream), principal, Duration::from_secs(5)).unwrap();
        (peer, client)
    }

    fn active_eligible_peer() -> (Peer, TcpStream) {
        let (mut peer, client) = raw_peer();
        peer.phase = Phase::Active;
        peer.audio_meters = true;
        peer.stream_status = true;
        peer.monitor_frames = true;
        (peer, client)
    }

    fn read_exact(client: &mut TcpStream, length: usize) -> Vec<u8> {
        let mut buffer = vec![0_u8; length];
        client.read_exact(&mut buffer).unwrap();
        buffer
    }

    #[test]
    fn stream_status_and_meter_records_coexist_and_drain_independently() {
        let (mut peer, mut client) = active_eligible_peer();

        publish_audio_meters(std::slice::from_mut(&mut peer), meters_message());
        publish_stream_status(std::slice::from_mut(&mut peer), status_message());
        // Both slots hold their own encoded record; neither evicted the other.
        assert!(!peer.lossy.slot(LossySlot::Meter).is_empty());
        assert!(!peer.lossy.slot(LossySlot::StreamStatus).is_empty());

        // A newer stream status replaces only the stream status slot.
        publish_stream_status(std::slice::from_mut(&mut peer), status_message());
        assert!(!peer.lossy.slot(LossySlot::Meter).is_empty());
        assert!(!peer.lossy.slot(LossySlot::StreamStatus).is_empty());

        let expected_meters = encode_line(&WireMessage::AudioMeters(meters_message()))
            .unwrap()
            .into_bytes();
        let expected_status = encode_line(&WireMessage::StreamStatus(status_message()))
            .unwrap()
            .into_bytes();

        while !peer.lossy.slot(LossySlot::Meter).is_empty()
            || !peer.lossy.slot(LossySlot::StreamStatus).is_empty()
        {
            assert!(!matches!(write_peer(&mut peer), WriteOutcome::Failed));
        }
        assert_eq!(
            read_exact(&mut client, expected_meters.len()),
            expected_meters
        );
        assert_eq!(
            read_exact(&mut client, expected_status.len()),
            expected_status
        );

        // The drained line is a well-formed stream_status record.
        let decoded = LineDecoder::new().push(&expected_status).unwrap().remove(0);
        assert!(matches!(decoded, WireMessage::StreamStatus(_)));
    }

    #[test]
    fn ineligible_peers_never_store_or_write_stream_status() {
        // A raw peer that did not opt in keeps both lossy slots empty.
        let (mut peer, mut client) = raw_peer();
        peer.phase = Phase::Active;
        peer.audio_meters = false;
        peer.stream_status = false;
        publish_stream_status(std::slice::from_mut(&mut peer), status_message());
        assert!(peer.lossy.slot(LossySlot::StreamStatus).is_empty());
        write_peer(&mut peer);
        client.set_nonblocking(true).unwrap();
        let mut probe = [0_u8; 16];
        assert!(matches!(
            client.read(&mut probe),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));

        // Web transports are excluded from the lossy side channel entirely.
        let (inbound_sender, inbound) = mpsc::channel();
        let (outbound_sender, _outbound) = mpsc::sync_channel(8);
        let (_acknowledgement_sender, acknowledgements) = mpsc::channel();
        drop(inbound_sender);
        let web_peer = Peer::new(
            1,
            Transport::Web(WebTransport {
                inbound,
                outbound: outbound_sender,
                acknowledgements,
                cancel: Arc::new(AtomicBool::new(false)),
                _slot: None,
            }),
            Principal::authenticated(
                UserId::new("user").unwrap(),
                SessionId::new("session").unwrap(),
                [Role::Admin],
            ),
            Duration::from_secs(5),
        )
        .unwrap();
        let mut web_peer = web_peer;
        web_peer.phase = Phase::Active;
        // Eligibility is derived once at handshake, exactly as in `handshake`.
        let client_type = ClientType::Studio;
        web_peer.audio_meters = !web_peer.transport.is_web() && client_type == ClientType::Studio;
        web_peer.stream_status = web_peer.audio_meters;
        assert!(
            !web_peer.stream_status,
            "web peers are not lossy-record eligible"
        );
        publish_stream_status(std::slice::from_mut(&mut web_peer), status_message());
        assert!(web_peer.lossy.slot(LossySlot::StreamStatus).is_empty());
    }

    #[test]
    fn unstarted_lossy_records_are_discarded_for_shutdown_together() {
        let (mut peer, _client) = active_eligible_peer();
        publish_audio_meters(std::slice::from_mut(&mut peer), meters_message());
        publish_stream_status(std::slice::from_mut(&mut peer), status_message());
        assert!(!peer.lossy.slot(LossySlot::Meter).is_empty());
        assert!(!peer.lossy.slot(LossySlot::StreamStatus).is_empty());

        discard_unstarted_lossy_records(&mut peer);
        assert!(peer.lossy.slot(LossySlot::Meter).is_empty());
        assert!(peer.lossy.slot(LossySlot::StreamStatus).is_empty());
    }

    struct PartialWriter {
        bytes: Vec<u8>,
        limit: usize,
    }

    impl Write for PartialWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let written = bytes.len().min(self.limit);
            self.bytes.extend_from_slice(&bytes[..written]);
            Ok(written)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn monitor_group(tap: MonitorTap, sequence: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        for part in 0..2_u16 {
            let message = fm_protocol::MonitorFrameMessage {
                server: identity(),
                tap,
                sequence,
                frame: 3,
                input: fm_protocol::WireInputId::new(NonZeroU128::new(1).unwrap()),
                origin: fm_protocol::MonitorFrameOrigin::Simulated,
                width: 2,
                height: 2,
                part,
                parts: 2,
                data: vec![u8::try_from(part).unwrap() + 1; 5],
            };
            bytes.extend_from_slice(
                encode_line(&WireMessage::MonitorFrame(message))
                    .unwrap()
                    .as_bytes(),
            );
        }
        bytes
    }

    #[test]
    fn partial_lossy_writes_never_interleave_records() {
        // Regression: a partially written meter used to let the stream status
        // start in the same pass, splicing its bytes into the meter line.
        let meters = encode_line(&WireMessage::AudioMeters(meters_message()))
            .unwrap()
            .into_bytes();
        let status = encode_line(&WireMessage::StreamStatus(status_message()))
            .unwrap()
            .into_bytes();
        let preview = monitor_group(MonitorTap::Preview, 1);
        let program = monitor_group(MonitorTap::Program, 1);
        let mut records = LossyRecords::default();
        records.replace(LossySlot::Meter, meters.clone());
        records.replace(LossySlot::StreamStatus, status.clone());
        records.replace(LossySlot::PreviewFrame, preview.clone());
        records.replace(LossySlot::ProgramFrame, program.clone());
        let mut writer = PartialWriter {
            bytes: Vec::new(),
            limit: 7,
        };
        let newer_program = monitor_group(MonitorTap::Program, 2);
        let mut replaced = false;
        while !records.is_empty() {
            records.write_once(&mut writer).unwrap();
            // A newer group arriving while the program group is mid-flight
            // waits behind it instead of splicing into it.
            if !replaced && records.slot(LossySlot::ProgramFrame).started() {
                records.replace(LossySlot::ProgramFrame, newer_program.clone());
                records.replace(LossySlot::Meter, meters.clone());
                replaced = true;
            }
        }
        assert!(replaced);
        assert_eq!(
            writer.bytes,
            [
                meters.clone(),
                status,
                preview,
                program,
                meters,
                newer_program
            ]
            .concat()
        );

        let decoded = LineDecoder::new().push(&writer.bytes).unwrap();
        let kinds = decoded
            .iter()
            .map(|message| match message {
                WireMessage::AudioMeters(_) => "meters",
                WireMessage::StreamStatus(_) => "status",
                WireMessage::MonitorFrame(frame) => frame.tap.as_str(),
                _ => "other",
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "meters", "status", "preview", "preview", "program", "program", "meters",
                "program", "program"
            ]
        );
    }

    #[test]
    fn lossy_round_robin_never_starves_quieter_slots() {
        let meters = encode_line(&WireMessage::AudioMeters(meters_message()))
            .unwrap()
            .into_bytes();
        let mut records = LossyRecords::default();
        records.replace(LossySlot::Meter, meters.clone());
        records.replace(LossySlot::StreamStatus, b"status\n".to_vec());
        records.replace(LossySlot::PreviewFrame, b"preview\n".to_vec());
        records.replace(LossySlot::ProgramFrame, b"program\n".to_vec());
        let mut writer = Vec::new();
        for _ in 0..4 {
            records.write_once(&mut writer).unwrap();
            // The meter slot is refilled after every pass, as at frame rate.
            records.replace(LossySlot::Meter, meters.clone());
        }
        assert_eq!(
            writer,
            [
                meters.clone(),
                b"status\n".to_vec(),
                b"preview\n".to_vec(),
                b"program\n".to_vec()
            ]
            .concat()
        );
        records.write_once(&mut writer).unwrap();
        assert!(writer.ends_with(&meters));
    }

    /// Regression: a raw peer was written one record per loop pass, and a
    /// pass also realizes a native frame. With frames slow (display outputs
    /// on a software GPU) heartbeat acknowledgements waited behind capture
    /// and stream status events until Studio gave up on the daemon, and the
    /// lossy meter and monitor slots, written only once the queue was idle,
    /// never reached it.
    #[test]
    fn one_pass_writes_every_queued_record_and_then_the_lossy_slots() {
        let (mut peer, mut client) = active_eligible_peer();
        for index in 0..5 {
            peer.queue(&filler(index), Accounting::Raw).unwrap();
        }
        publish_audio_meters(std::slice::from_mut(&mut peer), meters_message());
        publish_monitor_frame(
            std::slice::from_mut(&mut peer),
            MonitorTap::Program,
            &monitor_group(MonitorTap::Program, 4),
        );
        assert!(matches!(write_peer_pass(&mut peer), WriteOutcome::Pending));
        assert!(peer.outbound.is_empty());
        assert!(peer.lossy.is_empty());
        let expected = (0..5)
            .map(|index| encode_line(&filler(index)).unwrap().into_bytes())
            .chain([
                encode_line(&WireMessage::AudioMeters(meters_message()))
                    .unwrap()
                    .into_bytes(),
                monitor_group(MonitorTap::Program, 4),
            ])
            .collect::<Vec<_>>()
            .concat();
        assert_eq!(read_exact(&mut client, expected.len()), expected);
    }

    /// Regression: a pass that wrote the handshake response and then failed
    /// a later write reported only the failure, so a `--once` daemon never
    /// recorded its one peer and, after that peer closed, waited for another
    /// connection instead of exiting.
    #[test]
    fn a_pass_ends_at_the_handshake_response_so_the_caller_sees_it() {
        let (mut peer, mut client) = raw_peer();
        for index in 0..3 {
            peer.queue(&filler(index), Accounting::Raw).unwrap();
        }
        peer.outbound.front_mut().unwrap().handshake_response = true;
        assert!(matches!(
            write_peer_pass(&mut peer),
            WriteOutcome::HandshakeResponseWritten
        ));
        assert_eq!(
            peer.outbound.len(),
            2,
            "the pass continued past the response"
        );
        let first = encode_line(&filler(0)).unwrap().into_bytes();
        assert_eq!(read_exact(&mut client, first.len()), first);
    }

    #[test]
    fn a_pass_stops_at_its_write_budget_and_the_next_pass_continues() {
        let (mut peer, mut client) = active_eligible_peer();
        let large =
            |index: usize| error_message("filler", &format!("{index}:{}", "x".repeat(40_000)));
        let count = 32;
        for index in 0..count {
            peer.queue(&large(index), Accounting::Raw).unwrap();
        }
        let expected = (0..count)
            .map(|index| encode_line(&large(index)).unwrap().into_bytes())
            .collect::<Vec<_>>()
            .concat();
        assert!(expected.len() as u64 > WRITE_BUDGET_PER_PASS);
        let reader = thread::spawn(move || read_exact(&mut client, expected.len()) == expected);
        assert!(matches!(write_peer_pass(&mut peer), WriteOutcome::Pending));
        let longest = u64::try_from(fm_protocol::MAX_LINE_BYTES).unwrap();
        assert!(peer.written <= WRITE_BUDGET_PER_PASS + longest);
        assert!(!peer.outbound.is_empty(), "one pass wrote past its budget");
        while !peer.outbound.is_empty() {
            assert!(matches!(write_peer_pass(&mut peer), WriteOutcome::Pending));
            thread::sleep(Duration::from_millis(1));
        }
        assert!(reader.join().unwrap());
    }

    #[test]
    fn write_peer_starts_one_lossy_slot_per_pass() {
        let (mut peer, mut client) = active_eligible_peer();
        publish_audio_meters(std::slice::from_mut(&mut peer), meters_message());
        publish_monitor_frame(
            std::slice::from_mut(&mut peer),
            MonitorTap::Program,
            &monitor_group(MonitorTap::Program, 4),
        );
        assert!(!matches!(write_peer(&mut peer), WriteOutcome::Failed));
        assert!(peer.lossy.slot(LossySlot::Meter).is_empty());
        assert!(!peer.lossy.slot(LossySlot::ProgramFrame).is_empty());
        assert!(!matches!(write_peer(&mut peer), WriteOutcome::Failed));
        assert!(peer.lossy.is_empty());
        let expected_meters = encode_line(&WireMessage::AudioMeters(meters_message()))
            .unwrap()
            .into_bytes();
        let group = monitor_group(MonitorTap::Program, 4);
        assert_eq!(
            read_exact(&mut client, expected_meters.len() + group.len()),
            [expected_meters, group].concat()
        );
    }

    #[test]
    fn monitor_frames_reach_only_active_view_status_studio_raw_peers() {
        let view_status = vec!["view_status".to_owned(), "transition".to_owned()];
        let no_view_status = vec!["transition".to_owned()];
        assert!(monitor_frames_eligible(
            false,
            ClientType::Studio,
            &view_status
        ));
        assert!(!monitor_frames_eligible(
            true,
            ClientType::Studio,
            &view_status
        ));
        assert!(!monitor_frames_eligible(
            false,
            ClientType::Integration,
            &view_status
        ));
        assert!(!monitor_frames_eligible(
            false,
            ClientType::Studio,
            &no_view_status
        ));

        let group = monitor_group(MonitorTap::Preview, 1);
        let (mut ineligible, mut client) = raw_peer();
        ineligible.phase = Phase::Active;
        ineligible.audio_meters = true;
        ineligible.monitor_frames = false;
        let (mut syncing, _syncing_client) = active_eligible_peer();
        syncing.phase = Phase::Syncing;
        let mut peers = vec![ineligible, syncing];
        publish_monitor_frame(&mut peers, MonitorTap::Preview, &group);
        publish_monitor_frame(&mut peers, MonitorTap::Program, &group);
        assert!(peers.iter().all(|peer| peer.lossy.is_empty()));
        write_peer(&mut peers[0]);
        client.set_nonblocking(true).unwrap();
        let mut probe = [0_u8; 16];
        assert!(matches!(
            client.read(&mut probe),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert!(!peers.iter().any(Peer::wants_monitor_frames));

        let (eligible, _client) = active_eligible_peer();
        peers.push(eligible);
        assert!(peers.iter().any(Peer::wants_monitor_frames));
        publish_monitor_frame(&mut peers, MonitorTap::Preview, &group);
        assert!(!peers[2].lossy.slot(LossySlot::PreviewFrame).is_empty());
    }

    #[test]
    fn unstarted_monitor_frames_are_discarded_for_shutdown() {
        let (mut peer, _client) = active_eligible_peer();
        let group = monitor_group(MonitorTap::Preview, 1);
        publish_monitor_frame(std::slice::from_mut(&mut peer), MonitorTap::Preview, &group);
        publish_monitor_frame(std::slice::from_mut(&mut peer), MonitorTap::Program, &group);
        discard_unstarted_lossy_records(&mut peer);
        assert!(peer.lossy.is_empty());
    }

    /// Serves an empty resume so a test can hold a real session.
    struct StubPlane;

    impl fm_server::ControlPlane for StubPlane {
        type Error = std::convert::Infallible;

        fn initial_sync(
            &self,
            _cursor: Option<&fm_protocol::EventCursor>,
        ) -> Result<fm_server::InitialSync, Self::Error> {
            Ok(fm_server::InitialSync {
                engine: fm_protocol::EngineIdentity {
                    engine_id: "engine".to_owned(),
                    state_epoch: 1,
                    log_id: "log".to_owned(),
                },
                current_revision: 0,
                payload: SyncPayload::Resume(Vec::new()),
            })
        }
    }

    /// A real session whose outbound rate window admits `per_window`
    /// messages every `window_ms`.
    fn limited_session(per_window: u64, window_ms: u64, heartbeat_timeout_ms: u64) -> Session {
        let limits = fm_server::SessionLimits {
            outbound_messages: fm_server::RateLimit::new(per_window, window_ms),
            heartbeat_timeout_ms,
            ..fm_server::SessionLimits::default()
        };
        let config = fm_server::ServerConfig::new(
            fm_server::ServerMode::Development,
            fm_server::AuthenticationMode::Development,
            std::net::IpAddr::from([127, 0, 0, 1]),
            "digest",
        )
        .with_session_limits(limits);
        let mut server = Server::new(config, StubPlane).unwrap();
        server.mark_ready().unwrap();
        let principal = Principal::development(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            [Role::Admin],
        );
        server
            .handshake(
                &fm_protocol::HandshakeRequest {
                    protocol: fm_protocol::CURRENT_PROTOCOL_VERSION,
                    build: "test".to_owned(),
                    client_type: ClientType::Studio,
                    desired_role: fm_protocol::Role::Operator,
                    // Resuming at the stub's current revision replays nothing.
                    resume_cursor: Some(fm_protocol::ResumeCursor {
                        server: identity(),
                        revision: 0,
                    }),
                },
                &principal,
                now_millis().unwrap(),
            )
            .unwrap()
            .session
    }

    fn command_result(id: &str) -> WireMessage {
        WireMessage::CommandResult(CommandResult::Accepted {
            id: id.to_owned(),
            revision: 1,
            scheduled_frame: None,
        })
    }

    fn filler(index: usize) -> WireMessage {
        error_message("filler", &format!("live record {index}"))
    }

    /// Reads every complete line the peer has written so far.
    fn received(client: &mut TcpStream, decoder: &mut LineDecoder) -> Vec<WireMessage> {
        client.set_nonblocking(true).unwrap();
        let mut messages = Vec::new();
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            match client.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => messages.extend(decoder.push(&chunk[..read]).unwrap()),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("{error}"),
            }
        }
        messages
    }

    /// Regression: a command result that met an outbound queue full of live
    /// records closed the session, losing a durable acknowledgement and
    /// dropping a merely busy peer.
    #[test]
    fn a_command_result_waits_out_a_full_outbound_queue_instead_of_disconnecting() {
        let (mut peer, mut client) = active_eligible_peer();
        peer.session = Some(limited_session(1_000, 1_000, 15_000));
        // Live records fill every slot they may take; the reply slot stays.
        for index in 0..OUTBOUND_CAPACITY - REPLY_RESERVE {
            peer.queue(&filler(index), Accounting::Session).unwrap();
        }
        // Something else (an earlier reply) takes the reserved slot too.
        peer.queue(&filler(99), Accounting::Session).unwrap();
        assert_eq!(peer.outbound.len(), OUTBOUND_CAPACITY);

        assert_eq!(peer.reply(command_result("busy")), Ok(()));
        assert!(peer.held_result.is_some(), "the result is held, not lost");
        let mut close = [false];
        release_held_results(std::slice::from_mut(&mut peer), &mut close, |_| false);
        assert_eq!(close, [false], "backpressure never disconnects");
        assert!(peer.held_result.is_some());

        // The peer drains one record; the held result takes the free slot.
        while peer.outbound.len() == OUTBOUND_CAPACITY {
            assert!(!matches!(write_peer(&mut peer), WriteOutcome::Failed));
        }
        release_held_results(std::slice::from_mut(&mut peer), &mut close, |_| false);
        assert_eq!(close, [false]);
        assert!(peer.held_result.is_none());
        while !peer.outbound.is_empty() {
            assert!(!matches!(write_peer(&mut peer), WriteOutcome::Failed));
        }
        let mut decoder = LineDecoder::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut messages = Vec::new();
        while messages.len() < OUTBOUND_CAPACITY + 1 {
            assert!(Instant::now() < deadline, "only {} records", messages.len());
            messages.extend(received(&mut client, &mut decoder));
        }
        assert_eq!(messages.last(), Some(&command_result("busy")));
    }

    /// Regression: `OutboundRateLimited` is retryable and non-disconnecting in
    /// fm-server, yet a rate-limited command result closed the session.
    #[test]
    fn a_rate_limited_command_result_is_retried_not_disconnected() {
        let (mut peer, mut client) = active_eligible_peer();
        peer.session = Some(limited_session(1, 200, 15_000));
        peer.queue(&filler(0), Accounting::Session).unwrap();
        assert_eq!(
            peer.queue(&filler(1), Accounting::Session),
            Err(QueueRefusal::RateLimited)
        );

        assert_eq!(peer.reply(command_result("limited")), Ok(()));
        let mut close = [false];
        release_held_results(std::slice::from_mut(&mut peer), &mut close, |_| false);
        assert_eq!(close, [false]);
        assert!(peer.held_result.is_some());

        let deadline = Instant::now() + Duration::from_secs(5);
        while peer.held_result.is_some() {
            assert!(Instant::now() < deadline, "the window never reopened");
            thread::sleep(Duration::from_millis(20));
            release_held_results(std::slice::from_mut(&mut peer), &mut close, |_| false);
            assert_eq!(close, [false]);
        }
        while !peer.outbound.is_empty() {
            assert!(!matches!(write_peer(&mut peer), WriteOutcome::Failed));
        }
        let mut decoder = LineDecoder::new();
        let mut messages = Vec::new();
        while messages.len() < 2 {
            assert!(Instant::now() < deadline);
            messages.extend(received(&mut client, &mut decoder));
        }
        assert_eq!(messages, [filler(0), command_result("limited")]);
    }

    /// A rate-limited live event waits in its bounded slot; the session stays.
    #[test]
    fn rate_limited_live_records_wait_instead_of_disconnecting() {
        let (mut peer, _client) = active_eligible_peer();
        peer.session = Some(limited_session(1, 60_000, 15_000));
        peer.queue(&filler(0), Accounting::Session).unwrap();
        peer.initial_sync.push_back(filler(1));
        assert_eq!(peer.feed_initial_sync(), Ok(()));
        assert_eq!(peer.initial_sync.len(), 1, "the sync record waits its turn");
        assert_eq!(peer.outbound.len(), 1);
    }

    /// A web peer's rate-limited record is sent on a later pass.
    #[test]
    fn a_rate_limited_web_record_stays_pending() {
        let (_inbound_sender, inbound) = mpsc::channel();
        let (outbound_sender, outbound) = mpsc::sync_channel(8);
        let (acknowledgement_sender, acknowledgements) = mpsc::channel();
        let mut peer = Peer::new(
            1,
            Transport::Web(WebTransport {
                inbound,
                outbound: outbound_sender,
                acknowledgements,
                cancel: Arc::new(AtomicBool::new(false)),
                _slot: None,
            }),
            Principal::authenticated(
                UserId::new("user").unwrap(),
                SessionId::new("session").unwrap(),
                [Role::Admin],
            ),
            Duration::from_secs(5),
        )
        .unwrap();
        peer.phase = Phase::Active;
        peer.session = Some(limited_session(1, 200, 15_000));
        peer.queue(&filler(0), Accounting::Session).unwrap();
        peer.queue(&command_result("web"), Accounting::Session)
            .unwrap();
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        acknowledgement_sender.send(()).unwrap();
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        // The window is spent: the result is not sent, and nothing fails.
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        assert_eq!(outbound.try_iter().count(), 1);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "the result was never sent");
            assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
            if let Ok(bytes) = outbound.try_recv() {
                assert_eq!(
                    LineDecoder::new().push(&bytes).unwrap(),
                    [command_result("web")]
                );
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Backpressure handling never keeps a stuck peer alive: a held reply
    /// stops dispatch, so its heartbeats go unrecorded and the session's
    /// heartbeat timeout closes it as before.
    #[test]
    fn a_stuck_peer_with_a_held_result_still_times_out() {
        let (mut peer, _client) = active_eligible_peer();
        peer.session = Some(limited_session(1, 60_000, 100));
        peer.queue(&filler(0), Accounting::Session).unwrap();
        assert_eq!(peer.reply(command_result("stuck")), Ok(()));
        assert!(peer.held_result.is_some());
        assert!(super::super::session_heartbeat_active(peer.session.as_mut().unwrap()).unwrap());
        thread::sleep(Duration::from_millis(150));
        assert!(
            !super::super::session_heartbeat_active(peer.session.as_mut().unwrap()).unwrap(),
            "the heartbeat timeout still closes a peer that never drains"
        );
    }

    /// A peer whose kernel socket buffers are full (it stopped reading) never
    /// delays another peer: each pass makes one non-blocking write attempt
    /// for it, its records stay bounded, and backpressure alone never closes
    /// it. Its heartbeat timeout closes it instead (see
    /// `a_stuck_peer_with_a_held_result_still_times_out`).
    #[test]
    fn a_peer_with_full_socket_buffers_never_delays_another_peer() {
        const RESULTS: usize = 3 * OUTBOUND_CAPACITY;
        // Far below `CLIENT_WRITE_TIMEOUT`, which a blocking write would hit.
        const WRITE_LIMIT: Duration = Duration::from_millis(50);

        let (mut stalled, _stalled_client) = active_eligible_peer();
        stalled.session = Some(limited_session(1_000, 1_000, 15_000));
        let (mut live, mut live_client) = active_eligible_peer();
        live.id = 1;
        live.session = Some(limited_session(1_000, 1_000, 15_000));

        // The stalled peer's send buffer and its client's receive buffer are
        // full: the client never reads. A first `WouldBlock` only means the
        // send buffer is full *now*: under load the loopback receiver may not
        // have processed (and ACKed) the queued segments yet, and once it
        // does the send buffer frees up again. Refill until the socket stays
        // full across a quiet period, i.e. the receive window has closed.
        let Transport::Raw(stream) = &mut stalled.transport else {
            unreachable!("raw peer")
        };
        let chunk = vec![b'\n'; 64 * 1024];
        let settle_deadline = Instant::now() + Duration::from_secs(10);
        let mut quiet_rounds = 0;
        while quiet_rounds < 3 {
            assert!(
                Instant::now() < settle_deadline,
                "the stalled socket never stayed full"
            );
            let mut accepted = false;
            loop {
                match stream.write(&chunk) {
                    Ok(_) => accepted = true,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("{error}"),
                }
            }
            quiet_rounds = if accepted { 0 } else { quiet_rounds + 1 };
            thread::sleep(Duration::from_millis(20));
        }

        let mut peers = vec![stalled, live];
        let mut decoder = LineDecoder::new();
        let mut results = Vec::new();
        let mut slowest_stalled_write = Duration::ZERO;
        let mut pass = 0_usize;
        while results.len() < RESULTS {
            assert!(pass < 4 * RESULTS, "only {} results arrived", results.len());
            let sequence = u64::try_from(pass).unwrap();
            publish_monitor_frame(
                &mut peers,
                MonitorTap::Program,
                &monitor_group(MonitorTap::Program, sequence),
            );
            if pass < RESULTS {
                // Live records pile up for the stalled peer until its
                // bounded queue refuses them.
                match peers[0].queue(&filler(pass), Accounting::Session) {
                    Ok(()) | Err(QueueRefusal::Full) => {}
                    Err(refusal) => panic!("the stalled peer refused a record: {refusal:?}"),
                }
                peers[1]
                    .reply(command_result(&format!("live-{pass}")))
                    .unwrap();
            }
            release_held_results(&mut peers, &mut [false, false], |_| false);

            let started = Instant::now();
            assert!(matches!(write_peer(&mut peers[0]), WriteOutcome::Pending));
            slowest_stalled_write = slowest_stalled_write.max(started.elapsed());
            assert!(matches!(write_peer(&mut peers[1]), WriteOutcome::Pending));
            assert!(matches!(write_peer(&mut peers[1]), WriteOutcome::Pending));

            results.extend(
                received(&mut live_client, &mut decoder)
                    .into_iter()
                    .filter(|message| matches!(message, WireMessage::CommandResult(_))),
            );
            pass += 1;
        }

        assert!(
            slowest_stalled_write < WRITE_LIMIT,
            "a write to the stalled peer took {slowest_stalled_write:?}"
        );
        let expected = (0..RESULTS)
            .map(|index| command_result(&format!("live-{index}")))
            .collect::<Vec<_>>();
        assert_eq!(results, expected, "every live result arrives, in order");
        // Nothing written to the stalled peer, and nothing grows past bounds.
        assert_eq!(peers[0].outbound.len(), OUTBOUND_CAPACITY);
        assert!(peers[0].held_result.is_none());
        assert!(
            super::super::session_heartbeat_active(peers[0].session.as_mut().unwrap()).unwrap()
        );
    }
    fn web_peer() -> (
        Peer,
        mpsc::Receiver<Vec<u8>>,
        mpsc::SyncSender<()>,
        mpsc::Sender<WireMessage>,
    ) {
        let (inbound_sender, inbound) = mpsc::channel();
        let (outbound_sender, outbound) = mpsc::sync_channel(OUTBOUND_CAPACITY);
        let (acknowledgement_sender, acknowledgements) = mpsc::sync_channel(OUTBOUND_CAPACITY);
        let mut peer = Peer::new(
            1,
            Transport::Web(WebTransport {
                inbound,
                outbound: outbound_sender,
                acknowledgements,
                cancel: Arc::new(AtomicBool::new(false)),
                _slot: None,
            }),
            Principal::authenticated(
                UserId::new("user").unwrap(),
                SessionId::new("session").unwrap(),
                [Role::Admin],
            ),
            Duration::from_secs(5),
        )
        .unwrap();
        peer.phase = Phase::Active;
        peer.session = Some(limited_session(1_000, 1_000, 15_000));
        (peer, outbound, acknowledgement_sender, inbound_sender)
    }

    /// Regression: a Web peer was handed one record per pass and the next
    /// only after the relay acknowledged it, so a Viewer tally received about
    /// one record per relay poll and was dropped as a slow client while a CLI
    /// cut every frame. Every queued record is now handed over in one pass,
    /// in order, and acknowledgements retire them in order.
    #[test]
    fn web_records_are_pipelined_and_retired_in_order() {
        let (mut peer, outbound, acknowledge, _inbound) = web_peer();
        for index in 0..10 {
            peer.queue(&filler(index), Accounting::Session).unwrap();
        }
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        let handed: Vec<WireMessage> = outbound
            .try_iter()
            .flat_map(|bytes| LineDecoder::new().push(&bytes).unwrap())
            .collect();
        assert_eq!(handed, (0..10).map(filler).collect::<Vec<_>>());
        // Handed-over records keep their place in the bounded queue until
        // the relay acknowledges them.
        assert_eq!(peer.outbound.len(), 10);

        for _ in 0..4 {
            acknowledge.send(()).unwrap();
        }
        peer.queue(&command_result("later"), Accounting::Session)
            .unwrap();
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        assert_eq!(peer.outbound.len(), 7);
        assert!(peer.outbound.iter().all(|record| record.channel_sent));
        assert_eq!(
            LineDecoder::new()
                .push(&outbound.try_recv().unwrap())
                .unwrap(),
            [command_result("later")]
        );

        // Shutdown keeps every record already handed to the relay.
        peer.queue(&filler(99), Accounting::Session).unwrap();
        peer.replace_outbound_for_shutdown();
        assert_eq!(peer.outbound.len(), 7);
        for _ in 0..7 {
            acknowledge.send(()).unwrap();
        }
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        assert!(peer.outbound.is_empty());
        // An acknowledgement for a record never handed over is a relay bug.
        acknowledge.send(()).unwrap();
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Failed));
    }

    /// Regression: a raw peer that sent more messages than its inbound queue
    /// holds in one write was closed as `inbound_overflow`, so a Studio or
    /// CLI peer pipelining commands to a native daemon (one command per
    /// frame across every peer) was dropped. Everything one read decodes is
    /// kept, and a full queue is not read until it drains (TCP
    /// backpressure).
    #[test]
    fn a_raw_peer_that_pipelines_is_slowed_not_dropped() {
        let (mut peer, mut client) = raw_peer();
        peer.phase = Phase::Active;
        let mut burst = String::new();
        for index in 0..(3 * INBOUND_CAPACITY) {
            burst.push_str(&encode_line(&filler(index)).unwrap());
        }
        client.write_all(burst.as_bytes()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while peer.inbound.len() < 3 * INBOUND_CAPACITY {
            assert!(Instant::now() < deadline, "the burst never arrived");
            assert!(!read_peer(&mut peer), "a pipelining peer was closed");
            thread::sleep(Duration::from_millis(5));
        }
        // A full queue is not read: what arrives next waits in the socket.
        client
            .write_all(encode_line(&filler(1_000)).unwrap().as_bytes())
            .unwrap();
        thread::sleep(Duration::from_millis(50));
        assert!(!read_peer(&mut peer));
        assert_eq!(peer.inbound.len(), 3 * INBOUND_CAPACITY);
        peer.inbound.clear();
        assert!(!read_peer(&mut peer));
        assert_eq!(peer.inbound.front(), Some(&filler(1_000)));
    }

    /// Regression: the command turn rotated with the pass, not with the peer
    /// that took it, so of two peers with commands waiting the one after a
    /// run of idle peers took a turn for every idle peer (three here) for
    /// each turn the other took. Turns now alternate strictly, wherever the
    /// idle peers sit.
    #[test]
    fn command_turns_alternate_between_peers_with_commands_waiting() {
        let command = || {
            WireMessage::Command(fm_protocol::CommandMessage {
                protocol: fm_protocol::CURRENT_PROTOCOL_VERSION,
                id: "cut".into(),
                idempotency_key: "cut".into(),
                expected_revision: None,
                deadline_ms: None,
                payload: CommandPayload::Cut,
            })
        };
        let mut peers: Vec<Peer> = (0..4)
            .map(|index| {
                let (mut peer, _client) = raw_peer();
                peer.id = index;
                peer.phase = Phase::Active;
                peer
            })
            .collect();
        for index in [0, 3] {
            for _ in 0..4 {
                peers[index].inbound.push_back(command());
            }
        }
        // A heartbeat is no command: its peer never takes the turn.
        peers[1].inbound.push_back(filler(0));
        let close = vec![false; peers.len()];
        let mut cursor = 0;
        let mut turns = Vec::new();
        for _ in 0..6 {
            let turn = command_turn(&peers, &close, cursor % peers.len()).unwrap();
            peers[turn].inbound.pop_front();
            turns.push(turn);
            cursor = turn + 1;
        }
        assert_eq!(turns, [0, 3, 0, 3, 0, 3]);
        // Closed, held, or not yet active peers never take it.
        peers[0].held_result = Some(HeldResult {
            message: filler(1),
            slot: None,
        });
        peers[3].phase = Phase::Syncing;
        assert_eq!(command_turn(&peers, &close, 0), None);
    }

    /// Every raw session runs as the daemon's principal with a session id
    /// of its own, as every Web session does.
    #[test]
    fn raw_peers_hold_their_own_session_ids() {
        let daemon = Principal::development(
            UserId::new("local-operator").unwrap(),
            SessionId::new("local-session").unwrap(),
            [Role::Admin],
        );
        let first = raw_principal(&daemon, 0);
        let second = raw_principal(&daemon, 1);
        assert_ne!(first.session_id(), second.session_id());
        for principal in [&first, &second] {
            assert_eq!(principal.user_id(), daemon.user_id());
            assert_eq!(principal.roles(), daemon.roles());
            assert_eq!(principal.kind(), daemon.kind());
        }
        assert_eq!(first.session_id().as_str(), "local-session-0");
    }
}
