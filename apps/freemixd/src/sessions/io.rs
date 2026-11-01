//! The session scheduler's per-pass I/O: live records, outbound writes,
//! status publications, and shutdown draining.
//!
//! The scheduler in `sessions.rs` owns the pass; these helpers write each
//! peer's queue, publish the live feeds, and drain the peers on shutdown.

use super::{
    Accounting, AudioMetersMessage, CLIENT_READ_POLL_INTERVAL, CLIENT_WRITE_TIMEOUT, Counted,
    DaemonShutdownReason, ErrorMessage, Instant, LiveEvent, MonitorTap, OUTBOUND_CAPACITY,
    Outbound, OutboundWrite, Peer, Phase, REPLY_RESERVE, RecorderStatusMessage, ServerIdentity,
    SessionError, SharedControl, SourceSyncStatusMessage, StreamStatusMessage, StructuredError,
    Transport, TryRecvError, TrySendError, WEB_FEED_MAX_QUEUED, WRITE_BUDGET_PER_PASS, dropping,
    encode_line, now_millis, release_held_results, shutdown_message, thread, web_meters,
};
use fm_protocol::WireMessage;

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
pub(super) fn drain_live(
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
                let Some(subscription) = peer.subscription.as_ref() else {
                    break;
                };
                match subscription.try_recv() {
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

pub(super) enum WriteOutcome {
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
pub(super) fn write_web_peer(peer: &mut Peer) -> WriteOutcome {
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
                let Some(record) = outbound.pop_front() else {
                    return WriteOutcome::Failed;
                };
                if record.accounted && matches!(record.accounting, Accounting::Session) {
                    let Some(session) = session.as_mut() else {
                        return WriteOutcome::Failed;
                    };
                    if session.outbound_delivered().is_err() {
                        return WriteOutcome::Failed;
                    }
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
            let Some(session) = session.as_mut() else {
                return WriteOutcome::Failed;
            };
            match session.queue_outbound(bytes.len(), now) {
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

pub(super) fn write_peer(peer: &mut Peer) -> WriteOutcome {
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
pub(super) fn write_peer_pass(peer: &mut Peer) -> WriteOutcome {
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

pub(super) fn finish_outbound(peer: &mut Peer) -> WriteOutcome {
    let Some(record) = peer.outbound.pop_front() else {
        return WriteOutcome::Failed;
    };
    if record.accounted && matches!(record.accounting, Accounting::Session) {
        let Some(session) = peer.session.as_mut() else {
            return WriteOutcome::Failed;
        };
        if session.outbound_delivered().is_err() {
            return WriteOutcome::Failed;
        }
    }
    if record.handshake_response {
        peer.handshake_written = true;
        WriteOutcome::HandshakeResponseWritten
    } else {
        WriteOutcome::Pending
    }
}

pub(super) fn write_lossy(peer: &mut Peer) -> WriteOutcome {
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

pub(super) fn publish_audio_meters(peers: &mut [Peer], meters: AudioMetersMessage) {
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
pub(super) fn publish_title_phases(peers: &mut [Peer], phases: fm_protocol::TitlePhasesMessage) {
    let Ok(bytes) = encode_line(&WireMessage::TitlePhases(phases)).map(String::into_bytes) else {
        return;
    };
    for peer in peers {
        peer.replace_title_phases(&bytes);
    }
}

/// The replay status (OR-012) goes to raw Studio peers in its own
/// latest-wins slot.
pub(super) fn publish_replay_status(peers: &mut [Peer], status: fm_protocol::ReplayStatusMessage) {
    let Ok(bytes) =
        encode_line(&WireMessage::ReplayStatus(Box::new(status))).map(String::into_bytes)
    else {
        return;
    };
    for peer in peers {
        peer.replace_replay_status(bytes.clone());
    }
}

/// The answer to a `reel_status_request`: the status, or an `error`
/// (`status_too_large`) carrying the request id when the status is too long
/// for one line, which would otherwise close the session.
pub(super) fn reel_status_reply(
    status: fm_protocol::ReelStatusMessage,
    request_id: String,
    current_revision: u64,
) -> WireMessage {
    let message = WireMessage::ReelStatus(Box::new(status));
    if encode_line(&message).is_ok() {
        return message;
    }
    WireMessage::Error(ErrorMessage {
        request_id: Some(request_id),
        current_revision: Some(current_revision),
        error: StructuredError {
            code: "status_too_large".into(),
            message: "the reel status is too long for one line; shorten reel names or music \
                      paths, or remove reels"
                .into(),
            fields: Vec::new(),
            retryable: false,
        },
    })
}

/// The highlight reel status (OR-013) goes to raw Studio peers in its own
/// latest-wins slot.
pub(super) fn publish_reel_status(peers: &mut [Peer], status: fm_protocol::ReelStatusMessage) {
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
    pub(super) fn poll(
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
    pub(super) fn next_sequence(&mut self) -> u64 {
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
    pub(super) fn poll(
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
    pub(super) fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }
}

pub(super) fn publish_recorder_status(peers: &mut [Peer], status: RecorderStatusMessage) {
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
pub(super) fn publish_source_sync_status(peers: &mut [Peer], status: &SourceSyncStatusMessage) {
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
/// latest-wins slot, once a statistics sample, and (Protocol 2.110) to Web
/// pages' feeds.
pub(super) fn publish_health_status(peers: &mut [Peer], status: &fm_protocol::HealthStatusMessage) {
    let Ok(bytes) = encode_line(&WireMessage::HealthStatus(status.clone())).map(String::into_bytes)
    else {
        return;
    };
    for peer in peers {
        peer.replace_health_status(bytes.clone());
    }
}

pub(super) fn publish_stream_status(peers: &mut [Peer], status: StreamStatusMessage) {
    let Ok(bytes) = encode_line(&WireMessage::StreamStatus(status)).map(String::into_bytes) else {
        return;
    };
    for peer in peers {
        peer.replace_stream_status(bytes.clone());
    }
}

/// Publishes one tap's atomic encoded frame group to every eligible peer.
pub(super) fn publish_monitor_frame(peers: &mut [Peer], tap: MonitorTap, bytes: &[u8]) {
    for peer in peers {
        peer.replace_monitor_frame(tap, bytes);
    }
}

/// Drops lossy records that never reached the wire so shutdown cannot flush
/// stale telemetry or monitor frames after its goodbye; in-flight records keep
/// draining.
pub(super) fn discard_unstarted_lossy_records(peer: &mut Peer) {
    peer.lossy.discard_unstarted();
    // A Web peer's feed records not handed over yet are dropped, and no
    // further one follows the goodbye.
    peer.web_feed = None;
}

pub(super) enum ShutdownQueue {
    Replace,
    PreserveCommandResult,
}

pub(super) fn shutdown_peers(
    peers: &mut Vec<Peer>,
    control: &SharedControl,
    queue: &ShutdownQueue,
) {
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

pub(super) fn close_all(peers: &mut Vec<Peer>, control: &SharedControl) {
    while let Some(peer) = peers.pop() {
        close_peer(peer, control);
    }
}

pub(super) fn shutdown_for_reason(
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

pub(super) fn close_peer(mut peer: Peer, control: &SharedControl) {
    if let Some(subscription) = peer.subscription.take() {
        control.borrow_mut().unsubscribe(subscription.id());
    }
}
