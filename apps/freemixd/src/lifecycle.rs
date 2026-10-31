//! The daemon's lifecycle: the process shutdown flag and its signals, the
//! shutdown reason, and the operator-state status text.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(unused_imports, clippy::wildcard_imports)]
// the binary items, some of them native only
use super::*;

#[derive(Clone)]
pub(crate) struct ProcessShutdown {
    pub(crate) requested: Arc<AtomicBool>,
    pub(crate) diagnostic_deadline: Option<Instant>,
    /// Snapshot published to the status listener; never read by the daemon.
    pub(crate) status: Arc<DaemonStatus>,
    /// The authoritative `fm-server` status the control loop republishes on
    /// every pass. The session loop only ever holds `&Server`, so the server
    /// cannot change its own status while the loop runs; this is the value the
    /// daemon knows, recorded by [`Self::observe_service`] at every point that
    /// can move it.
    pub(crate) service: ServiceStatus,
    /// Control handle for the bounded operator-state render, attached once the
    /// control service exists and a status listener is configured. The render
    /// never re-enters a borrow a caller already holds: `try_borrow` skips
    /// that pass instead of panicking.
    pub(crate) control: Option<SharedControl>,
    /// When the operator state was last published; gates rendering to one
    /// [`String`] per [`OPERATOR_STATE_PUBLISH_INTERVAL`].
    pub(crate) operator_published_at: Cell<Option<Instant>>,
}

impl ProcessShutdown {
    /// Records a new authoritative service status and publishes it at once.
    pub(crate) fn observe_service(&mut self, service: ServiceStatus) {
        self.service = service;
        self.status.publish(service);
    }

    /// Attaches the control handle the operator-state render reads.
    pub(crate) fn attach_control(&mut self, control: &SharedControl) {
        self.control = Some(Rc::clone(control));
    }

    /// Publishes one bounded, sanitized operator-state snapshot.
    ///
    /// Called from the control loop on every pass and self-gated to one render
    /// per [`OPERATOR_STATE_PUBLISH_INTERVAL`], so a pass that already holds a
    /// mutable control borrow is simply skipped: the status listener is a
    /// diagnostic surface and must never change how the control plane runs.
    /// The render itself reads the engine's protocol snapshot and the native
    /// daemon's realized state, then copies one bounded string into the
    /// listener cell.
    pub(crate) fn publish_operator_state(&self, native: Option<&NativeDaemon>) {
        let Some(control) = self.control.as_ref() else {
            return;
        };
        let now = Instant::now();
        if self.operator_published_at.get().is_some_and(|last| {
            now.saturating_duration_since(last) < OPERATOR_STATE_PUBLISH_INTERVAL
        }) {
            return;
        }
        let Ok(control) = control.try_borrow() else {
            return;
        };
        self.operator_published_at.set(Some(now));
        let text = operator_state_text(&control, native);
        self.status.publish_operator_state(&text);
    }

    /// Publishes the daemon's statistics text (RC-015) for the support
    /// bundle; rendered only while a status listener consumes it.
    pub(crate) fn publish_statistics(&self, render: impl FnOnce() -> String) {
        if self.control.is_some() {
            self.status.publish_statistics(&render());
        }
    }

    pub(crate) fn requested(&self) -> bool {
        self.requested.load(AtomicOrdering::Acquire)
            || self
                .diagnostic_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
    }

    pub(crate) fn set_diagnostic_deadline(&mut self, duration: Duration) -> AppResult<()> {
        self.diagnostic_deadline = Some(
            Instant::now()
                .checked_add(duration)
                .ok_or_else(|| AppFailure("diagnostic shutdown deadline overflow".into()))?,
        );
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn register_process_shutdown() -> AppResult<ProcessShutdown> {
    use signal_hook::consts::signal::{SIGINT, SIGTERM};

    let requested = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGINT, Arc::clone(&requested))?;
    signal_hook::flag::register(SIGTERM, Arc::clone(&requested))?;
    Ok(ProcessShutdown {
        requested,
        diagnostic_deadline: None,
        status: DaemonStatus::new(),
        service: ServiceStatus::new(),
        control: None,
        operator_published_at: Cell::new(None),
    })
}

#[cfg(windows)]
pub(crate) fn register_process_shutdown() -> AppResult<ProcessShutdown> {
    let requested = Arc::new(AtomicBool::new(false));
    let handler_flag = Arc::clone(&requested);
    ctrlc::set_handler(move || handler_flag.store(true, AtomicOrdering::Release))?;
    Ok(ProcessShutdown {
        requested,
        diagnostic_deadline: None,
        status: DaemonStatus::new(),
        service: ServiceStatus::new(),
        control: None,
        operator_published_at: Cell::new(None),
    })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn register_process_shutdown() -> AppResult<ProcessShutdown> {
    Ok(ProcessShutdown {
        requested: Arc::new(AtomicBool::new(false)),
        diagnostic_deadline: None,
        status: DaemonStatus::new(),
        service: ServiceStatus::new(),
        control: None,
        operator_published_at: Cell::new(None),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DaemonShutdownReason {
    Once,
    ProcessSignal,
    ProgramSurface,
}

#[cfg(test)]
#[derive(Eq, PartialEq)]
pub(crate) enum OnceClientOutcome {
    Unserved,
    HandshakeResponseWritten,
}

/// Polled by every daemon loop, which makes it the control plane's liveness
/// beat as well as its cooperative stop check.
pub(crate) fn requested_daemon_shutdown(
    native: Option<&NativeDaemon>,
    process: Option<&ProcessShutdown>,
) -> Option<DaemonShutdownReason> {
    if let Some(process) = process {
        // Two atomic stores next to the beat. Republishing keeps the health
        // surface live: a degraded daemon shows up on `/healthz` within one
        // poll interval instead of staying frozen at whatever startup wrote.
        process.status.beat();
        process.status.publish(process.service);
        // One bounded render per second keeps the support bundle current
        // without making a poll pass cost a formatting pass every 5 ms.
        process.publish_operator_state(native);
    }
    let reason = if process.is_some_and(ProcessShutdown::requested) {
        Some(DaemonShutdownReason::ProcessSignal)
    } else if native.is_some_and(NativeDaemon::shutdown_requested) {
        Some(DaemonShutdownReason::ProgramSurface)
    } else {
        None
    };
    if reason.is_some()
        && let Some(process) = process
    {
        process.status.begin_draining();
    }
    reason
}

/// Renders the bounded operator-state section published to the status listener.
///
/// The section is line-oriented and fixed-order: the configured stream roster
/// with desired state, both recorder slots' desired flags, and — when a native
/// daemon runs — realized stream state, recorder configuration and activity,
/// and the disk alert. Only target ids, operator-authored names, booleans,
/// counts, and bounded labels are ever read, so URLs, stream keys, and
/// filesystem paths cannot leak. An oversized render is truncated at a UTF-8
/// boundary with an explicit marker.
pub(crate) fn operator_state_text(
    control: &ControlService<Policy>,
    native: Option<&NativeDaemon>,
) -> String {
    let snapshot = &control.snapshot().snapshot;
    render_operator_state(&snapshot.streams, &snapshot.recordings, native)
}

/// Renders the operator-state lines from the engine's protocol snapshot and
/// the native daemon's realized state, then applies the byte cap.
pub(crate) fn render_operator_state(
    streams: &[StreamStatus],
    recordings: &[RecordingStatus],
    native: Option<&NativeDaemon>,
) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "streams={}", streams.len());
    for stream in streams {
        let _ = write!(
            output,
            "stream target={} name={} desired={}",
            stream.target,
            operator_state_name(&stream.name),
            stream.desired_running
        );
        if let Some(realized) = native_stream_realized(native, stream.target) {
            let _ = write!(output, " realized={realized}");
        }
        output.push('\n');
    }
    let desired = |slot: usize| {
        recordings
            .get(slot)
            .is_some_and(|status| status.desired_active)
    };
    let _ = write!(output, "recorders=desired={},{}", desired(0), desired(1));
    #[cfg(feature = "native-media")]
    if let Some(native) = native {
        let _ = write!(
            output,
            " configured={},{} running={},{}",
            native.record_program.is_some(),
            native.record_program_2.is_some(),
            native.recorders[0].is_running(),
            native.recorders[1].is_running(),
        );
    }
    #[cfg(not(feature = "native-media"))]
    let _ = native;
    output.push('\n');
    #[cfg(feature = "native-media")]
    if let Some(native) = native {
        let _ = writeln!(
            output,
            "disk_alert={} free_bytes={}",
            disk_alert_label(native.disk_free_bytes.map(|_| native.disk_alert.state())),
            native.disk_free_bytes.unwrap_or(0)
        );
    }
    cap_operator_state(output)
}

/// Replaces control characters in an operator-authored stream name so one name
/// can never forge an extra line in the support bundle. The name itself is
/// safe by construction: the engine inventory carries only the authored label,
/// never a destination URL or key.
pub(crate) fn operator_state_name(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_control() {
            sanitized.push(' ');
        } else {
            sanitized.push(character);
        }
    }
    sanitized
}

/// The realized state label of one stream target as the native daemon sees it,
/// or `None` when no native daemon runs or the target has no sink.
#[cfg(feature = "native-media")]
pub(crate) fn native_stream_realized(
    native: Option<&NativeDaemon>,
    target: WireStreamTargetId,
) -> Option<&'static str> {
    let native = native?;
    let target = SwitcherStreamTargetId::from_non_zero(target.get());
    let stream = native
        .streams
        .iter()
        .find(|stream| stream.target == target)?;
    if stream.first_failure.is_some() {
        return Some("failed");
    }
    Some(match native.output_set.state(stream.destination_id) {
        None | Some(DestinationState::Stopped) => "stopped",
        Some(DestinationState::Connecting | DestinationState::AwaitingRandomAccess) => "starting",
        Some(DestinationState::Live) => "live",
        Some(DestinationState::WaitingToReconnect { .. }) => "waiting_to_reconnect",
        Some(DestinationState::Congested) => "congested",
        Some(DestinationState::Failed) => "failed",
    })
}

#[cfg(not(feature = "native-media"))]
pub(crate) const fn native_stream_realized(
    _native: Option<&NativeDaemon>,
    _target: WireStreamTargetId,
) -> Option<&'static str> {
    None
}

/// Applies the operator-state byte cap, marking an overlong render instead of
/// letting the status listener drop it.
pub(crate) fn cap_operator_state(text: String) -> String {
    if text.len() <= OPERATOR_STATE_MAX_BYTES {
        return text;
    }
    let mut capped = String::with_capacity(OPERATOR_STATE_MAX_BYTES);
    let prefix_bytes =
        OPERATOR_STATE_MAX_BYTES.saturating_sub(OPERATOR_STATE_TRUNCATION_MARKER.len());
    capped.push_str(utf8_prefix(&text, prefix_bytes));
    capped.push_str(OPERATOR_STATE_TRUNCATION_MARKER);
    capped
}

/// The longest UTF-8 prefix of `value` no longer than `maximum_bytes`.
pub(crate) fn utf8_prefix(value: &str, maximum_bytes: usize) -> &str {
    let mut end = maximum_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
