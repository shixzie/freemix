//! The daemon's one control-session scheduler, shared by the simulated and
//! the native-media daemon.
//!
//! A single thread serves every control session: up to [`MAX_PEERS`] raw TCP
//! peers plus up to [`MAX_WEB_CLIENTS`] Web peers from the gateway. Each peer
//! is its own session (handshake, granted role, heartbeat, live
//! subscription, rate limits, outbound queue, and lossy side channels), and
//! no peer ever blocks the loop: sockets are non-blocking, Web records are
//! handed over with `try_send`, and every queue is bounded. A native daemon
//! realizes its Program frames from the same loop, so it dispatches commands
//! on at most one frame per pass and serves every peer's reads, heartbeats,
//! and writes between such frames; the commands queued when a pass takes its
//! command turn begin on that one frame together and share one journal
//! append (a group commit, see `native_group` and `Runtime::dispatch_group`).
//! A simulated daemon dispatches one message per peer per pass, and the
//! commands several peers sent in one pass share one journal append the
//! same way.

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
    HeartbeatAcknowledgementMessage, LineDecoder, MonitorTap, RecorderStatusMessage,
    ServerIdentity, SourceSyncStatusMessage, StreamStatusMessage, StructuredError, WireMessage,
    encode_line,
};
use fm_server::{Server, Session, SessionError, SyncPayload};

use super::{
    AppResult, CLIENT_READ_POLL_INTERVAL, CLIENT_WRITE_TIMEOUT, CommandDelivery, ControlHandle,
    DaemonShutdownReason, GroupMember, NativeDaemon, PendingWrite, ProcessShutdown, SharedControl,
    admit_session_command, current_handshake, diagnostics_response, error_message,
    execute_command_group, execute_session_command, group_commit_eligible, handshake_code,
    handshake_response, is_client_session_termination, native_group_eligible, now_millis,
    reconciled_handshake_outcome, record_heartbeat, rejected_handshake_response,
    requested_daemon_shutdown, server_identity, shutdown_message, structured_session_error,
};
use crate::input_thumbnails::{ThumbnailKey, ThumbnailOutbox, ThumbnailWork, ThumbnailWorker};
use crate::journal::DurableStore;
use crate::latest_record::LatestRecord;
use crate::monitor_tap::SimulatedMonitorWorker;
use crate::title_render::TitleAssets;
use crate::web::{ClientSlot, MAX_WEB_CLIENTS, WebEvent, WebGateway};
use crate::web_feed::{WEB_FEED_MAX_QUEUED, WebFeed, web_meters};

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
    /// A Web peer's lossy feed record (see [`WebFeed`]): never charged to the
    /// session's outbound rate window, and at most one is queued at a time.
    feed: bool,
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
    /// Raw Studio peers take `recorder_status` like `stream_status` (2.66).
    recorder_status: bool,
    monitor_frames: bool,
    /// A Web peer's lossy status feed, when its role may view status
    /// (Protocol 2.66).
    web_feed: Option<WebFeed>,
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
    /// Live records taken from the subscription that met transient
    /// backpressure, queued first on a later pass, in order: an event, and
    /// after a Web peer's `title_exposure_changed` the `title_state`
    /// records that resynchronize its titles (Protocol 2.96).
    deferred_live: VecDeque<WireMessage>,
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
    /// The latest `recorder_status` (Protocol 2.66).
    RecorderStatus,
    PreviewFrame,
    ProgramFrame,
    /// The latest exact title or scene preview (`title_preview_frame` or
    /// `scene_preview_frame` parts).
    TitlePreview,
    /// The input thumbnail being written (Protocol 2.59); the next one waits
    /// in the peer's [`ThumbnailOutbox`] until this slot is empty.
    Thumbnail,
    /// The latest `source_sync_status` (Protocol 2.76).
    SourceSyncStatus,
    /// The latest `title_phases` (Protocol 2.90).
    TitlePhases,
    /// The latest `health_status` (Protocol 2.100).
    HealthStatus,
    /// The latest `replay_status` (OR-012).
    ReplayStatus,
    /// The latest `reel_status` (OR-013).
    ReelStatus,
}

impl LossySlot {
    const COUNT: usize = 12;

    const fn index(self) -> usize {
        match self {
            Self::Meter => 0,
            Self::StreamStatus => 1,
            Self::PreviewFrame => 2,
            Self::ProgramFrame => 3,
            Self::TitlePreview => 4,
            Self::Thumbnail => 5,
            Self::RecorderStatus => 6,
            Self::SourceSyncStatus => 7,
            Self::TitlePhases => 8,
            Self::HealthStatus => 9,
            Self::ReplayStatus => 10,
            Self::ReelStatus => 11,
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
            recorder_status: false,
            monitor_frames: false,
            web_feed: None,
            thumbnails: None,
            lossy: LossyRecords::default(),
            held_result: None,
            deferred_live: VecDeque::new(),
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
            feed: false,
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
        if self.phase != Phase::Active {
            return;
        }
        if let Some(feed) = self.web_feed.as_mut() {
            feed.offer_stream_status(bytes);
        } else if self.stream_status {
            self.lossy.replace(LossySlot::StreamStatus, bytes);
        }
    }

    /// Coalesces the latest recorder status in its own slot (Protocol 2.66).
    fn replace_recorder_status(&mut self, bytes: Vec<u8>) {
        if self.phase != Phase::Active {
            return;
        }
        if let Some(feed) = self.web_feed.as_mut() {
            feed.offer_recorder_status(bytes);
        } else if self.recorder_status {
            self.lossy.replace(LossySlot::RecorderStatus, bytes);
        }
    }

    /// Coalesces the latest live source drift telemetry (Protocol 2.76): a
    /// raw Studio session takes it in its own slot, and since Protocol
    /// 2.101 a Web page's feed paces it.
    fn replace_source_sync_status(&mut self, bytes: Vec<u8>) {
        if self.phase != Phase::Active {
            return;
        }
        if let Some(feed) = self.web_feed.as_mut() {
            feed.offer_source_sync_status(bytes);
        } else if self.recorder_status {
            self.lossy.replace(LossySlot::SourceSyncStatus, bytes);
        }
    }

    /// Coalesces the latest replay status in its own slot (OR-012); Web
    /// pages do not take it.
    fn replace_replay_status(&mut self, bytes: Vec<u8>) {
        if self.phase == Phase::Active && self.web_feed.is_none() && self.recorder_status {
            self.lossy.replace(LossySlot::ReplayStatus, bytes);
        }
    }

    /// Coalesces the latest highlight reel status in its own slot (OR-013);
    /// Web pages do not take it.
    fn replace_reel_status(&mut self, bytes: Vec<u8>) {
        if self.phase == Phase::Active && self.web_feed.is_none() && self.recorder_status {
            self.lossy.replace(LossySlot::ReelStatus, bytes);
        }
    }

    /// Coalesces the latest daemon health in its own slot (Protocol 2.100);
    /// Web pages do not take it.
    fn replace_health_status(&mut self, bytes: Vec<u8>) {
        if self.phase == Phase::Active && self.web_feed.is_none() && self.recorder_status {
            self.lossy.replace(LossySlot::HealthStatus, bytes);
        }
    }

    /// Coalesces the latest `title_phases` (Protocol 2.90): a Web page's
    /// feed paces it, a raw Studio session takes it in its own slot.
    fn replace_title_phases(&mut self, bytes: &[u8]) {
        if self.phase != Phase::Active {
            return;
        }
        if let Some(feed) = self.web_feed.as_mut() {
            feed.offer_title_phases(bytes.to_vec());
        } else if self.recorder_status {
            self.lossy.replace(LossySlot::TitlePhases, bytes.to_vec());
        }
    }

    /// Whether this session takes `title_phases` records.
    fn wants_title_phases(&self) -> bool {
        self.phase == Phase::Active && (self.web_feed.is_some() || self.recorder_status)
    }

    /// Whether this session takes audio meters: a raw Studio session, or a
    /// Web page granted the lossy status feed.
    fn wants_audio_meters(&self) -> bool {
        self.phase == Phase::Active && (self.audio_meters || self.web_feed.is_some())
    }

    /// Offers a Web peer's feed the Master meters of `web_bytes`.
    fn replace_web_meter(&mut self, web_bytes: &[u8]) {
        if self.phase == Phase::Active
            && let Some(feed) = self.web_feed.as_mut()
        {
            feed.offer_meters(web_bytes.to_vec());
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
        // A Web page takes each part as its own record (Protocol 2.90).
        if self.transport.is_web() {
            let lines = messages
                .iter()
                .map(|message| {
                    encode_line(message)
                        .map(String::into_bytes)
                        .map_err(|_| QueueRefusal::Fatal)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(feed) = self.web_feed.as_mut() {
                feed.offer_preview(lines);
            }
            return Ok(());
        }
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

    /// Queues one input's (or media list item's) newest thumbnail for a
    /// subscribed peer.
    fn offer_thumbnail(&mut self, key: ThumbnailKey, bytes: &[u8]) {
        if self.phase == Phase::Active
            && let Some(outbox) = self.thumbnails.as_mut()
        {
            outbox.offer(key, bytes.to_vec());
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
    // When `title_phases` (Protocol 2.90) is next sent.
    let mut phase_pacer = crate::web_titles::PhasePacer::default();
    // Where the next pass starts dispatching, so no peer is always last.
    let mut dispatch_cursor = 0_usize;
    let mut once_peer = None;
    // Exact title previews render off this loop, as simulated monitor
    // frames do.
    let title_previews = crate::title_preview::spawn_preview_worker(Arc::clone(titles))?;
    // System audio devices are listed off this loop too (Protocol 2.68).
    let system_audio_devices = crate::system_audio_devices::SystemAudioDevices::spawn(
        crate::system_audio_devices::enumerator(native.is_some()),
        crate::system_audio_devices::REUSE_FOR,
    )?;
    // So are the headphone output devices (AU-001, Protocol 2.102).
    let audio_output_devices = crate::audio_output_devices::AudioOutputDevices::spawn(
        crate::audio_output_devices::enumerator(native.is_some()),
        crate::audio_output_devices::REUSE_FOR,
    )?;
    // Document pages render off this loop too (IN-014).
    let documents = store.documents().cloned().unwrap_or_default();
    let document_pages = crate::documents::PageWorker::spawn(Arc::clone(&documents))?;
    // What the server can capture is listed off this loop too (IN-011).
    let screen_capture_targets = crate::screen_capture_targets::ScreenCaptureTargets::spawn(
        crate::screen_capture_targets::enumerator(native.is_some()),
        crate::screen_capture_targets::REUSE_FOR,
    )?;
    let runtime = Runtime {
        server,
        authority,
        control,
        store,
        titles,
        title_previews: &title_previews,
        system_audio_devices: &system_audio_devices,
        audio_output_devices: &audio_output_devices,
        web_access: web.map(WebGateway::access),
        document_pages: &document_pages,
        screen_capture_targets: &screen_capture_targets,
        process_shutdown,
        playlist_clock: std::cell::RefCell::new(crate::playlist_clock::PlaylistClock::default()),
        cue_clock: std::cell::RefCell::new(crate::cue_clock::CueClock::default()),
        media_clock: std::cell::RefCell::new(crate::media_clock::MediaClock::default()),
        slideshow_folders: std::cell::RefCell::new(crate::stills::SlideshowFolders::default()),
        end_triggers: std::cell::RefCell::new(crate::media_clock::EndTriggerFeed::default()),
        grade_cache: std::cell::RefCell::new(crate::grading::GradeCache::default()),
        thumbnail_refresh: std::cell::RefCell::new(false),
        statistics: std::cell::RefCell::new(crate::statistics::DaemonStatistics::new(
            Instant::now(),
        )),
        replay_pacer: std::cell::RefCell::new(ReplayStatusPacer::default()),
        reel_pacer: std::cell::RefCell::new(ReelStatusPacer::default()),
    };
    let mut native = native;
    // Only the native pacer realizes idle frames between commands, so only
    // it has idle progress to record (the simulated pacer realizes only the
    // frames of work a command's durable projection already settles past).
    let mut progress = native
        .is_some()
        .then(|| crate::NativeProgress::new(durable));
    // Simulated monitor frames are composed off this loop so a slow Scene
    // composition never delays command results, heartbeats, or events.
    // Input thumbnails render off this loop too (Protocol 2.59).
    // A native daemon decodes each media list item's picture (IN-006).
    #[cfg(feature = "native-media")]
    let item_pictures = native
        .is_some()
        .then(|| crate::native_files::item_pictures(titles.store()))
        .flatten();
    #[cfg(not(feature = "native-media"))]
    let item_pictures = None;
    // Every media list item's file length is probed off this loop too
    // (Protocol 2.91): a native daemon through its decoder, a simulated one
    // by its deterministic stand-in.
    #[cfg(feature = "native-media")]
    let length_probe = native
        .is_some()
        .then(|| crate::native_files::item_lengths(titles.store()))
        .flatten();
    #[cfg(not(feature = "native-media"))]
    let length_probe: Option<Arc<dyn crate::media_lengths::LengthProbe>> = None;
    let length_probe = match length_probe {
        Some(probe) => probe,
        None if native.is_some() => Arc::new(crate::media_lengths::NoLengths),
        None => Arc::new(crate::media_lengths::SimulatedLengths),
    };
    let mut media_lengths = crate::media_lengths::MediaLengthWorker::spawn(
        titles
            .store()
            .map(fm_persistence::ProjectStore::assets_root),
        length_probe,
    )?;
    let mut media_lengths_revision = None;
    let mut thumbnails = ThumbnailWorker::spawn(
        authority.clone(),
        Arc::clone(titles),
        Arc::clone(&documents),
        item_pictures,
    )?;
    let mut thumbnail_grades = crate::grading::GradeCache::default();
    let mut simulated_pacer = SimulatedPacer::default();
    let mut simulated_monitor = if native.is_none() {
        Some(SimulatedMonitorWorker::spawn(
            authority.clone(),
            Arc::clone(titles),
            Arc::clone(&documents),
        )?)
    } else {
        None
    };
    // The simulated daemon's audio plane renders the persisted strip and
    // Master chains on its own wall clock while a Studio peer demands
    // meters (AU-005). It never advances the engine or the journal.
    let mut simulated_audio = if native.is_none() {
        crate::sim_audio::SimulatedAudioRuntime::new(
            durable.project(),
            durable.position().frames_rendered,
        )
        .ok()
    } else {
        None
    };
    let mut simulated_audio_sequence = 0_u64;

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
                        let web_principal = web_principal(&connection.identity, next_peer_id);
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
        let mut failure = None;
        for (index, peer) in peers.iter_mut().enumerate() {
            close[index] = read_peer(peer);
            // A heartbeat the peer sent is recorded before its timeout is
            // judged: one read only now (the loop was busy, or the peer's
            // socket was read late) still keeps the session alive. Only
            // heartbeats at the front of the queue are, so the peer's
            // messages are still answered in order, and none while a reply
            // is held, so a peer that never drains still times out.
            while !close[index]
                && peer.phase == Phase::Active
                && peer.held_result.is_none()
                && matches!(peer.inbound.front(), Some(WireMessage::Heartbeat(_)))
            {
                let heartbeat = peer.inbound.pop_front().expect("the front is a heartbeat");
                match runtime.dispatch(peer, heartbeat, durable, None) {
                    Ok(()) => {}
                    Err(DispatchError::Peer) => {
                        close[index] = dropping(peer.id, "dispatch_refused");
                    }
                    Err(DispatchError::Daemon(error)) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
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
        if let Some(error) = failure {
            shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
            return Err(error);
        }

        let mut live_budget = vec![LIVE_EVENTS_PER_PASS; peers.len()];
        // A native command waits for and realizes the pacer's next frame, so
        // a pass dispatches commands on one frame only, at one peer's turn:
        // every peer is read and written between two such frames, so no
        // peer's commands delay another peer's heartbeats or records by
        // more than one frame. The commands queued when the turn comes are
        // executed together as one group commit, all begun on that frame, up
        // to NATIVE_GROUP_COMMANDS (see `native_group`): one journal append
        // and disk sync, and one frame, for all of them, so throughput is no
        // longer one command per frame. A command that cannot join a group
        // takes the turn alone, as before group commit.
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
        // A simulated daemon dispatches one message per peer per pass, so a
        // pass may carry several peers' commands. Consecutive ones (in the
        // pass's round-robin order) are held and executed as one group
        // commit: one journal append and disk sync instead of one each (see
        // `Runtime::dispatch_group`). A command that cannot join a group is
        // executed after the group held before it, so commands still run in
        // the order the pass took them. Other messages are answered as they
        // come: they record nothing, and a peer's next message waits for
        // the next pass.
        let group_commits = native.is_none() && store.groups_commits();
        let mut group: Vec<(usize, fm_protocol::CommandMessage)> = Vec::new();
        // The native group the command turn takes, if any; its peers send
        // nothing else this pass.
        let mut native_commands = match command_turn {
            Some(turn) if store.groups_commits() => native_group(&mut peers, &close, turn),
            _ => Vec::new(),
        };
        let mut grouped = vec![false; peers.len()];
        for (index, _) in &native_commands {
            grouped[*index] = true;
        }
        for offset in 0..peers.len() {
            let index = (start + offset) % peers.len();
            if close[index] || peers[index].held_result.is_some() {
                continue;
            }
            if grouped[index] {
                if command_turn == Some(index)
                    && let Err(DispatchError::Daemon(error)) = runtime.dispatch_group(
                        &mut peers,
                        &mut close,
                        std::mem::take(&mut native_commands),
                        durable,
                        native.as_deref_mut(),
                    )
                {
                    shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
                    return Err(error);
                }
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
            let message = match message {
                WireMessage::Command(command)
                    if group_commits && peers[index].phase == Phase::Active =>
                {
                    // A Web title command is checked against the hidden
                    // fields on its own (see `Runtime::web_command_refusal`).
                    if group_commit_eligible(&command)
                        && !web_title_command(&peers[index], &command)
                    {
                        group.push((index, command));
                        continue;
                    }
                    if !group.is_empty()
                        && let Err(DispatchError::Daemon(error)) = runtime.dispatch_group(
                            &mut peers,
                            &mut close,
                            std::mem::take(&mut group),
                            durable,
                            None,
                        )
                    {
                        shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
                        return Err(error);
                    }
                    if close[index] {
                        continue;
                    }
                    WireMessage::Command(command)
                }
                message => message,
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
        if !group.is_empty()
            && let Err(DispatchError::Daemon(error)) =
                runtime.dispatch_group(&mut peers, &mut close, group, durable, None)
        {
            shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
            return Err(error);
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
        // A document whose status changed off this loop (a conversion
        // finished, or the daemon just started) reports it (IN-014).
        if documents.take_changed() {
            crate::documents::sync(&mut control.borrow_mut(), authority, &documents)?;
        }
        // Media list item lengths (Protocol 2.91): the worker learns every
        // item asset after each revision, and a changed report republishes
        // the lists it touches.
        let revision = control.borrow().revision();
        if media_lengths_revision != Some(revision) {
            media_lengths_revision = Some(revision);
            let assets = control.borrow().media_list_assets();
            media_lengths.want(assets, durable.project().settings().frame_rate);
        }
        if let Some(lengths) = media_lengths.take_changed() {
            control
                .borrow_mut()
                .set_media_list_lengths(authority, lengths)?;
        }
        // A page answer is lossy: one a peer cannot take now is dropped,
        // and the client asks again for what it still shows (IN-014).
        for (id, answer) in document_pages.take_finished() {
            if let Some(index) = peers.iter().position(|peer| peer.id == id)
                && !close[index]
                && peers[index].phase == Phase::Active
                && let Err(refusal) =
                    peers[index].queue(&WireMessage::PagedPage(answer), Accounting::Session)
                && !refusal.transient()
            {
                close[index] = true;
            }
        }
        // A listed device answer waits while its peer resynchronizes or
        // cannot take it yet; a peer that left drops it.
        for (id, answer) in screen_capture_targets.take_finished() {
            let Some(index) = peers.iter().position(|peer| peer.id == id) else {
                continue;
            };
            let peer = &mut peers[index];
            if close[index] {
                continue;
            }
            if peer.phase != Phase::Active || peer.held_result.is_some() {
                screen_capture_targets.defer(id, answer);
                continue;
            }
            match peer.queue(
                &WireMessage::ScreenCaptureTargets(answer.clone()),
                Accounting::Session,
            ) {
                Ok(()) => {}
                Err(refusal) if refusal.transient() => screen_capture_targets.defer(id, answer),
                Err(_) => close[index] = true,
            }
        }
        for (id, answer) in system_audio_devices.take_finished() {
            let Some(index) = peers.iter().position(|peer| peer.id == id) else {
                continue;
            };
            let peer = &mut peers[index];
            if close[index] {
                continue;
            }
            if peer.phase != Phase::Active || peer.held_result.is_some() {
                system_audio_devices.defer(id, answer);
                continue;
            }
            match peer.queue(
                &WireMessage::SystemAudioDevices(answer.clone()),
                Accounting::Session,
            ) {
                Ok(()) => {}
                Err(refusal) if refusal.transient() => system_audio_devices.defer(id, answer),
                Err(_) => close[index] = true,
            }
        }
        for (id, answer) in audio_output_devices.take_finished() {
            let Some(index) = peers.iter().position(|peer| peer.id == id) else {
                continue;
            };
            let peer = &mut peers[index];
            if close[index] {
                continue;
            }
            if peer.phase != Phase::Active || peer.held_result.is_some() {
                audio_output_devices.defer(id, answer);
                continue;
            }
            match peer.queue(
                &WireMessage::AudioOutputDevices(answer.clone()),
                Accounting::Session,
            ) {
                Ok(()) => {}
                Err(refusal) if refusal.transient() => audio_output_devices.defer(id, answer),
                Err(_) => close[index] = true,
            }
        }
        // The playlist clock runs before the native frame is realized, so a
        // boundary that is the pacer's next frame is cut by its advance. A
        // refused advance of either clock is retried only on a pass no
        // peer's command was ready for (the journal-idle check is the
        // clock's), so retries never take a client's frame or command turn.
        // While either daemon's frame loop (the native pacer, or the
        // simulated one) is still realizing a command's transition or
        // title phase, the clocks (and media end triggers)
        // wait for it to end, as they waited while a command realized those
        // frames itself: a boundary that falls inside is cut on the first
        // frame after it.
        let settling = control.borrow().work_in_flight();
        if settling && native.is_none() {
            // The simulated clocks still follow the show's frame while
            // they wait, so the frames the pacer realizes count as the
            // wall time they took.
            runtime.track_simulated_clocks();
        }
        if !settling
            && let Err(error) = runtime.playlist_clock.borrow_mut().poll(
                control,
                store,
                durable,
                authority,
                native.as_deref_mut(),
                Some(process_shutdown),
                Instant::now(),
                ready_turn.is_none(),
            )
        {
            shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
            return Err(error);
        }
        if !settling
            && let Err(error) = runtime.cue_clock.borrow_mut().poll(
                control,
                store,
                durable,
                authority,
                native.as_deref_mut(),
                Some(process_shutdown),
                Instant::now(),
                ready_turn.is_none(),
            )
        {
            shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
            return Err(error);
        }
        // Folder slideshows follow their folders on either daemon (IN-009).
        if let Some(assets) = runtime
            .titles
            .store()
            .map(fm_persistence::ProjectStore::assets_root)
        {
            runtime.slideshow_folders.borrow_mut().poll(
                &mut control.borrow_mut(),
                &assets,
                Instant::now(),
            );
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
        let ticked = match native.as_deref_mut() {
            Some(native) => native.tick_if_due(&mut control.borrow_mut(), authority),
            None => simulated_pacer.poll(
                &mut control.borrow_mut(),
                authority,
                durable.project().settings().frame_rate,
                durable.position().frames_rendered,
                Instant::now(),
            ),
        };
        if let Err(error) = ticked {
            shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
            return Err(error);
        }
        let settling = control.borrow().work_in_flight();
        if !settling
            && let Err(error) = runtime.end_triggers.borrow_mut().poll(
                control,
                store,
                durable,
                authority,
                native.as_deref_mut(),
                Some(process_shutdown),
                Instant::now(),
                ready_turn.is_none(),
            )
        {
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
            // A Wayland desktop's new screen capture grants (IN-011) are
            // kept in the project and folded at once.
            // Tokens the project refuses are offered again later.
            #[cfg(feature = "native-media")]
            {
                use crate::native_portal_capture::{Kept, keep_restore_tokens};
                let now = Instant::now();
                if let Some(tokens) = native.take_screen_capture_restore_tokens(now) {
                    match keep_restore_tokens(durable, tokens) {
                        Kept::Changed => {
                            if let Some(progress) = progress.as_mut() {
                                progress.request_fold();
                            }
                        }
                        Kept::Unchanged => {}
                        Kept::Refused => native.screen_capture_restore_tokens_refused(now),
                    }
                }
            }
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
            if let Some(status) = native.take_recorder_status(authority) {
                publish_recorder_status(&mut peers, status);
            }
            if let Some(status) = native.take_source_sync_status(authority) {
                publish_source_sync_status(&mut peers, &status);
            }
        }
        // A simulated daemon renders its audio plane only while a Studio
        // peer demands meters; the engine is never ticked for it (AU-005).
        if native.is_none()
            && let Some(audio) = simulated_audio.as_mut()
            && peers.iter().any(Peer::wants_audio_meters)
        {
            let mut control = control.borrow_mut();
            let program = control.realized_program_frame();
            let revision = control.revision();
            let resets = control.take_loudness_resets();
            drop(control);
            if let Some(meters) = audio.poll(
                durable.project(),
                revision,
                program,
                resets,
                true,
                Instant::now(),
            ) {
                simulated_audio_sequence =
                    simulated_audio_sequence.checked_add(1).ok_or_else(|| {
                        crate::AppFailure("simulated audio meter sequence exhausted".into())
                    })?;
                let view = crate::AudioMeterView::from(&meters);
                let message = crate::audio_meters_message(
                    authority,
                    simulated_audio_sequence,
                    &view,
                    fm_protocol::AudioMonitorOutputState::Off,
                );
                publish_audio_meters(&mut peers, message);
            }
        }
        // Both daemons announce their replay recording (OR-012).
        if peers.iter().any(|peer| peer.recorder_status) {
            let status = runtime.replay_pacer.borrow_mut().poll(
                &control.borrow(),
                authority,
                Instant::now(),
            );
            if let Some(status) = status {
                publish_replay_status(&mut peers, status);
            }
        }
        // Both daemons announce their highlight reels (OR-013).
        if peers.iter().any(|peer| peer.recorder_status) {
            let status =
                runtime
                    .reel_pacer
                    .borrow_mut()
                    .poll(&control.borrow(), authority, Instant::now());
            if let Some(status) = status {
                publish_reel_status(&mut peers, status);
            }
        }
        // Both daemons sample their named statistics once a second (RC-015).
        let now = Instant::now();
        if runtime.statistics.borrow().due(now) {
            let diagnostics = control.borrow().diagnostics();
            let sample = crate::statistics::ControlSample {
                subscribers: u64::try_from(diagnostics.subscriber_count).unwrap_or(u64::MAX),
                revision: diagnostics.current_revision,
            };
            let native_sample = native
                .as_deref_mut()
                .map(NativeDaemon::take_statistics_sample);
            let mut statistics = runtime.statistics.borrow_mut();
            statistics.sample(now, sample, native_sample);
            process_shutdown.publish_statistics(|| statistics.render());
            if let Some(status) = statistics.health_status(authority, now) {
                publish_health_status(&mut peers, &status);
            }
        }
        let phases_demand = peers.iter().any(Peer::wants_title_phases);
        if let Some(phases) = phase_pacer.poll(Instant::now(), phases_demand, || {
            control.borrow().title_phases()
        }) {
            publish_title_phases(&mut peers, phases);
        }
        let realizing = native.as_deref();
        release_held_results(&mut peers, &mut close, |slot| {
            realizing.is_some_and(|native| native.recording_settling(slot))
        });
        let monitor_demand = peers.iter().any(Peer::wants_monitor_frames);
        if let Some(native) = native.as_deref_mut() {
            native.set_monitor_demand(monitor_demand);
            for (tap, bytes) in native.take_monitor_frames() {
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
        for (key, bytes) in records {
            for peer in &mut peers {
                peer.offer_thumbnail(key, &bytes);
            }
        }
        for peer in &mut peers {
            peer.feed_thumbnail(now);
        }
        if let Some(reason) = requested_daemon_shutdown(native.as_deref(), Some(process_shutdown)) {
            return Ok(shutdown_for_reason(reason, &mut peers, control));
        }
        let hidden = peers
            .iter()
            .any(|peer| peer.transport.is_web())
            .then(|| crate::web_titles::HiddenFields::new(control.borrow().title_hidden_fields()));
        drain_live(
            &mut peers,
            &mut close,
            &mut live_budget,
            hidden.as_ref(),
            &mut |message| web_resync(&runtime, &control.borrow(), message),
        );

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

/// The simulated daemon's frame loop: it renders no idle frames, but once
/// a command has realized the first frame of a timed transition, Fade to
/// Black, overlay motion, or title phase (and been answered then), this
/// realizes the rest at the show's frame rate, as the native pacer does,
/// so every session keeps being served and the switcher realization is
/// published when the work ends. A command committed meanwhile lands on
/// the frame the work has reached, which its journal record names.
///
/// A scene's In or Out phase is not work in flight (the native pacer plays
/// it on idle frames), but the command's durable project settles after it
/// (`settled`, its frames rendered), so this realizes it too.
#[derive(Debug, Default)]
pub(super) struct SimulatedPacer {
    /// The frame cursor and the instant the work in flight was first seen.
    anchor: Option<(u64, Instant)>,
}

impl SimulatedPacer {
    /// Realizes the frames wall time has reached since the work in flight
    /// was first seen, stopping as soon as it settles: no work is in flight
    /// and the engine has reached `settled`, the frame the durable project
    /// says the commands end on.
    pub(super) fn poll(
        &mut self,
        control: &mut fm_control::ControlService<fm_auth::Policy>,
        server: &fm_protocol::ServerIdentity,
        rate: fm_types::FrameRate,
        settled: u64,
        now: Instant,
    ) -> crate::AppResult<()> {
        let busy = |control: &fm_control::ControlService<fm_auth::Policy>| {
            control.work_in_flight() || control.frame_cursor() < settled
        };
        if !busy(control) {
            self.anchor = None;
            return Ok(());
        }
        let (frame, at) = *self.anchor.get_or_insert((control.frame_cursor(), now));
        let target = crate::playlist_clock::wall_frame(frame, at, now, rate);
        while control.frame_cursor() < target && busy(control) {
            control.tick(server)?;
        }
        if !busy(control) {
            self.anchor = None;
        }
        Ok(())
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
        slideshows: crate::stills::SlidesShown::all(control),
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
    // at most one group of commands per frame across every peer) is not
    // read until its queue has room, so TCP backpressure slows it instead
    // of the daemon dropping it. Every message one read decodes is kept, so
    // the queue holds at most INBOUND_CAPACITY - 1 messages plus one read's
    // worth of lines.
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

/// The most commands a native pass executes as one group commit. A group's
/// events are published together once it is durable, into every peer's
/// bounded live subscription (64 events, drained a few per pass), so the
/// bound keeps a group's burst of events well inside it (a cut publishes
/// two; a group of 32 cuts overflowed it and dropped every peer as a slow
/// client). It also keeps the members' staging, which runs before the
/// group's append in front of its frame, short.
const NATIVE_GROUP_COMMANDS: usize = 8;

/// The commands a native pass whose command turn is `turn` executes as one
/// group commit, taken off their peers' queues: from `turn` round the peer
/// list, one command per active peer per round, as many rounds as needed,
/// so peers with commands queued share the group fairly. A peer contributes
/// the commands at the front of its queue until a message that is not a
/// [`native_group_eligible`] command (or a Web title command, checked on
/// its own), which waits for a later pass with everything behind it, so
/// each peer's messages are still handled in the order it sent them.
/// Returns nothing, and takes nothing, unless the turn's own command can
/// join and the group would hold at least two: a lone command takes its
/// turn as before group commit.
fn native_group(
    peers: &mut [Peer],
    close: &[bool],
    turn: usize,
) -> Vec<(usize, fm_protocol::CommandMessage)> {
    let joins = |peer: &Peer, at: usize| match peer.inbound.get(at) {
        Some(WireMessage::Command(command)) => {
            native_group_eligible(command) && !web_title_command(peer, command)
        }
        _ => false,
    };
    let mut open: Vec<usize> = (0..peers.len())
        .map(|offset| (turn + offset) % peers.len())
        .filter(|&index| {
            !close[index]
                && peers[index].held_result.is_none()
                && peers[index].phase == Phase::Active
        })
        .collect();
    if open.first() != Some(&turn) || !joins(&peers[turn], 0) {
        return Vec::new();
    }
    let mut taken = vec![0_usize; peers.len()];
    let mut order = Vec::new();
    while order.len() < NATIVE_GROUP_COMMANDS && !open.is_empty() {
        open.retain(|&index| {
            if order.len() >= NATIVE_GROUP_COMMANDS || !joins(&peers[index], taken[index]) {
                return false;
            }
            taken[index] += 1;
            order.push(index);
            true
        });
    }
    if order.len() < 2 {
        return Vec::new();
    }
    order
        .into_iter()
        .map(|index| match peers[index].inbound.pop_front() {
            Some(WireMessage::Command(command)) => (index, command),
            _ => unreachable!("a grouped peer's queue starts with its grouped commands"),
        })
        .collect()
}

/// The principal of a Web session: the operator's `FREEMIXD_WEB_TOKEN`
/// holds every role, a paired device only the roles of its pairing.
fn web_principal(identity: &crate::web::WebIdentity, peer_id: u64) -> Principal {
    let session = fm_auth::SessionId::new(format!("web-{peer_id}"))
        .expect("scheduler web session id is valid");
    match identity {
        crate::web::WebIdentity::StaticToken => Principal::authenticated(
            fm_auth::UserId::new("web-token").expect("stable web user is valid"),
            session,
            [fm_auth::Role::Admin],
        ),
        crate::web::WebIdentity::Device { id, role } => Principal::authenticated(
            fm_auth::UserId::new(id.as_str()).expect("device ids are valid user ids"),
            session,
            role.granted_roles().iter().copied(),
        ),
    }
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
    /// Lists system audio devices off this loop (Protocol 2.68).
    system_audio_devices: &'a crate::system_audio_devices::SystemAudioDevices,
    /// Lists headphone output devices off this loop (Protocol 2.102).
    audio_output_devices: &'a crate::audio_output_devices::AudioOutputDevices,
    /// The web gateway's pairing and devices (Protocol 2.71), when it runs.
    web_access: Option<&'a std::sync::Arc<crate::web::WebAccess>>,
    /// Answers document page requests off this loop (IN-014).
    document_pages: &'a crate::documents::PageWorker,
    /// Lists what the server can capture off this loop (IN-011).
    screen_capture_targets: &'a crate::screen_capture_targets::ScreenCaptureTargets,
    process_shutdown: &'a ProcessShutdown,
    /// The playlist clock; simulated snapshots read its wall-clock frame.
    playlist_clock: std::cell::RefCell<crate::playlist_clock::PlaylistClock>,
    /// The cue clock: runs waiting cue actions on their frames (RC-017).
    cue_clock: std::cell::RefCell<crate::cue_clock::CueClock>,
    /// Simulated runtime: plays media transports in wall time.
    media_clock: std::cell::RefCell<crate::media_clock::MediaClock>,
    /// Follows every folder slideshow's folder (IN-009).
    slideshow_folders: std::cell::RefCell<crate::stills::SlideshowFolders>,
    /// Feeds media end triggers to the playlist, retrying a refused one.
    end_triggers: std::cell::RefCell<crate::media_clock::EndTriggerFeed>,
    /// Compiled grades per engine revision for exact scene previews.
    grade_cache: std::cell::RefCell<crate::grading::GradeCache>,
    /// A peer subscribed to input thumbnails since the last poll, so every
    /// thumbnail is rendered and sent again at once (Protocol 2.59).
    thumbnail_refresh: std::cell::RefCell<bool>,
    /// The daemon's named statistics (RC-015), sampled once a second.
    statistics: std::cell::RefCell<crate::statistics::DaemonStatistics>,
    /// Paces `replay_status` (OR-012).
    replay_pacer: std::cell::RefCell<ReplayStatusPacer>,
    /// Paces `reel_status` (OR-013).
    reel_pacer: std::cell::RefCell<ReelStatusPacer>,
}

impl Runtime<'_> {
    /// A simulated daemon's pass that waits for the frame loop to settle:
    /// its playlist and cue clocks follow the frames the pacer realizes
    /// (see [`crate::playlist_clock::PlaylistClock::track`]).
    fn track_simulated_clocks(&self) {
        let control = self.control.borrow();
        self.playlist_clock.borrow_mut().track(&control);
        self.cue_clock.borrow_mut().track(&control);
    }

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
        if !simulated {
            return Ok(());
        }
        self.catch_up_simulated(&mut self.control.borrow_mut(), command, durable)
            .map_err(DispatchError::Daemon)
    }

    /// [`Self::catch_up_playlist`] for a simulated `control` (the live
    /// service, or a group commit's copy of it).
    fn catch_up_simulated(
        &self,
        control: &mut fm_control::ControlService<fm_auth::Policy>,
        command: &fm_protocol::CommandMessage,
        durable: &StoredProject,
    ) -> AppResult<()> {
        // A fired shortcut may operate a cue list or start or stop the
        // playlist (RC-004), so it honours both.
        let cue = matches!(
            command.payload,
            CommandPayload::CueControl { .. } | CommandPayload::FireShortcut { .. }
        );
        // A script run may start or stop the playlist too (RC-011).
        let playlist = matches!(
            command.payload,
            CommandPayload::PlaylistControl { .. }
                | CommandPayload::RunScript { .. }
                | CommandPayload::FireShortcut { .. }
        );
        if !(cue || playlist) {
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
                .catch_up(control, self.authority, rate, now)?;
        }
        self.playlist_clock
            .borrow()
            .catch_up(control, self.authority, rate, now)
    }

    /// Executes the commands of one pass as one group commit
    /// ([`execute_command_group`]): one journal append and disk sync for the
    /// group instead of one per command. The group runs in the pass's
    /// round-robin order, and each peer is answered, in its own order, only
    /// once the whole group is durable (or refused, when it could not be
    /// made so). A simulated daemon's group holds at most one command per
    /// peer; a native daemon's (`native`) may hold several of one peer's
    /// (see `native_group`), all begun on one frame. One command alone goes
    /// the single-command way.
    fn dispatch_group(
        &self,
        peers: &mut [Peer],
        close: &mut [bool],
        group: Vec<(usize, fm_protocol::CommandMessage)>,
        durable: &mut StoredProject,
        native: Option<&mut NativeDaemon>,
    ) -> Result<(), DispatchError> {
        if let [(index, _)] = group.as_slice() {
            let index = *index;
            let (_, command) = group.into_iter().next().expect("one command");
            return match self.dispatch(
                &mut peers[index],
                WireMessage::Command(command),
                durable,
                native,
            ) {
                Err(DispatchError::Peer) => {
                    close[index] = dropping(peers[index].id, "dispatch_refused");
                    Ok(())
                }
                result => result,
            };
        }
        let simulated = native.is_none();
        let members: Vec<GroupMember> = group
            .iter()
            .map(|(index, command)| GroupMember {
                principal: peers[*index].principal.clone(),
                server: peers[*index]
                    .identity
                    .clone()
                    .expect("active peers have identities"),
                // A live fire's Preview and Program are the authority's to
                // bind (RC-004), as for a command executed alone.
                command: fm_control::client_fire_payload(&command.payload).map_or_else(
                    || command.clone(),
                    |payload| fm_protocol::CommandMessage {
                        payload,
                        ..command.clone()
                    },
                ),
            })
            .collect();
        // A command holds an in-flight slot of its session from its
        // admission until it completed. A peer with several commands in the
        // group has the slot of its previous one released before the next
        // is admitted, as when they run one by one, so a group never holds
        // more of a session's slots than one-by-one execution would.
        let mut holding: Vec<Option<usize>> = vec![None; peers.len()];
        let mut released = vec![false; members.len()];
        let outcomes = execute_command_group(
            self.control,
            self.store,
            durable,
            &members,
            native,
            Some(self.process_shutdown),
            |member, control, staged, now| {
                let command = &members[member].command;
                let index = group[member].0;
                let session = peers[index]
                    .session
                    .as_mut()
                    .expect("active peers have sessions");
                if let Some(previous) = holding[index].take() {
                    session.command_completed()?;
                    released[previous] = true;
                }
                if simulated {
                    self.catch_up_simulated(control, command, staged)?;
                }
                let refusal = admit_session_command(session, control, command, now)?;
                if refusal.is_none() {
                    holding[index] = Some(member);
                }
                Ok(refusal)
            },
        )
        .map_err(DispatchError::Daemon)?;
        self.sync_title_templates();
        for (member, ((index, _), outcome)) in group.iter().zip(outcomes).enumerate() {
            let peer = &mut peers[*index];
            if close[*index] {
                continue;
            }
            if outcome.admitted
                && !released[member]
                && let Err(error) = peer
                    .session
                    .as_mut()
                    .expect("active peers have sessions")
                    .command_completed()
            {
                if is_client_session_termination(&error) {
                    close[*index] = dropping(peer.id, "dispatch_refused");
                    continue;
                }
                return Err(DispatchError::Daemon(error.into()));
            }
            let mut result = outcome.delivery.result;
            if peer.transport.is_web() {
                crate::web_titles::web_result(&mut result);
            }
            if peer.reply(WireMessage::CommandResult(result)).is_err() {
                close[*index] = dropping(peer.id, "dispatch_refused");
            }
        }
        Ok(())
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
            WireMessage::Command(mut command) => {
                // Caught up first, so a Web fire is checked against the
                // show it runs on.
                self.catch_up_playlist(&command, durable, native.is_none())?;
                if let Some(refused) = self.web_command_refusal(peer, &mut command) {
                    peer.reply(WireMessage::CommandResult(refused))?;
                    return Ok(());
                }
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
                let CommandDelivery { mut result, .. } = delivery;
                if peer.transport.is_web() {
                    crate::web_titles::web_result(&mut result);
                }
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
                        diagnostics_response(
                            self.control,
                            request,
                            native
                                .as_deref()
                                .map_or_else(Vec::new, NativeDaemon::source_sync_samples),
                            self.statistics.borrow().health(Instant::now()),
                        )
                        .map_err(|_| ())?,
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
            // answers that it has no presenter. Like every device inventory
            // it is answered only to a session that may edit the project.
            WireMessage::DisplayInventoryRequest(request) => {
                if self.refuse_request(
                    peer,
                    &request.request_id,
                    Session::admit_device_inventory,
                )? {
                    return Ok(());
                }
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
                if self.refuse_request(
                    peer,
                    &request.request_id,
                    Session::admit_device_inventory,
                )? {
                    return Ok(());
                }
                let inventory = self.control.borrow().capture_inventory().clone();
                peer.reply(WireMessage::CaptureDevices(
                    fm_protocol::CaptureDevicesMessage {
                        request_id: request.request_id,
                        inventory,
                    },
                ))?;
                Ok(())
            }
            // Web device pairing (RC-007, Protocol 2.71): only a raw
            // control session holding manage_users may manage it, never a
            // session relayed by the gateway itself.
            WireMessage::WebAccessRequest(request) => {
                let session = peer.session.as_mut().expect("active peers have sessions");
                let (refusal, gateway) = if peer.transport.is_web() {
                    (Some(fm_protocol::WebAccessRefusal::WebSession), None)
                } else {
                    match session.admit_web_access(now_millis().map_err(|_| ())?) {
                        Err(SessionError::Authorization(_)) => {
                            (Some(fm_protocol::WebAccessRefusal::PermissionDenied), None)
                        }
                        Err(error) => {
                            let current_revision =
                                Some(self.control.borrow().diagnostics().current_revision);
                            peer.reply(WireMessage::Error(ErrorMessage {
                                request_id: Some(request.request_id),
                                current_revision,
                                error: structured_session_error(&error),
                            }))?;
                            return Ok(());
                        }
                        Ok(()) => match self.web_access {
                            None => (Some(fm_protocol::WebAccessRefusal::Unavailable), None),
                            Some(access) => {
                                let (refusal, status) = access.handle(&request.action);
                                (refusal, Some(status))
                            }
                        },
                    }
                };
                peer.reply(WireMessage::WebAccess(fm_protocol::WebAccessMessage {
                    request_id: request.request_id,
                    refusal,
                    gateway,
                }))?;
                Ok(())
            }
            // One window of a slideshow's slide list (IN-009), only to a
            // session that steps or edits presentations.
            WireMessage::SlideshowSlidesRequest(request) => {
                if self.refuse_request(
                    peer,
                    &request.request_id,
                    Session::admit_presentation_content,
                )? {
                    return Ok(());
                }
                let slides = self.control.borrow().slideshow_slides(&request);
                peer.reply(WireMessage::SlideshowSlides(slides))?;
                Ok(())
            }
            // The server's system audio devices (AU-011, Protocol 2.68) are
            // listed off this loop and sent on a later pass.
            // What the server can capture (IN-011) is listed off this loop
            // and sent on a later pass. It names every open window's
            // application and title, so only a session that may edit the
            // project (and so add a screen capture input) is answered.
            WireMessage::ScreenCaptureTargetsRequest(request) => {
                let session = peer.session.as_ref().expect("active peers have sessions");
                if let Err(error) = session.admit_screen_capture_targets() {
                    let current_revision =
                        Some(self.control.borrow().diagnostics().current_revision);
                    peer.reply(WireMessage::Error(ErrorMessage {
                        request_id: Some(request.request_id),
                        current_revision,
                        error: structured_session_error(&error),
                    }))?;
                    return Ok(());
                }
                self.screen_capture_targets
                    .submit(peer.id, request.request_id);
                Ok(())
            }
            // A new pick of a screen capture input's target (IN-011,
            // Protocol 2.85).
            WireMessage::ScreenCapturePickRequest(request) => {
                let session = peer.session.as_mut().expect("active peers have sessions");
                if let Err(error) = session.admit_screen_capture_pick(now_millis().map_err(|_| ())?)
                {
                    let current_revision =
                        Some(self.control.borrow().diagnostics().current_revision);
                    peer.reply(WireMessage::Error(ErrorMessage {
                        request_id: Some(request.request_id),
                        current_revision,
                        error: structured_session_error(&error),
                    }))?;
                    return Ok(());
                }
                let input = request.input.to_domain();
                let capture = self
                    .control
                    .borrow()
                    .screen_capture_inputs()
                    .iter()
                    .any(|(candidate, _)| *candidate == input);
                if !capture {
                    peer.reply(WireMessage::Error(ErrorMessage {
                        request_id: Some(request.request_id),
                        current_revision: None,
                        error: StructuredError {
                            code: "not_screen_capture".into(),
                            message: format!("input {input} is not a screen capture input"),
                            fields: Vec::new(),
                            retryable: false,
                        },
                    }))?;
                    return Ok(());
                }
                // A simulated daemon captures nothing, so there is nothing
                // to pick again.
                if let Some(native) = native {
                    native.pick_screen_capture(input);
                }
                Ok(())
            }
            // The replay status (OR-012), to any session that may view
            // status.
            WireMessage::ReplayStatusRequest(request) => {
                if self.refuse_request(peer, &request.request_id, Session::admit_view_status)? {
                    return Ok(());
                }
                let sequence = self.replay_pacer.borrow_mut().next_sequence();
                let status = self.control.borrow().replay_status(
                    self.authority,
                    sequence,
                    Some(request.request_id),
                );
                peer.reply(WireMessage::ReplayStatus(Box::new(status)))?;
                Ok(())
            }
            // The highlight reel status (OR-013), to any session that may
            // view status.
            WireMessage::ReelStatusRequest(request) => {
                if self.refuse_request(peer, &request.request_id, Session::admit_view_status)? {
                    return Ok(());
                }
                let sequence = self.reel_pacer.borrow_mut().next_sequence();
                let status = self.control.borrow().reel_status(
                    self.authority,
                    sequence,
                    Some(request.request_id),
                );
                peer.reply(WireMessage::ReelStatus(Box::new(status)))?;
                Ok(())
            }
            WireMessage::SystemAudioDevicesRequest(request) => {
                if self.refuse_request(
                    peer,
                    &request.request_id,
                    Session::admit_device_inventory,
                )? {
                    return Ok(());
                }
                self.system_audio_devices
                    .submit(peer.id, request.request_id);
                Ok(())
            }
            // The headphone output devices (AU-001, Protocol 2.102), listed
            // off this loop like the system audio devices, and only for a
            // session that may choose the device.
            WireMessage::AudioOutputDevicesRequest(request) => {
                if self.refuse_request(
                    peer,
                    &request.request_id,
                    Session::admit_device_inventory,
                )? {
                    return Ok(());
                }
                self.audio_output_devices
                    .submit(peer.id, request.request_id);
                Ok(())
            }
            // A document page's notes or thumbnail (IN-014) are read off
            // this loop and sent on a later pass, only to a session that
            // steps or edits presentations.
            WireMessage::PagedPageRequest(request) => {
                if self.refuse_request(
                    peer,
                    &request.request_id,
                    Session::admit_presentation_content,
                )? {
                    return Ok(());
                }
                let input = request.input.to_domain();
                let uri = self
                    .control
                    .borrow()
                    .document_inputs()
                    .into_iter()
                    .find_map(|(document, uri)| (document == input).then_some(uri));
                self.document_pages.submit(peer.id, uri, request);
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

    /// Answers a read-only request that `admit` refuses with an error
    /// naming its `request_id` (`permission_denied` for a missing
    /// permission) and returns whether it was refused.
    fn refuse_request(
        &self,
        peer: &mut Peer,
        request_id: &str,
        admit: impl FnOnce(&Session) -> Result<(), SessionError>,
    ) -> Result<bool, DispatchError> {
        let session = peer.session.as_ref().expect("active peers have sessions");
        let Err(error) = admit(session) else {
            return Ok(false);
        };
        let current_revision = Some(self.control.borrow().diagnostics().current_revision);
        peer.reply(WireMessage::Error(ErrorMessage {
            request_id: Some(request_id.to_owned()),
            current_revision,
            error: structured_session_error(&error),
        }))?;
        Ok(true)
    }

    /// The refusal a Web peer's command meets before it executes (see
    /// [`crate::web_titles`]); `None` for raw peers and allowed commands. A
    /// Web peer's cue list or shortcut edit gets back the stored values
    /// its hidden markers stand for, and its fire may not write a value
    /// hidden from it where it can read it (Protocol 2.104).
    fn web_command_refusal(
        &self,
        peer: &Peer,
        command: &mut fm_protocol::CommandMessage,
    ) -> Option<fm_protocol::CommandResult> {
        if !web_title_command(peer, command) {
            return None;
        }
        let control = self.control.borrow();
        let hidden = crate::web_titles::HiddenFields::new(control.title_hidden_fields());
        let current_revision = control.diagnostics().current_revision;
        if crate::web_titles::is_definition_edit(command) {
            let stored = crate::web_titles::StoredDefinitions {
                cue_lists: control
                    .cue_list_states()
                    .into_iter()
                    .map(|state| state.list)
                    .collect(),
                shortcuts: control
                    .shortcut_states()
                    .into_iter()
                    .map(|state| state.shortcut)
                    .collect(),
            };
            return crate::web_titles::web_definition_edit(
                command,
                &hidden,
                &stored,
                current_revision,
            );
        }
        if crate::web_titles::is_fire(command) {
            return crate::web_titles::web_fire_refusal(
                command,
                &control.fire_title_writes(&command.payload),
                &hidden,
                &crate::web_titles::visible_fields(&control.title_states()),
                current_revision,
            );
        }
        crate::web_titles::web_command_refusal(command, &hidden, current_revision)
    }

    /// The fields titles hide from Web sessions, when `web`.
    fn web_hidden(&self, web: bool) -> Option<crate::web_titles::HiddenFields> {
        web.then(|| {
            crate::web_titles::HiddenFields::new(self.control.borrow().title_hidden_fields())
        })
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
        // A Web page may ask for small title previews through its status
        // feed (Protocol 2.90); scene previews stay raw-only.
        let web = peer.transport.is_web();
        if web && (matches!(request, PreviewRequest::Scene(_)) || peer.web_feed.is_none()) {
            peer.reply(WireMessage::Error(ErrorMessage {
                request_id: Some(request_id),
                current_revision: None,
                error: StructuredError {
                    code: "unsupported".into(),
                    message: "scene previews are served to raw protocol sessions only".into(),
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
                        web,
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
    #[allow(clippy::too_many_lines)] // one extend per follow-on record kind
    fn complete_snapshot(&self, peer: &mut Peer) {
        if matches!(peer.initial_sync.front(), Some(WireMessage::Snapshot(_))) {
            let control = self.control.borrow();
            // A Web page is never sent a field its title hides (2.90).
            let web = peer.transport.is_web();
            peer.initial_sync
                .extend(control.title_states().into_iter().map(|state| {
                    WireMessage::TitleState(if web {
                        crate::web_titles::web_title_state(state)
                    } else {
                        state
                    })
                }));
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
            // A Web page is never sent a hidden field's value in a cue list
            // or shortcut, nor any script's source (Protocol 2.104).
            let hidden = self.web_hidden(web);
            let web_record = |message: WireMessage| match &hidden {
                Some(hidden) => crate::web_titles::web_message(message, hidden),
                None => message,
            };
            peer.initial_sync.extend(
                self.cue_list_states(&control)
                    .into_iter()
                    .map(|state| web_record(WireMessage::CueListState(state))),
            );
            peer.initial_sync.extend(
                control
                    .script_states()
                    .into_iter()
                    .map(|state| web_record(WireMessage::ScriptState(state))),
            );
            peer.initial_sync.extend(
                control
                    .shortcut_states()
                    .into_iter()
                    .map(|state| web_record(WireMessage::ShortcutState(state))),
            );
            peer.initial_sync.extend(
                control
                    .still_states()
                    .into_iter()
                    .map(WireMessage::StillState),
            );
            peer.initial_sync.extend(
                control
                    .media_list_states()
                    .into_iter()
                    .map(WireMessage::MediaListState),
            );
            peer.initial_sync.extend(
                control
                    .strip_dsp_states()
                    .into_iter()
                    .map(WireMessage::AudioStripDspState),
            );
            peer.initial_sync.extend(
                control
                    .paged_states()
                    .into_iter()
                    .map(WireMessage::PagedState),
            );
        }
    }

    /// Every cue list's state. A waiting cue's countdown reads the show's
    /// wall-clock frame on the simulated daemon, as the playlist state does.
    fn cue_list_states(
        &self,
        control: &fm_control::ControlService<fm_auth::Policy>,
    ) -> Vec<fm_protocol::CueListStateMessage> {
        let rate = control.snapshot().snapshot.frame_rate;
        control
            .cue_list_states()
            .into_iter()
            .map(|mut state| {
                state.frame =
                    self.cue_clock
                        .borrow()
                        .observed_frame(state.frame, rate, Instant::now());
                state
            })
            .collect()
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

    #[allow(clippy::too_many_lines)] // one handshake: authorize, admit, then subscribe
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
            // Resumed events reach a Web page filtered as live ones do,
            // and followed by what the live path would have sent after
            // them: the current state of each title they name and, after
            // an exposure change, the cue lists and shortcuts that set
            // title fields (see `web_resync`).
            SyncPayload::Resume(events) => {
                let hidden = self.web_hidden(peer.transport.is_web());
                let mut inputs = Vec::new();
                let mut exposure = false;
                for event in events {
                    let message = WireMessage::Event(event);
                    let Some(hidden) = &hidden else {
                        peer.initial_sync.push_back(message);
                        continue;
                    };
                    for input in crate::web_titles::resync_inputs(&message) {
                        if !inputs.contains(&input) {
                            inputs.push(input);
                        }
                    }
                    exposure |= crate::web_titles::exposure_changed(&message);
                    peer.initial_sync
                        .push_back(crate::web_titles::web_message(message, hidden));
                }
                if let Some(hidden) = &hidden {
                    let records =
                        web_resync_records(self, &self.control.borrow(), &inputs, exposure);
                    peer.initial_sync.extend(
                        records
                            .into_iter()
                            .map(|record| crate::web_titles::web_message(record, hidden)),
                    );
                }
            }
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
        peer.recorder_status = peer.audio_meters;
        // Web pages whose role may view status get the bounded lossy feed:
        // Master meters, stream and recorder status (Protocol 2.66).
        peer.web_feed = (peer.transport.is_web()
            && handshake
                .server_hello
                .permissions
                .iter()
                .any(|permission| permission == VIEW_STATUS_PERMISSION))
        .then(WebFeed::default);
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

/// Whether `command` is a Web peer's title command, cue list, shortcut,
/// or script edit, or cue or shortcut fire (Protocol 2.104), that must
/// first be checked against the fields titles hide (see
/// [`crate::web_titles`]).
fn web_title_command(peer: &Peer, command: &fm_protocol::CommandMessage) -> bool {
    peer.transport.is_web()
        && (matches!(
            command.payload,
            CommandPayload::SetTitleFieldExposure { .. }
                | CommandPayload::SetTitleFields { .. }
                | CommandPayload::TakeTitle { .. }
        ) || crate::web_titles::is_definition_edit(command)
            || crate::web_titles::is_fire(command))
}

/// The records that resynchronize a Web session after `message`, unfiltered
/// (see [`drain_live`]): one `title_state` per title it names (see
/// [`crate::web_titles::resync_inputs`]) as the session may now see it, and
/// after a `title_exposure_changed` one `cue_list_state` per cue list and
/// one `shortcut_state` per shortcut that sets a title field (Protocol
/// 2.104), whose hidden values may have changed.
fn web_resync(
    runtime: &Runtime<'_>,
    control: &fm_control::ControlService<fm_auth::Policy>,
    message: &WireMessage,
) -> Vec<WireMessage> {
    web_resync_records(
        runtime,
        control,
        &crate::web_titles::resync_inputs(message),
        crate::web_titles::exposure_changed(message),
    )
}

/// The resynchronizing records of [`web_resync`] for the titles `inputs`
/// and, with `exposure`, the cue lists and shortcuts too: what follows a
/// live event, or the events a resumed Web session is replayed.
fn web_resync_records(
    runtime: &Runtime<'_>,
    control: &fm_control::ControlService<fm_auth::Policy>,
    inputs: &[fm_protocol::WireInputId],
    exposure: bool,
) -> Vec<WireMessage> {
    let mut records: Vec<WireMessage> = if inputs.is_empty() {
        Vec::new()
    } else {
        crate::web_titles::web_title_states(control.title_states(), inputs)
            .into_iter()
            .map(WireMessage::TitleState)
            .collect()
    };
    if exposure {
        records.extend(
            runtime
                .cue_list_states(control)
                .into_iter()
                .filter(|state| {
                    state
                        .list
                        .spec
                        .cues
                        .iter()
                        .any(|cue| crate::web_titles::sets_title_values(&cue.actions))
                })
                .map(WireMessage::CueListState),
        );
        records.extend(
            control
                .shortcut_states()
                .into_iter()
                .filter(|state| {
                    matches!(
                        &state.shortcut.spec.action,
                        fm_protocol::WireShortcutAction::Actions(actions)
                            if crate::web_titles::sets_title_values(actions)
                    )
                })
                .map(WireMessage::ShortcutState),
        );
    }
    records
}

/// Queues each active peer's live events, up to its budget. A Web peer's
/// title records leave out the fields their titles hide (`hidden`, present
/// whenever a Web peer is connected), and its `title_exposure_changed` event
/// is followed by one `title_state` per title it names, as the Web session
/// may now see it (`title_states`, Protocol 2.96), so a newly exposed
/// field's name and value reach the page without a reconnect. A cue,
/// script, or shortcut run that set title fields is followed the same way
/// by each title it set, so the page's values and field revisions follow.
/// Its cue lists, shortcuts, and scripts are redacted (Protocol 2.104), and
/// an exposure change is also followed by the cue lists and shortcuts that
/// set title fields. `resync` names those follow-on records (see
/// [`web_resync`]); each is redacted as the event is.
fn drain_live(
    peers: &mut [Peer],
    close: &mut [bool],
    budgets: &mut [usize],
    hidden: Option<&crate::web_titles::HiddenFields>,
    resync: &mut dyn FnMut(&WireMessage) -> Vec<WireMessage>,
) {
    for (index, peer) in peers.iter_mut().enumerate() {
        // A held acknowledgement precedes the events its command published.
        if close[index] || peer.phase != Phase::Active || peer.held_result.is_some() {
            continue;
        }
        while budgets[index] > 0 && peer.outbound.len() < OUTBOUND_CAPACITY - REPLY_RESERVE {
            let message = if let Some(message) = peer.deferred_live.pop_front() {
                message
            } else {
                match peer
                    .subscription
                    .as_ref()
                    .expect("active peers have subscriptions")
                    .try_recv()
                {
                    Ok(LiveEvent::Durable(event)) => match hidden {
                        Some(hidden) if peer.transport.is_web() => {
                            let message = WireMessage::Event(event);
                            // Named before filtering: a run that set only a
                            // hidden field still moves the field revision.
                            peer.deferred_live.extend(
                                resync(&message)
                                    .into_iter()
                                    .map(|record| crate::web_titles::web_message(record, hidden)),
                            );
                            crate::web_titles::web_live_message(message, hidden)
                        }
                        _ => WireMessage::Event(event),
                    },
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
                    peer.deferred_live.push_front(message);
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
        phase,
        web_feed,
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
    // One lossy feed record joins the queue only while it is nearly idle
    // and none is in flight: everything else the peer is owed goes first,
    // and a peer that stops reading stops receiving feed records.
    if *phase == Phase::Active
        && outbound.len() <= WEB_FEED_MAX_QUEUED
        && !outbound.iter().any(|record| record.feed)
        && let Some(bytes) = web_feed.as_mut().and_then(|feed| feed.next(Instant::now()))
    {
        outbound.push_back(Outbound {
            write: OutboundWrite::Web(bytes),
            accounting: Accounting::Raw,
            handshake_response: false,
            command_result: false,
            accounted: false,
            channel_sent: false,
            feed: true,
        });
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
    // Web pages take only the Master meters, encoded once for all of them.
    if peers.iter().any(|peer| peer.web_feed.is_some())
        && let Ok(web) = encode_line(&WireMessage::AudioMeters(web_meters(&meters)))
    {
        for peer in peers.iter_mut() {
            peer.replace_web_meter(web.as_bytes());
        }
    }
    let Ok(bytes) = encode_line(&WireMessage::AudioMeters(meters)).map(String::into_bytes) else {
        return;
    };
    for peer in peers {
        peer.replace_meter(bytes.clone());
    }
}

/// Title animation phases (Protocol 2.90) go to Web pages and raw Studio
/// sessions, latest-wins.
fn publish_title_phases(peers: &mut [Peer], phases: fm_protocol::TitlePhasesMessage) {
    let Ok(bytes) = encode_line(&WireMessage::TitlePhases(phases)).map(String::into_bytes) else {
        return;
    };
    for peer in peers {
        peer.replace_title_phases(&bytes);
    }
}

/// The replay status (OR-012) goes to raw Studio peers in its own
/// latest-wins slot.
fn publish_replay_status(peers: &mut [Peer], status: fm_protocol::ReplayStatusMessage) {
    let Ok(bytes) =
        encode_line(&WireMessage::ReplayStatus(Box::new(status))).map(String::into_bytes)
    else {
        return;
    };
    for peer in peers {
        peer.replace_replay_status(bytes.clone());
    }
}

/// The highlight reel status (OR-013) goes to raw Studio peers in its own
/// latest-wins slot.
fn publish_reel_status(peers: &mut [Peer], status: fm_protocol::ReelStatusMessage) {
    let Ok(bytes) = encode_line(&WireMessage::ReelStatus(Box::new(status))).map(String::into_bytes)
    else {
        return;
    };
    for peer in peers {
        peer.replace_reel_status(bytes.clone());
    }
}

/// Paces `replay_status` (OR-012): at most four a second while it changes,
/// one a second otherwise (the live edge moves every frame).
#[derive(Debug, Default)]
pub(crate) struct ReplayStatusPacer {
    sequence: u64,
    generation: Option<u64>,
    sent_at: Option<Instant>,
}

impl ReplayStatusPacer {
    const CHANGED: std::time::Duration = std::time::Duration::from_millis(250);
    const IDLE: std::time::Duration = std::time::Duration::from_secs(1);

    /// The next status to publish, if one is due.
    fn poll(
        &mut self,
        control: &fm_control::ControlService,
        authority: &ServerIdentity,
        now: Instant,
    ) -> Option<fm_protocol::ReplayStatusMessage> {
        let generation = control.replay_generation();
        let elapsed = self.sent_at.map(|sent| now.saturating_duration_since(sent));
        let due = match elapsed {
            None => true,
            Some(elapsed) if self.generation != Some(generation) => elapsed >= Self::CHANGED,
            Some(elapsed) => elapsed >= Self::IDLE,
        };
        if !due {
            return None;
        }
        self.generation = Some(generation);
        self.sent_at = Some(now);
        self.sequence = self.sequence.wrapping_add(1);
        Some(control.replay_status(authority, self.sequence, None))
    }

    /// The sequence of a status answering a request.
    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }
}

/// Paces `reel_status` (OR-013): at most four a second while it changes,
/// one a second otherwise (export progress moves while a job runs).
#[derive(Debug, Default)]
pub(crate) struct ReelStatusPacer {
    sequence: u64,
    generation: Option<u64>,
    sent_at: Option<Instant>,
}

impl ReelStatusPacer {
    const CHANGED: std::time::Duration = std::time::Duration::from_millis(250);
    const IDLE: std::time::Duration = std::time::Duration::from_secs(1);

    /// The next status to publish, if one is due.
    fn poll(
        &mut self,
        control: &fm_control::ControlService,
        authority: &ServerIdentity,
        now: Instant,
    ) -> Option<fm_protocol::ReelStatusMessage> {
        let generation = control.reel_generation();
        let elapsed = self.sent_at.map(|sent| now.saturating_duration_since(sent));
        let due = match elapsed {
            None => true,
            Some(elapsed) if self.generation != Some(generation) => elapsed >= Self::CHANGED,
            Some(elapsed) => elapsed >= Self::IDLE,
        };
        if !due {
            return None;
        }
        self.generation = Some(generation);
        self.sent_at = Some(now);
        self.sequence = self.sequence.wrapping_add(1);
        Some(control.reel_status(authority, self.sequence, None))
    }

    /// The sequence of a status answering a request.
    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }
}

fn publish_recorder_status(peers: &mut [Peer], status: RecorderStatusMessage) {
    let Ok(bytes) = encode_line(&WireMessage::RecorderStatus(status)).map(String::into_bytes)
    else {
        return;
    };
    for peer in peers {
        peer.replace_recorder_status(bytes.clone());
    }
}

/// Live source drift telemetry (Protocol 2.76) goes to raw Studio peers
/// in its own latest-wins slot and (Protocol 2.101) to Web pages' feeds.
fn publish_source_sync_status(peers: &mut [Peer], status: &SourceSyncStatusMessage) {
    let Ok(bytes) =
        encode_line(&WireMessage::SourceSyncStatus(status.clone())).map(String::into_bytes)
    else {
        return;
    };
    for peer in peers {
        peer.replace_source_sync_status(bytes.clone());
    }
}

/// Daemon health (Protocol 2.100) goes to raw Studio peers in its own
/// latest-wins slot, once a statistics sample.
fn publish_health_status(peers: &mut [Peer], status: &fm_protocol::HealthStatusMessage) {
    let Ok(bytes) = encode_line(&WireMessage::HealthStatus(status.clone())).map(String::into_bytes)
    else {
        return;
    };
    for peer in peers {
        peer.replace_health_status(bytes.clone());
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
    // A Web peer's feed records not handed over yet are dropped, and no
    // further one follows the goodbye.
    peer.web_feed = None;
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
            dynamics: Vec::new(),
            loudness: Vec::new(),
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

        // Web transports never use the raw lossy slots; a Web page's status
        // feed is its own (see `web_status_feed_is_lossy_paced_...`).
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

    fn recorder_status_message(sequence: u64) -> RecorderStatusMessage {
        RecorderStatusMessage {
            server: identity(),
            sequence,
            samples: vec![
                fm_protocol::RecorderStatusSample {
                    recorder: fm_protocol::WireRecorderSlot::new(1).unwrap(),
                    realized: fm_protocol::RecorderRealizedState::Recording,
                    elapsed_ms: 12_345,
                    failure: None,
                },
                fm_protocol::RecorderStatusSample {
                    recorder: fm_protocol::WireRecorderSlot::new(2).unwrap(),
                    realized: fm_protocol::RecorderRealizedState::Failed,
                    elapsed_ms: 0,
                    failure: Some(fm_protocol::OutputFailureCode::FolderNotWritable.into()),
                },
            ],
        }
    }

    fn reel_status_message(sequence: u64) -> fm_protocol::ReelStatusMessage {
        fm_protocol::ReelStatusMessage {
            server: identity(),
            sequence,
            request_id: None,
            reels: vec![fm_protocol::ReelStatus {
                id: 1,
                spec: fm_protocol::ReelSpec {
                    id: 1,
                    name: "Goals".to_owned(),
                    clips: vec![fm_protocol::ReelClip {
                        event: 1,
                        angle: fm_protocol::WireInputId::new(NonZeroU128::new(1).unwrap())
                            .to_domain(),
                        trim_in_frames: 0,
                        trim_out_frames: 0,
                        speed_percent: 100,
                    }],
                },
                total_frames: Some(300),
                error: None,
            }],
            exports: vec![fm_protocol::ReelExportStatus {
                job: 1,
                reel: 1,
                destination: "/srv/reels/goals.mp4".to_owned(),
                state: fm_protocol::ReelExportState::Running,
                completed_frames: 120,
                total_frames: Some(300),
                bytes: None,
                failure: None,
            }],
        }
    }

    /// Protocol 2.66: a Web peer that may view status gets a lossy feed of
    /// Master meters, recorder status, and stream status. One feed record is
    /// handed over at a time, only behind a nearly idle queue and never
    /// charged to the rate window, so everything else the peer is owed goes
    /// first; a peer that does not acknowledge it gets no further one, and
    /// a newer record of a kind replaces the waiting one.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn web_status_feed_is_lossy_paced_and_never_ahead_of_owed_records() {
        let (mut peer, outbound, acknowledge, _inbound) = web_peer();
        peer.web_feed = Some(WebFeed::default());
        let handed = |outbound: &mpsc::Receiver<Vec<u8>>| -> Vec<WireMessage> {
            outbound
                .try_iter()
                .flat_map(|bytes| LineDecoder::new().push(&bytes).unwrap())
                .collect()
        };
        let mut meters = meters_message();
        meters.master.push(AudioMeterChannel {
            peak_millionths: 900_000,
            rms_millionths: 400_000,
        });
        meters.inputs = vec![fm_protocol::InputAudioMeters {
            input: fm_protocol::WireInputId::new(NonZeroU128::new(3).unwrap()),
            channels: meters.master.clone(),
        }];
        meters.monitor = Some(fm_protocol::AudioMonitorMeters {
            solo: false,
            output: fm_protocol::AudioMonitorOutputState::Off,
            channels: meters.master.clone(),
        });
        let mut older = meters.clone();
        older.sequence = 6;
        publish_audio_meters(std::slice::from_mut(&mut peer), older);
        publish_audio_meters(std::slice::from_mut(&mut peer), meters.clone());
        publish_recorder_status(std::slice::from_mut(&mut peer), recorder_status_message(1));
        publish_stream_status(std::slice::from_mut(&mut peer), status_message());
        // Web peers never use the raw lossy slots.
        assert!(peer.lossy.is_empty());

        // A busy queue goes first; no feed record joins it.
        for index in 0..3 {
            peer.queue(&filler(index), Accounting::Session).unwrap();
        }
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        assert_eq!(handed(&outbound), (0..3).map(filler).collect::<Vec<_>>());
        for _ in 0..3 {
            acknowledge.send(()).unwrap();
        }

        // Idle: the newest Master-only meter record is handed over.
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        let [WireMessage::AudioMeters(web)] = &handed(&outbound)[..] else {
            panic!("expected one meter record");
        };
        assert_eq!(web.sequence, 7);
        assert_eq!(web.master, meters.master);
        assert!(web.inputs.is_empty() && web.monitor.is_none());

        // Unacknowledged, it holds back every further feed record, but not
        // a command result.
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        assert_eq!(handed(&outbound), Vec::<WireMessage>::new());
        peer.queue(&command_result("cut"), Accounting::Session)
            .unwrap();
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        assert_eq!(handed(&outbound), [command_result("cut")]);

        // Acknowledged, the statuses follow one at a time: recorders first.
        publish_recorder_status(std::slice::from_mut(&mut peer), recorder_status_message(2));
        for _ in 0..2 {
            acknowledge.send(()).unwrap();
        }
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        assert_eq!(
            handed(&outbound),
            [WireMessage::RecorderStatus(recorder_status_message(2))]
        );
        acknowledge.send(()).unwrap();
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        assert_eq!(
            handed(&outbound),
            [WireMessage::StreamStatus(status_message())]
        );
        acknowledge.send(()).unwrap();
        // A meter within its interval waits; nothing else is pending.
        publish_audio_meters(std::slice::from_mut(&mut peer), meters);
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        assert_eq!(handed(&outbound), Vec::<WireMessage>::new());
        thread::sleep(crate::web_feed::WEB_METER_INTERVAL);
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        assert!(matches!(
            &handed(&outbound)[..],
            [WireMessage::AudioMeters(_)]
        ));
        acknowledge.send(()).unwrap();
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        assert!(peer.outbound.is_empty());

        // Shutdown drops the feed: nothing follows the goodbye.
        publish_stream_status(std::slice::from_mut(&mut peer), status_message());
        discard_unstarted_lossy_records(&mut peer);
        thread::sleep(crate::web_feed::WEB_STATUS_INTERVAL);
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        assert_eq!(handed(&outbound), Vec::<WireMessage>::new());
    }

    /// AU-004 (Protocol 2.101): a Web page's feed hands over the live
    /// source drift telemetry; it never uses the raw lossy slot.
    #[test]
    fn web_feeds_take_source_sync_status() {
        let (mut peer, outbound, _acknowledge, _inbound) = web_peer();
        peer.web_feed = Some(WebFeed::default());
        let status = SourceSyncStatusMessage {
            server: identity(),
            sequence: 4,
            sources: vec![fm_protocol::SourceSyncSample {
                input: fm_protocol::WireInputId::new(NonZeroU128::new(3).unwrap()),
                kind: fm_protocol::SourceSyncKind::Capture,
                state: fm_protocol::SourceClockState::Running,
                input_rate_hz: 48_000,
                output_rate_hz: 48_000,
                drift_mppm: 12_300,
                correction_mppm: 12_000,
                buffer_us: 40_000,
                target_us: 40_000,
                resyncs: 0,
                underruns: 1,
                overruns: 0,
                picture_drops: 2,
                picture_repeats: 0,
                restarts: 1,
            }],
        };
        publish_source_sync_status(std::slice::from_mut(&mut peer), &status);
        assert!(peer.lossy.is_empty());
        assert!(matches!(write_peer(&mut peer), WriteOutcome::Pending));
        let handed: Vec<WireMessage> = outbound
            .try_iter()
            .flat_map(|bytes| LineDecoder::new().push(&bytes).unwrap())
            .collect();
        assert_eq!(handed, [WireMessage::SourceSyncStatus(status)]);
    }

    /// Raw Studio peers keep the recorder status in its own lossy slot.
    #[test]
    fn raw_peers_take_recorder_status_in_its_own_slot() {
        let (mut peer, mut client) = active_eligible_peer();
        publish_recorder_status(std::slice::from_mut(&mut peer), recorder_status_message(1));
        assert!(peer.lossy.slot(LossySlot::RecorderStatus).is_empty());
        peer.recorder_status = true;
        publish_recorder_status(std::slice::from_mut(&mut peer), recorder_status_message(2));
        assert!(!peer.lossy.slot(LossySlot::RecorderStatus).is_empty());
        let expected = encode_line(&WireMessage::RecorderStatus(recorder_status_message(2)))
            .unwrap()
            .into_bytes();
        while !peer.lossy.is_empty() {
            assert!(!matches!(write_peer(&mut peer), WriteOutcome::Failed));
        }
        assert_eq!(read_exact(&mut client, expected.len()), expected);
    }

    /// Raw Studio peers keep the highlight reel status in its own lossy
    /// slot (OR-013).
    #[test]
    fn raw_peers_take_reel_status_in_its_own_slot() {
        let (mut peer, mut client) = active_eligible_peer();
        publish_reel_status(std::slice::from_mut(&mut peer), reel_status_message(1));
        assert!(peer.lossy.slot(LossySlot::ReelStatus).is_empty());
        peer.recorder_status = true;
        publish_reel_status(std::slice::from_mut(&mut peer), reel_status_message(2));
        assert!(!peer.lossy.slot(LossySlot::ReelStatus).is_empty());
        let expected = encode_line(&WireMessage::ReelStatus(Box::new(reel_status_message(2))))
            .unwrap()
            .into_bytes();
        while !peer.lossy.is_empty() {
            assert!(!matches!(write_peer(&mut peer), WriteOutcome::Failed));
        }
        assert_eq!(read_exact(&mut client, expected.len()), expected);
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

    /// A native pass's group takes the commands queued at the front of
    /// every active peer, one per peer per round from the turn, each peer's
    /// in the order it sent them, stops a peer at its first message that
    /// cannot join (leaving it and all behind it queued), and is bounded.
    /// A turn whose own command cannot join, or a group of one, takes
    /// nothing.
    #[test]
    fn a_native_group_takes_queued_commands_round_robin_in_each_peers_order() {
        let command = |id: &str, payload: CommandPayload| {
            WireMessage::Command(fm_protocol::CommandMessage {
                protocol: fm_protocol::CURRENT_PROTOCOL_VERSION,
                id: id.into(),
                idempotency_key: id.into(),
                expected_revision: None,
                deadline_ms: None,
                payload,
            })
        };
        let cut = |id: &str| command(id, CommandPayload::Cut);
        let record = |id: &str| {
            command(
                id,
                CommandPayload::RecordStart {
                    recorder: fm_protocol::WireRecorderSlot::new(1).unwrap(),
                },
            )
        };
        let fresh = || -> Vec<Peer> {
            (0..4)
                .map(|index| {
                    let (mut peer, _client) = raw_peer();
                    peer.id = index;
                    peer.phase = Phase::Active;
                    peer
                })
                .collect()
        };
        let ids = |group: &[(usize, fm_protocol::CommandMessage)]| -> Vec<(usize, String)> {
            group
                .iter()
                .map(|(index, command)| (*index, command.id.clone()))
                .collect()
        };
        let close = vec![false; 4];

        let mut peers = fresh();
        peers[0].inbound.extend([cut("a1"), filler(0), cut("a2")]);
        peers[1]
            .inbound
            .extend([cut("b1"), cut("b2"), record("b3"), cut("b4")]);
        peers[2].inbound.extend([cut("c1"), cut("c2"), cut("c3")]);
        peers[3].inbound.extend([cut("d1")]);
        peers[3].held_result = Some(HeldResult {
            message: filler(1),
            slot: None,
        });
        let group = native_group(&mut peers, &close, 1);
        assert_eq!(
            ids(&group),
            [
                (1, "b1".into()),
                (2, "c1".into()),
                (0, "a1".into()),
                (1, "b2".into()),
                (2, "c2".into()),
                (2, "c3".into()),
            ]
        );
        assert_eq!(peers[0].inbound, [filler(0), cut("a2")]);
        assert_eq!(peers[1].inbound, [record("b3"), cut("b4")]);
        assert!(peers[2].inbound.is_empty());
        assert_eq!(peers[3].inbound, [cut("d1")]);

        // The turn's own command cannot join, or it would be alone.
        let mut peers = fresh();
        peers[0].inbound.extend([record("a1"), cut("a2")]);
        peers[1].inbound.extend([cut("b1")]);
        assert_eq!(ids(&native_group(&mut peers, &close, 0)), Vec::new());
        assert_eq!(ids(&native_group(&mut peers, &close, 1)), Vec::new());
        assert_eq!(peers[0].inbound.len(), 2);
        assert_eq!(peers[1].inbound.len(), 1);

        // Bounded: the rest waits, in order, for a later pass.
        let mut peers = fresh();
        for (index, peer) in peers.iter_mut().enumerate() {
            for sent in 0..NATIVE_GROUP_COMMANDS {
                peer.inbound.push_back(cut(&format!("{index}-{sent}")));
            }
        }
        let group = native_group(&mut peers, &close, 0);
        assert_eq!(group.len(), NATIVE_GROUP_COMMANDS);
        let per_peer = NATIVE_GROUP_COMMANDS / 4;
        for (index, peer) in peers.iter().enumerate() {
            assert_eq!(
                peer.inbound.front(),
                Some(&cut(&format!("{index}-{per_peer}")))
            );
        }
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
