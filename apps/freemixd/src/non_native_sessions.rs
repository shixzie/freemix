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
use fm_control::{LiveEvent, Subscription};
use fm_persistence::StoredProject;
use fm_protocol::{
    AudioMetersMessage, ClientType, CommandPayload, CommandResult, ErrorMessage,
    HandshakeOutcome as ProtocolHandshakeOutcome, HandshakeResponse,
    HeartbeatAcknowledgementMessage, LineDecoder, MonitorTap, ServerIdentity, StreamStatusMessage,
    StructuredError, WireMessage, encode_line,
};
use fm_server::{Server, Session, SyncPayload};

use super::{
    AppResult, CLIENT_READ_POLL_INTERVAL, CLIENT_WRITE_TIMEOUT, CommandDelivery, ControlHandle,
    DaemonShutdownReason, NativeDaemon, PendingWrite, ProcessShutdown, SharedControl,
    current_handshake, diagnostics_response, error_message, execute_session_command,
    handshake_code, handshake_response, is_client_session_termination, now_millis,
    reconciled_handshake_outcome, record_heartbeat, rejected_handshake_response,
    requested_daemon_shutdown, server_identity, shutdown_message, structured_session_error,
};
use crate::journal::DurableStore;
use crate::latest_record::LatestRecord;
use crate::monitor_tap::SimulatedMonitorTap;
use crate::web::{WebEvent, WebGateway};

const MAX_PEERS: usize = 2;
const INBOUND_CAPACITY: usize = 8;
const OUTBOUND_CAPACITY: usize = 32;
const LIVE_EVENTS_PER_PASS: usize = 8;
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
    lossy: LossyRecords,
    /// An accepted `RecordStart` acknowledgement held until the native
    /// runtime has realized its segment. While it is held the peer's later
    /// messages and live events wait behind it, so the peer observes exactly
    /// the order it did when the start blocked the daemon; every other peer,
    /// and this peer's lossy side channels, keep being served.
    held_result: Option<HeldResult>,
}

/// A command result waiting for a recorder slot to finish realizing.
struct HeldResult {
    result: CommandResult,
    slot: usize,
}

/// One independent latest-wins side-channel slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LossySlot {
    Meter,
    StreamStatus,
    PreviewFrame,
    ProgramFrame,
}

impl LossySlot {
    const COUNT: usize = 4;

    const fn index(self) -> usize {
        match self {
            Self::Meter => 0,
            Self::StreamStatus => 1,
            Self::PreviewFrame => 2,
            Self::ProgramFrame => 3,
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

    fn discard_unstarted(&mut self) {
        for slot in &mut self.slots {
            slot.discard_unstarted();
        }
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
            lossy: LossyRecords::default(),
            held_result: None,
        })
    }

    fn queue(&mut self, message: &WireMessage, accounting: Accounting) -> Result<(), ()> {
        if self.outbound.len() >= OUTBOUND_CAPACITY {
            return Err(());
        }
        let bytes = encode_line(message).map_err(|_| ())?.into_bytes();
        let byte_len = bytes.len();
        let (write, accounted) = match &self.transport {
            Transport::Raw(_) => (OutboundWrite::Raw(PendingWrite::new(bytes)), true),
            Transport::Web(_) => (OutboundWrite::Web(bytes), false),
        };
        if matches!(accounting, Accounting::Session) && accounted {
            self.session
                .as_mut()
                .ok_or(())?
                .queue_outbound(byte_len, now_millis().map_err(|_| ())?)
                .map_err(|_| ())?;
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
            if self.queue(&message, Accounting::Session).is_err() {
                return Err(());
            }
        }
        Ok(())
    }

    fn close_after(&mut self, message: &WireMessage) -> Result<(), ()> {
        self.queue(message, Accounting::Raw)?;
        self.phase = Phase::Closing;
        Ok(())
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

    fn wants_monitor_frames(&self) -> bool {
        self.monitor_frames && self.phase == Phase::Active
    }

    fn replace_outbound_for_shutdown(&mut self) {
        let retained = usize::from(self.outbound.front().is_some_and(|record| {
            record.channel_sent
                || matches!(&record.write, OutboundWrite::Raw(write) if write.started())
        }));
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
    process_shutdown: &ProcessShutdown,
    once: bool,
    web: Option<&WebGateway>,
) -> AppResult<DaemonShutdownReason> {
    let handshake_timeout =
        Duration::from_millis(server.config().session_limits.heartbeat_timeout_ms);
    let peer_limit = if native.is_some() { 1 } else { MAX_PEERS };
    let mut peers = Vec::with_capacity(peer_limit);
    let mut next_peer_id = 0_u64;
    let mut once_peer = None;
    let runtime = Runtime {
        server,
        control,
        store,
        process_shutdown,
    };
    let mut native = native;
    let mut simulated_monitor = SimulatedMonitorTap::default();

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
                    WebEvent::Connected(connection)
                        if !peers.iter().any(|peer| peer.transport.is_web()) =>
                    {
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

        if once_peer.is_none() && (!once || peers.is_empty()) && raw_peer_count(&peers) < peer_limit
        {
            match listener.accept() {
                Ok((stream, _)) => {
                    if stream.set_nonblocking(true).is_ok() && stream.set_nodelay(true).is_ok() {
                        let Some(peer) = Peer::new(
                            next_peer_id,
                            Transport::Raw(stream),
                            principal.clone(),
                            handshake_timeout,
                        ) else {
                            close_all(&mut peers, control);
                            return Err("handshake deadline exceeds Instant range".into());
                        };
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
            if !close[index] && peer.phase == Phase::AwaitHandshake {
                close[index] = Instant::now() >= peer.handshake_deadline;
            }
            if !close[index]
                && let Some(session) = peer.session.as_mut()
            {
                close[index] = !super::session_heartbeat_active(session).unwrap_or(false);
            }
            if !close[index]
                && peer
                    .subscription
                    .as_ref()
                    .and_then(Subscription::failure)
                    .is_some()
            {
                close[index] = true;
            }
        }

        let mut live_budget = vec![LIVE_EVENTS_PER_PASS; peers.len()];
        for index in 0..peers.len() {
            if close[index] || peers[index].held_result.is_some() {
                continue;
            }
            let message = match peers[index].phase {
                Phase::AwaitHandshake | Phase::Active => peers[index].inbound.pop_front(),
                Phase::Syncing | Phase::Closing => None,
            };
            let Some(message) = message else {
                continue;
            };
            match runtime.dispatch(&mut peers[index], message, durable, native.as_deref_mut()) {
                Ok(()) => {}
                Err(DispatchError::Peer) => close[index] = true,
                Err(DispatchError::Daemon(error)) => {
                    shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
                    return Err(error);
                }
            }
        }
        if let Some(reason) = requested_daemon_shutdown(native.as_deref(), Some(process_shutdown)) {
            return Ok(shutdown_for_reason(reason, &mut peers, control));
        }
        if let Some(native) = native.as_deref_mut() {
            if let Err(error) = native.tick_if_due(&mut control.borrow_mut(), authority) {
                shutdown_peers(&mut peers, control, &ShutdownQueue::Replace);
                return Err(error);
            }
            native.advance_recordings();
            release_held_results(&mut peers, &mut close, |slot| {
                native.recording_settling(slot)
            });
            if let Some(meters) = native.take_audio_meters() {
                publish_audio_meters(&mut peers, meters);
            }
            if let Some(status) = native.take_stream_status() {
                publish_stream_status(&mut peers, status);
            }
        }
        let monitor_demand = peers.iter().any(Peer::wants_monitor_frames);
        if let Some(native) = native.as_deref_mut() {
            native.set_monitor_demand(monitor_demand);
            if let Some((tap, bytes)) = native.take_monitor_frame() {
                publish_monitor_frame(&mut peers, tap, &bytes);
            }
        } else {
            let frames = simulated_monitor.poll(
                &control.borrow(),
                durable.project(),
                authority,
                Instant::now(),
                monitor_demand,
            );
            for (tap, bytes) in frames {
                publish_monitor_frame(&mut peers, tap, &bytes);
            }
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
                close[index] = true;
                continue;
            }
            match write_peer(peer) {
                WriteOutcome::Pending => {}
                WriteOutcome::HandshakeResponseWritten => {
                    if once && once_peer.is_none() {
                        once_peer = Some(peer.id);
                    }
                }
                WriteOutcome::Failed => close[index] = true,
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

/// Queues every held acknowledgement whose recorder slot is no longer
/// settling. A peer that cannot take its result is closed, exactly as if the
/// result had been queued when the command ran.
fn release_held_results(
    peers: &mut [Peer],
    close: &mut [bool],
    mut settling: impl FnMut(usize) -> bool,
) {
    for (index, peer) in peers.iter_mut().enumerate() {
        let Some(held) = peer.held_result.as_ref() else {
            continue;
        };
        if settling(held.slot) {
            continue;
        }
        let held = peer.held_result.take().expect("checked above");
        if peer
            .queue(
                &WireMessage::CommandResult(held.result),
                Accounting::Session,
            )
            .is_err()
        {
            close[index] = true;
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
            Ok(messages)
                if messages.len() <= INBOUND_CAPACITY.saturating_sub(peer.inbound.len()) =>
            {
                peer.inbound.extend(messages);
                false
            }
            Ok(_) | Err(_) => true,
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

fn raw_peer_count(peers: &[Peer]) -> usize {
    peers
        .iter()
        .filter(|peer| matches!(peer.transport, Transport::Raw(_)))
        .count()
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

struct Runtime<'a> {
    server: &'a Server<ControlHandle>,
    control: &'a SharedControl,
    store: &'a dyn DurableStore,
    process_shutdown: &'a ProcessShutdown,
}

impl Runtime<'_> {
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
                let CommandDelivery { result, .. } = delivery;
                if let Some(slot) = recording_start_slot(&command.payload, &result)
                    && native
                        .as_deref()
                        .is_some_and(|native| native.recording_settling(slot))
                {
                    peer.held_result = Some(HeldResult { result, slot });
                    return Ok(());
                }
                peer.queue(&WireMessage::CommandResult(result), Accounting::Session)?;
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
                peer.queue(&message, Accounting::Session)?;
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
                peer.queue(&response, Accounting::Session)?;
                Ok(())
            }
            _ => {
                peer.queue(
                    &error_message(
                        "unexpected_message",
                        "only command and heartbeat messages are accepted after the handshake",
                    ),
                    Accounting::Session,
                )?;
                Ok(())
            }
        }
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
        while budgets[index] > 0 && peer.outbound.len() < OUTBOUND_CAPACITY {
            let event = match peer
                .subscription
                .as_ref()
                .expect("active peers have subscriptions")
                .try_recv()
            {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    close[index] = true;
                    break;
                }
            };
            let message = match event {
                LiveEvent::Durable(event) => WireMessage::Event(event),
                LiveEvent::Runtime(event) => WireMessage::RuntimeEvent(event),
            };
            if peer.queue(&message, Accounting::Session).is_err() {
                close[index] = true;
                break;
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

fn write_peer(peer: &mut Peer) -> WriteOutcome {
    if peer.transport.is_web() {
        let Some(record) = peer.outbound.front_mut() else {
            return WriteOutcome::Pending;
        };
        let Transport::Web(web) = &mut peer.transport else {
            unreachable!("web transport checked above")
        };
        if record.channel_sent {
            match web.acknowledgements.try_recv() {
                Ok(()) => return finish_outbound(peer),
                Err(TryRecvError::Empty) => return WriteOutcome::Pending,
                Err(TryRecvError::Disconnected) => return WriteOutcome::Failed,
            }
        }
        let OutboundWrite::Web(bytes) = &record.write else {
            unreachable!("web peers only queue web writes")
        };
        if matches!(record.accounting, Accounting::Session) && !record.accounted {
            let Ok(now) = now_millis() else {
                return WriteOutcome::Failed;
            };
            if peer
                .session
                .as_mut()
                .expect("session accounting has a session")
                .queue_outbound(bytes.len(), now)
                .is_err()
            {
                return WriteOutcome::Failed;
            }
            record.accounted = true;
        }
        match web.outbound.try_send(bytes.clone()) {
            Ok(()) => {
                record.channel_sent = true;
                WriteOutcome::Pending
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => WriteOutcome::Failed,
        }
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
        let complete = match &mut record.write {
            OutboundWrite::Raw(write) => write.write_once(stream),
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
    match peer.lossy.write_once(stream) {
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
}
