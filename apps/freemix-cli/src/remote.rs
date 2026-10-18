use std::{
    error::Error,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fm_client::{Client, ClientConfig, ConnectionState, Intake, Outbound};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandPayload, CommandResult, DiagnosticsRequest,
    DiagnosticsResponse, EventPayload, FadeToBlackState, HandshakeOutcome, HandshakeRequest,
    MAX_LINE_BYTES, Role, RuntimeLifecycleEvent, ServerIdentity, StreamRealizedState,
    StreamStatusMessage, WireMessage, decode_line, encode_line,
};
use fm_types::ProjectId;
use fm_ui_model::{ManualTransitionStatus, ProjectState};

type RemoteResult<T> = Result<T, Box<dyn Error>>;

const REMOTE_IO_TIMEOUT: Duration = Duration::from_secs(2);

pub fn status(address: SocketAddr) -> RemoteResult<()> {
    let remote = Remote::connect(address, Role::Viewer)?;
    remote.print_status()
}

pub fn diagnostics(address: SocketAddr) -> RemoteResult<()> {
    let mut remote = Remote::connect(address, Role::Viewer)?;
    let deadline = Instant::now() + REMOTE_IO_TIMEOUT;
    let request_id = format!("cli-diagnostics-{}", unique_client_id()?);
    remote.write(&WireMessage::DiagnosticsRequest(DiagnosticsRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: request_id.clone(),
    }))?;
    let response = loop {
        match remote.read_until(deadline)? {
            WireMessage::DiagnosticsResponse(response) => break response,
            WireMessage::Event(event) => {
                remote.client.intake(WireMessage::Event(event))?;
            }
            WireMessage::RuntimeEvent(event) => {
                remote.client.intake(WireMessage::RuntimeEvent(event))?;
            }
            WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
            _ => {
                return Err(
                    RemoteFailure("expected diagnostics_response from daemon".into()).into(),
                );
            }
        }
    };
    if response.request_id != request_id
        || response.protocol != CURRENT_PROTOCOL_VERSION
        || response.engine.engine_id != remote.server.engine_id
        || response.engine.state_epoch != remote.server.state_epoch
        || response.engine.log_id != remote.server.log_id
    {
        return Err(RemoteFailure("diagnostics response does not match the request".into()).into());
    }
    print_diagnostics(&response);
    Ok(())
}

/// Lists the daemon's display-output slots with their presenter state
/// (OR-011).
pub fn display_outputs(address: SocketAddr) -> RemoteResult<()> {
    let remote = Remote::connect(address, Role::Viewer)?;
    let state = remote
        .client
        .model()
        .state()
        .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?;
    print_display_outputs(
        state
            .display_outputs()
            .iter()
            .map(|(slot, config)| (*slot, config, state.display_output_state(*slot))),
    );
    Ok(())
}

/// Prints the cue lists of the daemon at `address` with their run state
/// (RC-017).
pub fn cue_lists(address: SocketAddr) -> RemoteResult<()> {
    let remote = Remote::connect(address, Role::Viewer)?;
    let state = remote
        .client
        .model()
        .state()
        .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?;
    print_cue_lists(state.cue_lists().iter());
    Ok(())
}

/// Prints one `CueList(...)` line per cue list followed by one indented
/// `Cue(...)` line per cue, or `CueLists=none`.
pub(crate) fn print_cue_lists<'a>(lists: impl IntoIterator<Item = &'a fm_protocol::CueListStatus>) {
    let optional = |value: Option<u64>| value.map_or_else(|| "-".to_owned(), |id| id.to_string());
    let mut any = false;
    for list in lists {
        any = true;
        let state = &list.state;
        println!(
            "CueList(id={}, name={:?}, standby={}, live={}, pending={}, played={:?}, fault={})",
            list.id,
            list.spec.name,
            optional(state.standby),
            optional(state.live),
            state.pending.map_or_else(
                || "-".to_owned(),
                |pending| format!("{}.{}@{}", pending.cue, pending.action, pending.due_frame)
            ),
            state.played,
            state
                .fault
                .map_or_else(|| "-".to_owned(), |(cue, action)| format!("{cue}.{action}")),
        );
        for cue in &list.spec.cues {
            println!(
                "  Cue(id={}, name={:?}, follow={}, actions={})",
                cue.id,
                cue.name,
                cue.auto_follow,
                cue.actions
                    .iter()
                    .map(fm_protocol::cue_action_token)
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
    }
    if !any {
        println!("CueLists=none");
    }
}

/// Prints every network stream input of the daemon at `address` with its
/// redacted configuration and latest receive status (IN-015).
pub fn network_streams(address: SocketAddr) -> RemoteResult<()> {
    let remote = Remote::connect(address, Role::Viewer)?;
    let view = remote
        .client
        .model()
        .view()
        .ok_or_else(|| RemoteFailure("remote project view is unavailable".into()))?;
    print_network_streams(view.network_streams.iter().cloned());
    Ok(())
}

/// Edits network stream input `input` of the daemon at `address`: the
/// options given replace the input's current settings, and a URL given
/// replaces its URL (IN-015).
pub fn set_network_stream(
    address: SocketAddr,
    input: fm_types::InputId,
    options: &crate::args::NetworkStreamOptions,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> RemoteResult<()> {
    let mut remote = Remote::connect(address, Role::Graphics)?;
    let (settings, stream_id) = remote
        .client
        .model()
        .view()
        .and_then(|view| {
            view.network_stream(input)
                .map(|entry| (entry.config.settings, entry.config.stream_id.clone()))
        })
        .ok_or_else(|| RemoteFailure(format!("input {input} is not a network stream")))?;
    // The daemon never reveals the stored stream key or passphrase, so an
    // edit that names neither keeps them (IN-016, IN-017).
    let payload = CommandPayload::SetNetworkStream {
        input: fm_protocol::WireInputId::from_domain(input),
        url: options.url.clone(),
        settings: options.apply(settings),
        stream_id: options.stream_id(stream_id.as_deref()),
        stream_key: options.stream_key.clone(),
        passphrase: options.passphrase.clone(),
    };
    let key = match key {
        Some(key) => key,
        None => implicit_key(&payload)?,
    };
    remote.execute(payload, key, expected_revision, CommandCompletion::Project)
}

/// Adds or reconfigures stream destination `target` of the daemon at
/// `address` (OR-006). For an existing destination every option left out
/// keeps the daemon's current value; the stored stream key, which the daemon
/// never reveals, is kept unless `--stream-key` or `--clear-stream-key` is
/// given.
pub fn set_stream_target(
    address: SocketAddr,
    target: fm_protocol::WireStreamTargetId,
    options: &crate::args::RemoteStreamTargetOptions,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> RemoteResult<()> {
    let mut remote = Remote::connect(address, Role::Graphics)?;
    let current = remote
        .client
        .model()
        .view()
        .ok_or_else(|| RemoteFailure("remote project view is unavailable".into()))?
        .stream_target(target.get())
        .map(|entry| entry.config.clone());
    let config = stream_target_config(current.as_ref(), options)?;
    if let fm_protocol::SecretEdit::Set(secret) = &options.stream_key
        && !fm_protocol::stream_key_is_valid(secret)
    {
        return Err(RemoteFailure(format!(
            "invalid stream key: it must be 1..={} bytes of printable ASCII without spaces",
            fm_protocol::MAX_STREAM_TARGET_KEY_BYTES
        ))
        .into());
    }
    if let fm_protocol::SecretEdit::Set(secret) = &options.srt_passphrase {
        if config.protocol != fm_protocol::StreamTargetProtocol::Srt {
            return Err(
                RemoteFailure("--srt-passphrase requires an SRT destination".into()).into(),
            );
        }
        if !fm_protocol::srt_passphrase_is_valid(secret) {
            return Err(RemoteFailure(format!(
                "invalid SRT passphrase: it must be {}..={} bytes of printable ASCII without \
                 spaces",
                fm_protocol::MIN_STREAM_TARGET_PASSPHRASE_BYTES,
                fm_protocol::MAX_STREAM_TARGET_PASSPHRASE_BYTES
            ))
            .into());
        }
    }
    let payload = CommandPayload::SetStreamTarget {
        target,
        config: Box::new(config),
        stream_key: options.stream_key.clone(),
        srt_passphrase: options.srt_passphrase.clone(),
        position: None,
    };
    let key = match key {
        Some(key) => key,
        None => implicit_key(&payload)?,
    };
    remote.execute(payload, key, expected_revision, CommandCompletion::Project)?;
    print_stream_targets(remote.stream_target_rows()?);
    Ok(())
}

/// Splits an authored endpoint into its scheme's protocol, if it is a full
/// URL, and its scheme-relative part. No error repeats the endpoint text.
fn split_endpoint(
    text: &str,
    field: &str,
) -> RemoteResult<(Option<fm_protocol::StreamTargetProtocol>, String)> {
    match text.split_once("://") {
        Some((scheme, rest)) => {
            let protocol =
                fm_protocol::StreamTargetProtocol::from_token(scheme).ok_or_else(|| {
                    RemoteFailure(format!(
                        "{field} must use rtmp://, rtmps://, srt:// or hls://"
                    ))
                })?;
            Ok((Some(protocol), rest.to_owned()))
        }
        None => Ok((None, text.to_owned())),
    }
}

/// The configuration `remote-stream-set` sends: each given option, else the
/// daemon's current value, else (for a new destination) the default or a
/// refusal naming the missing option.
#[allow(clippy::too_many_lines)] // one resolution per configuration field
fn stream_target_config(
    current: Option<&fm_protocol::StreamTargetConfig>,
    options: &crate::args::RemoteStreamTargetOptions,
) -> RemoteResult<fm_protocol::StreamTargetConfig> {
    let missing =
        |option: &str| RemoteFailure(format!("a new stream destination requires {option}"));
    let (endpoint_protocol, endpoint) = match options.endpoint.as_deref() {
        Some(text) => {
            let (protocol, endpoint) = split_endpoint(text, "--endpoint")?;
            (protocol, Some(endpoint))
        }
        None => (None, None),
    };
    let protocol = match (options.protocol, endpoint_protocol) {
        (Some(given), Some(scheme)) if given != scheme => {
            return Err(RemoteFailure(
                "--protocol does not match the --endpoint URL scheme".into(),
            )
            .into());
        }
        (Some(protocol), _) | (None, Some(protocol)) => protocol,
        (None, None) => current
            .map(|config| config.protocol)
            .ok_or_else(|| missing("--protocol"))?,
    };
    let endpoint = match endpoint {
        Some(endpoint) => endpoint,
        None => current
            .map(|config| config.endpoint.clone())
            .ok_or_else(|| missing("--endpoint"))?,
    };
    let backup_endpoint = match &options.backup_endpoint {
        Some(text) => {
            let (scheme, backup) = split_endpoint(text, "--backup-endpoint")?;
            if scheme.is_some_and(|scheme| scheme != protocol) {
                return Err(RemoteFailure(format!(
                    "--backup-endpoint must use {}://, matching the primary",
                    protocol.token()
                ))
                .into());
            }
            Some(backup)
        }
        None if options.clear_backup_endpoint => None,
        // A keyless HLS destination has no backup server: moving to HLS
        // drops the stored one instead of sending it for the daemon to
        // refuse.
        None => current
            .and_then(|config| config.backup_endpoint.clone())
            .filter(|_| protocol.carries_key()),
    };
    let name = match &options.name {
        Some(name) => name.clone(),
        None => current
            .map(|config| config.name.clone())
            .ok_or_else(|| missing("--name"))?,
    };
    let output = match options.output {
        Some(output) => fm_protocol::WireOutputId::new(
            NonZeroU128::new(output)
                .ok_or_else(|| RemoteFailure("output ID must be nonzero".into()))?,
        ),
        None => current
            .map(|config| config.output)
            .ok_or_else(|| missing("--output"))?,
    };
    let config = fm_protocol::StreamTargetConfig {
        name,
        protocol,
        endpoint,
        backup_endpoint,
        output,
        reconcile_on_startup: options
            .reconcile_on_startup
            .or(current.map(|config| config.reconcile_on_startup))
            .unwrap_or(false),
        video_bitrate_kbps: options
            .video_bitrate_kbps
            .or(current.map(|config| config.video_bitrate_kbps))
            .unwrap_or(fm_model::DEFAULT_VIDEO_BITRATE_KBPS),
        reconnect: options.reconnect.map_or_else(
            || {
                current.map_or(fm_protocol::StreamReconnect::DEFAULT, |config| {
                    config.reconnect
                })
            },
            |policy| fm_protocol::StreamReconnect {
                initial_delay_ms: policy.initial_delay_ms(),
                max_delay_ms: policy.max_delay_ms(),
                max_attempts: policy.max_attempts(),
            },
        ),
        // SRT carries its latency: the given one, else the current one, else
        // the default. Every other protocol carries none.
        srt_latency_ms: if protocol == fm_protocol::StreamTargetProtocol::Srt {
            Some(
                options
                    .srt_latency_ms
                    .or(current.and_then(|config| config.srt_latency_ms))
                    .unwrap_or(fm_protocol::DEFAULT_STREAM_TARGET_SRT_LATENCY_MS),
            )
        } else if options.srt_latency_ms.is_some() {
            return Err(
                RemoteFailure("--srt-latency-ms requires an SRT destination".into()).into(),
            );
        } else {
            None
        },
    };
    if config.srt_latency_ms.is_some_and(|latency| {
        !(fm_protocol::MIN_STREAM_TARGET_SRT_LATENCY_MS
            ..=fm_protocol::MAX_STREAM_TARGET_SRT_LATENCY_MS)
            .contains(&latency)
    }) {
        return Err(RemoteFailure(format!(
            "SRT latency must be in {}..={} ms",
            fm_protocol::MIN_STREAM_TARGET_SRT_LATENCY_MS,
            fm_protocol::MAX_STREAM_TARGET_SRT_LATENCY_MS
        ))
        .into());
    }
    if !(fm_protocol::MIN_STREAM_TARGET_BITRATE_KBPS..=fm_protocol::MAX_STREAM_TARGET_BITRATE_KBPS)
        .contains(&config.video_bitrate_kbps)
    {
        return Err(RemoteFailure(format!(
            "video bitrate must be in {}..={} kbps",
            fm_protocol::MIN_STREAM_TARGET_BITRATE_KBPS,
            fm_protocol::MAX_STREAM_TARGET_BITRATE_KBPS
        ))
        .into());
    }
    if !config.is_valid() {
        return Err(RemoteFailure(format!(
            "invalid stream destination: the name must be 1..={} bytes and each endpoint \
             1..={} bytes of printable ASCII without spaces",
            fm_protocol::MAX_STREAM_TARGET_NAME_BYTES,
            fm_protocol::MAX_STREAM_TARGET_ENDPOINT_BYTES
        ))
        .into());
    }
    Ok(config)
}

/// Prints every stream destination of the daemon at `address` with its
/// configuration (URL without key, whether a key is stored) and its latest
/// status sample (OR-006). When a destination is desired running the
/// command waits briefly for the daemon's next status sample.
pub fn stream_targets(address: SocketAddr) -> RemoteResult<()> {
    let mut remote = Remote::connect(address, Role::Viewer)?;
    let running = remote
        .client
        .model()
        .state()
        .is_some_and(|state| state.streams().iter().any(|stream| stream.desired_running));
    if running {
        remote.await_stream_status(STREAM_STATUS_WAIT);
    }
    print_stream_targets(remote.stream_target_rows()?);
    Ok(())
}

/// How long `remote-streams` waits for a status sample of a running
/// destination before listing it without one.
const STREAM_STATUS_WAIT: Duration = Duration::from_millis(1_500);

/// One destination row: its configuration, the durable stream roster entry,
/// and the latest lossy status sample, if any.
struct StreamTargetRow {
    entry: fm_protocol::StreamTargetEntry,
    desired_running: Option<bool>,
    sample: Option<fm_protocol::StreamStatusSample>,
}

/// Prints one `StreamTarget(...)` line per destination, or
/// `StreamTargets=none`. The URL never carries the key: only `key=yes|no`.
fn print_stream_targets(rows: Vec<StreamTargetRow>) {
    if rows.is_empty() {
        println!("StreamTargets=none");
    }
    for row in rows {
        let config = &row.entry.config;
        let status = row.sample.as_ref().map_or_else(
            || "status=none".to_owned(),
            |sample| {
                format!(
                    "state={}, connected={}, kbps={}, dropped={}, reconnects={}, uptime_ms={}, last_error={}",
                    stream_realized_name(sample.realized),
                    sample.connected,
                    sample.bitrate_kbps,
                    sample.dropped_pairs,
                    sample.reconnects,
                    sample.uptime_ms,
                    sample.failure.as_deref().unwrap_or("none"),
                )
            },
        );
        // An SRT destination also says its latency and whether a passphrase
        // is stored, never the passphrase itself.
        let srt = config.srt_latency_ms.map_or_else(String::new, |latency| {
            format!(
                ", srt_latency_ms={latency}, passphrase={}",
                if row.entry.has_passphrase {
                    "yes"
                } else {
                    "no"
                }
            )
        });
        println!(
            "StreamTarget(id={}, name={:?}, protocol={}, url={:?}, backup_url={}, output={}, startup={}, video_bitrate_kbps={}, reconnect={}, key={}, ladder_rungs={}{srt}, desired={}, {status})",
            row.entry.target,
            config.name,
            config.protocol.token(),
            config.display_url(),
            config.backup_endpoint.as_ref().map_or_else(
                || "none".to_owned(),
                |backup| format!("{:?}", format!("{}://{backup}", config.protocol.token()))
            ),
            config.output,
            if config.reconcile_on_startup {
                "reconcile"
            } else {
                "stopped"
            },
            config.video_bitrate_kbps,
            crate::app::format_reconnect(
                config.reconnect.initial_delay_ms,
                config.reconnect.max_delay_ms,
                config.reconnect.max_attempts,
            ),
            if row.entry.has_key { "yes" } else { "no" },
            row.entry.ladder_rungs,
            row.desired_running
                .map_or_else(|| "unknown".to_owned(), |desired| desired.to_string()),
        );
    }
}

/// Prints one `NetworkStream(...)` line per network stream input, or
/// `NetworkStreams=none`. The URL is the redacted one.
pub(crate) fn print_network_streams(
    entries: impl IntoIterator<Item = fm_protocol::NetworkStreamEntry>,
) {
    let mut any = false;
    for entry in entries {
        any = true;
        println!("{}", format_network_stream(&entry));
    }
    if !any {
        println!("NetworkStreams=none");
    }
}

fn format_network_stream(entry: &fm_protocol::NetworkStreamEntry) -> String {
    let config = &entry.config;
    let settings = &config.settings;
    let status = &entry.status;
    let state = match status.state {
        fm_protocol::NetworkStreamState::Reconnecting {
            attempt,
            next_in_ms,
        } => format!("reconnecting(attempt={attempt}, next_in_ms={next_in_ms})"),
        state => state.token().to_owned(),
    };
    // The stream key and passphrase are secrets: only their presence shows.
    let secret = |value: &Option<String>| {
        if value.is_some() {
            fm_types::network_stream::REDACTED
        } else {
            "none"
        }
    };
    format!(
        "NetworkStream(input={}, url={:?}, transport={}, jitter_ms={}, backoff_ms={}..{}, give_up={}, timeout_ms={}, stall={}, mode={}, latency_ms={}, key_length={}, audio={}, stream_id={}, stream_key={}, passphrase={}, state={state}, bitrate_bps={}, buffer_ms={}, dropped_frames={}, late_frames={}, reconnects={}, source_channels={}, last_error={})",
        entry.input,
        config.url,
        settings.transport.token(),
        settings.jitter_buffer_ms,
        settings.reconnect.backoff_min_ms,
        settings.reconnect.backoff_max_ms,
        settings.reconnect.give_up.token(),
        settings.timeout_ms,
        settings.stall.token(),
        settings.mode.token(),
        settings.latency_ms,
        settings.key_length.token(),
        settings.audio.token(),
        config
            .stream_id
            .as_deref()
            .map_or_else(|| "none".to_owned(), |id| format!("{id:?}")),
        secret(&config.stream_key),
        secret(&config.passphrase),
        status.bitrate_bps,
        status.buffer_ms,
        status.dropped_frames,
        status.late_frames,
        status.reconnects,
        status.source_channels,
        status
            .last_error
            .as_deref()
            .map_or_else(|| "none".to_owned(), |error| format!("{error:?}")),
    )
}

/// Lists the capture inputs of the daemon at `address` with their status
/// (IN-001).
pub fn capture_inputs(address: SocketAddr) -> RemoteResult<()> {
    let remote = Remote::connect(address, Role::Viewer)?;
    let view = remote
        .client
        .model()
        .view()
        .ok_or_else(|| RemoteFailure("remote project view is unavailable".into()))?;
    print_capture_inputs(view.capture_inputs.iter().cloned());
    Ok(())
}

/// Edits capture input `input` of the daemon at `address`: the options
/// given replace the input's current settings (IN-001).
pub fn set_capture_settings(
    address: SocketAddr,
    input: fm_types::InputId,
    options: &crate::args::CaptureOptions,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> RemoteResult<()> {
    let mut remote = Remote::connect(address, Role::Graphics)?;
    let current = remote
        .client
        .model()
        .view()
        .and_then(|view| {
            view.capture_input(input)
                .map(|entry| entry.config.settings.clone())
        })
        .ok_or_else(|| RemoteFailure(format!("input {input} is not a capture input")))?;
    let payload = CommandPayload::SetCaptureSettings {
        input: fm_protocol::WireInputId::from_domain(input),
        settings: Box::new(options.apply(current)),
    };
    let key = match key {
        Some(key) => key,
        None => implicit_key(&payload)?,
    };
    remote.execute(payload, key, expected_revision, CommandCompletion::Project)
}

/// Prints one `CaptureInput(...)` line per capture input, or
/// `CaptureInputs=none`.
pub(crate) fn print_capture_inputs(
    entries: impl IntoIterator<Item = fm_protocol::CaptureInputEntry>,
) {
    let mut any = false;
    for entry in entries {
        any = true;
        println!("{}", format_capture_input(&entry));
    }
    if !any {
        println!("CaptureInputs=none");
    }
}

fn quoted_or_none(text: Option<&str>) -> String {
    text.map_or_else(|| "none".to_owned(), |text| format!("{text:?}"))
}

fn format_capture_input(entry: &fm_protocol::CaptureInputEntry) -> String {
    let settings = &entry.config.settings;
    let status = &entry.status;
    format!(
        "CaptureInput(input={}, device={:?}, format={}, audio={}, loss={}, state={}, reason={}, node={}, active_format={}, audio_live={}, frames={}, losses={}, recoveries={}, renegotiations={}, last_error={})",
        entry.input,
        entry.config.stable_key,
        settings
            .format
            .map_or_else(|| "auto".to_owned(), |format| format.token()),
        settings.audio.token(),
        settings.loss.token(),
        status.state.token(),
        status.reason.map_or("none", |reason| reason.token()),
        quoted_or_none(status.device.as_deref()),
        status
            .format
            .map_or_else(|| "none".to_owned(), |format| format.token()),
        if status.audio { "on" } else { "off" },
        status.frames,
        status.losses,
        status.recoveries,
        status.renegotiations,
        quoted_or_none(status.last_error.as_deref()),
    )
}

/// Lists the capture devices the daemon at `address` can open, with a
/// table of each device's formats (IN-001).
pub fn capture_devices(address: SocketAddr) -> RemoteResult<()> {
    let mut remote = Remote::connect(address, Role::Viewer)?;
    let deadline = Instant::now() + REMOTE_IO_TIMEOUT;
    let request_id = format!("cli-capture-{}", unique_client_id()?);
    remote.write(&WireMessage::CaptureDevicesRequest(
        fm_protocol::CaptureDevicesRequest {
            request_id: request_id.clone(),
        },
    ))?;
    let reply = loop {
        match remote.read_until(deadline)? {
            WireMessage::CaptureDevices(reply) if reply.request_id == request_id => break reply,
            message @ (WireMessage::Event(_) | WireMessage::RuntimeEvent(_)) => {
                remote.client.intake(message)?;
            }
            WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
            _ => {
                return Err(RemoteFailure("expected capture_devices from daemon".into()).into());
            }
        }
    };
    print_capture_devices(&reply.inventory);
    Ok(())
}

/// Prints a `CaptureDevices(...)` header, then per device a `Device(...)`
/// line followed by an aligned table of its formats.
fn print_capture_devices(inventory: &fm_protocol::CaptureInventory) {
    println!(
        "CaptureDevices(backend={}, devices={}, error={})",
        inventory.backend.as_deref().unwrap_or("none"),
        inventory.devices.len(),
        quoted_or_none(inventory.error.as_deref())
    );
    for device in &inventory.devices {
        println!(
            "Device(key={:?}, name={:?}, path={:?}, driver={:?}, bus={:?}, audio={}, formats={})",
            device.stable_key,
            device.name,
            device.path,
            device.driver,
            device.bus,
            quoted_or_none(device.audio.as_deref()),
            device.formats.len()
        );
        let rows: Vec<(String, String, &str)> = device
            .formats
            .iter()
            .map(|format| (format.token(), format.label(), format.class().label()))
            .collect();
        if rows.is_empty() {
            continue;
        }
        let token_width = rows
            .iter()
            .map(|row| row.0.len())
            .fold("FORMAT".len(), usize::max);
        let label_width = rows
            .iter()
            .map(|row| row.1.len())
            .fold("LABEL".len(), usize::max);
        println!(
            "  {:token_width$}  {:label_width$}  CLASS",
            "FORMAT", "LABEL"
        );
        for (token, label, class) in &rows {
            println!("  {token:token_width$}  {label:label_width$}  {class}");
        }
    }
}

/// Prints one `DisplayOutput(...)` line per configured slot, or
/// `DisplayOutputs=none`.
pub(crate) fn print_display_outputs<'a>(
    outputs: impl IntoIterator<
        Item = (
            fm_protocol::DisplayOutputSlot,
            &'a fm_protocol::DisplayOutputConfig,
            fm_protocol::WireDisplayOutputState,
        ),
    >,
) {
    let mut any = false;
    for (slot, config, state) in outputs {
        any = true;
        println!("{}", format_display_output(slot, config, &state));
    }
    if !any {
        println!("DisplayOutputs=none");
    }
}

fn format_display_output(
    slot: fm_protocol::DisplayOutputSlot,
    config: &fm_protocol::DisplayOutputConfig,
    state: &fm_protocol::WireDisplayOutputState,
) -> String {
    let state = match state {
        fm_protocol::WireDisplayOutputState::Waiting(reason) => {
            format!("state=waiting, reason={}", reason.as_str())
        }
        fm_protocol::WireDisplayOutputState::Active(active) => format!(
            "state=active, on={:?}, name={:?}, active_mode={}, mode_fallback={}, hdr_state={}",
            active.id.as_str(),
            active.name,
            active.mode,
            active.mode_fallback,
            active.hdr.as_str()
        ),
    };
    format!(
        "DisplayOutput(slot={slot}, display={}, source={}, mode={}, scaling={}, hdr={}, {state})",
        config.display, config.source, config.mode, config.scaling, config.hdr
    )
}

/// Lists the displays the daemon's presenter sees (Protocol 2.42).
pub fn display_inventory(address: SocketAddr) -> RemoteResult<()> {
    let mut remote = Remote::connect(address, Role::Viewer)?;
    let deadline = Instant::now() + REMOTE_IO_TIMEOUT;
    let request_id = format!("cli-displays-{}", unique_client_id()?);
    remote.write(&WireMessage::DisplayInventoryRequest(
        fm_protocol::DisplayInventoryRequest {
            request_id: request_id.clone(),
        },
    ))?;
    let reply = loop {
        match remote.read_until(deadline)? {
            WireMessage::DisplayInventory(reply) if reply.request_id == request_id => break reply,
            message @ (WireMessage::Event(_) | WireMessage::RuntimeEvent(_)) => {
                remote.client.intake(message)?;
            }
            WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
            _ => {
                return Err(RemoteFailure("expected display_inventory from daemon".into()).into());
            }
        }
    };
    let inventory = reply.inventory;
    println!(
        "DisplayInventory(presenter={}, displays={}, error={})",
        inventory.presenter,
        inventory.displays.len(),
        inventory
            .error
            .as_deref()
            .map_or_else(|| "none".to_owned(), |error| format!("{error:?}"))
    );
    for display in &inventory.displays {
        println!(
            "Display(id={:?}, name={:?}, position={},{}, mode={}, modes={}, hdr={})",
            display.id.as_str(),
            display.name,
            display.x,
            display.y,
            display.current_mode,
            display
                .modes
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(";"),
            display.hdr.as_str()
        );
    }
    Ok(())
}

fn print_diagnostics(response: &DiagnosticsResponse) {
    println!(
        "diagnostics=v1 engine_id={} state_epoch={} revision={} retained_oldest={} retained_newest={} subscribers={}/{} retained_limit={} subscriber_queue={}",
        sanitize_identity(&response.engine.engine_id),
        response.engine.state_epoch,
        response.current_revision,
        optional_number(response.oldest_retained_revision),
        optional_number(response.newest_retained_revision),
        response.subscriber_count,
        response.subscriber_limit,
        response.retained_events_limit,
        response.subscriber_queue_limit,
    );
}

/// One media input's settings and transport on one line (IN-003).
pub(crate) fn format_media_status(status: &fm_protocol::MediaStatus) -> String {
    let settings = &status.settings;
    let transport = &status.transport;
    let tracks = transport
        .tracks
        .iter()
        .map(|track| {
            format!(
                "{}:{}{}",
                track.stream_index,
                track.language.as_deref().unwrap_or("und"),
                if track.default { "*" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "Media(input={}, available={}, state={}, position={}, time={}/{}, end={}, frames={}, complete={}, loops={}, track={}, tracks=[{}], trim={}..{}, speed_bp={}, end_action={}, end_trigger={}, audio_policy={}, audio_stream={}, growing={}, start={}, fault={})",
        status.input,
        transport.available,
        transport.state.token(),
        transport.position,
        transport.position_time.numerator,
        transport.position_time.denominator,
        transport.playable_end,
        transport.frames,
        transport.complete,
        transport.loops,
        optional_number(transport.audio_track.map(u64::from)),
        tracks,
        settings.trim_in,
        settings
            .trim_out
            .map_or_else(|| "end".to_owned(), |frame| frame.to_string()),
        settings.speed_basis_points,
        settings.end_action.token(),
        settings
            .end_trigger_lead
            .map_or_else(|| "off".to_owned(), |lead| lead.to_string()),
        settings.audio_policy.token(),
        optional_number(settings.audio_stream.map(u64::from)),
        settings.growing.map_or_else(
            || "none".to_owned(),
            |growing| format!(
                "{}/{}",
                growing.live_edge_offset_frames, growing.idle_completion_refreshes
            )
        ),
        settings.start_frame,
        transport.fault.map_or("none", |fault| fault.token()),
    )
}

fn optional_number(value: Option<u64>) -> String {
    value.map_or_else(|| "none".to_owned(), |value| value.to_string())
}

fn sanitize_identity(value: &str) -> String {
    let sanitized = value
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
        .take(96)
        .collect::<String>();
    if sanitized.is_empty() {
        "unknown".to_owned()
    } else {
        sanitized
    }
}

pub fn execute(
    address: SocketAddr,
    payload: CommandPayload,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> RemoteResult<()> {
    let completion = CommandCompletion::for_payload(&payload);
    let role = match completion {
        CommandCompletion::Project
        | CommandCompletion::Padlock
        | CommandCompletion::Playlist { edit: true }
        | CommandCompletion::Cue { edit: true }
        | CommandCompletion::Media { transport: false } => Role::Graphics,
        CommandCompletion::Audio | CommandCompletion::Mixer { .. } => Role::Audio,
        CommandCompletion::FadeToBlackOutput
        | CommandCompletion::Switcher
        | CommandCompletion::Favorite
        | CommandCompletion::Playlist { edit: false }
        | CommandCompletion::Cue { edit: false }
        | CommandCompletion::Media { transport: true }
        | CommandCompletion::TitleTake {
            edits_fields: false,
        } => Role::Operator,
        // Undo and redo additionally require the class of the edit they
        // revert, which the CLI cannot know before the daemon resolves it. A
        // take that also edits fields needs both transition and project-edit
        // authority, which only Admin holds.
        CommandCompletion::History | CommandCompletion::TitleTake { edits_fields: true } => {
            Role::Admin
        }
    };
    let mut remote = Remote::connect(address, role)?;
    let key = match key {
        Some(key) => key,
        None => implicit_key(&payload)?,
    };
    remote.execute(payload, key, expected_revision, completion)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandCompletion {
    Project,
    Audio,
    Switcher,
    /// Undo or redo: the durable event and realized domain follow the edit
    /// the daemon resolves, so any project or switcher edit event completes it.
    History,
    Padlock,
    /// A title take: completes on `title_changed` and switcher realization.
    TitleTake {
        edits_fields: bool,
    },
    /// A playlist edit (`project`) or control (`switcher`): completes on
    /// `playlist_changed` and that domain's realization.
    Playlist {
        edit: bool,
    },
    /// A cue list edit (`project`) or cue control (`switcher`): completes on
    /// `cue_list_changed`/`cue_list_removed` or `cue_run_changed` and that
    /// domain's realization (Protocol 2.50).
    Cue {
        edit: bool,
    },
    /// A favourite transition assignment: a transition-class project edit
    /// that completes on `transition_favorite_changed` and project
    /// realization.
    Favorite,
    /// A media transport control or settings edit: completes on
    /// `media_changed` and media realization (Protocol 2.37). Transport is
    /// an operator control; settings are project edits.
    Media {
        transport: bool,
    },
    /// A mixer label, group, or group-level command (Protocol 2.40):
    /// completes on `audio_mixer_changed` and project (edits) or audio (live
    /// group levels) realization.
    Mixer {
        live: bool,
    },
    /// A Fade-to-Black output choice (Protocol 2.40): completes on
    /// `fade_to_black_output_changed` and project realization.
    FadeToBlackOutput,
}

impl CommandCompletion {
    fn for_payload(payload: &CommandPayload) -> Self {
        match payload {
            CommandPayload::RenameInput { .. }
            | CommandPayload::ReorderInputs { .. }
            | CommandPayload::AddInput { .. }
            | CommandPayload::RemoveInput { .. }
            | CommandPayload::InputLifecycle(_)
            | CommandPayload::SetTitleFields { .. }
            | CommandPayload::SetDisplayOutput { .. }
            | CommandPayload::ClearDisplayOutput { .. }
            | CommandPayload::SetNetworkStream { .. }
            | CommandPayload::SetCaptureSettings { .. }
            | CommandPayload::SetStreamTarget { .. }
            | CommandPayload::RemoveStreamTarget { .. }
            | CommandPayload::SceneLayer(_) => Self::Project,
            CommandPayload::SetInputAudioStrip { .. } => Self::Audio,
            CommandPayload::AudioMixer(operation) => Self::Mixer {
                live: operation.is_live(),
            },
            CommandPayload::SetFadeToBlackOutput { .. }
            | CommandPayload::SetFadeToBlackOutputDuration { .. }
            | CommandPayload::SetFadeToBlackCurve { .. } => Self::FadeToBlackOutput,
            CommandPayload::Undo { .. } | CommandPayload::Redo { .. } => Self::History,
            CommandPayload::SetEditLock { .. } => Self::Padlock,
            CommandPayload::TakeTitle { fields, .. } => Self::TitleTake {
                edits_fields: !fields.is_empty(),
            },
            CommandPayload::SetPlaylist { .. } => Self::Playlist { edit: true },
            CommandPayload::SetTransitionFavorite { .. } => Self::Favorite,
            CommandPayload::Media { operation, .. } => Self::Media {
                transport: operation.is_transport(),
            },
            CommandPayload::PlaylistControl { .. } | CommandPayload::PlaylistAdvance { .. } => {
                Self::Playlist { edit: false }
            }
            CommandPayload::SetCueList { .. } | CommandPayload::RemoveCueList { .. } => {
                Self::Cue { edit: true }
            }
            CommandPayload::CueControl { .. } | CommandPayload::CueAdvance { .. } => {
                Self::Cue { edit: false }
            }
            _ => Self::Switcher,
        }
    }

    fn realizes(self, domain: &str) -> bool {
        match self {
            Self::Project
            | Self::Padlock
            | Self::Favorite
            | Self::Playlist { edit: true }
            | Self::Cue { edit: true }
            | Self::Mixer { live: false }
            | Self::FadeToBlackOutput => domain == "project",
            Self::Audio | Self::Mixer { live: true } => domain == "audio",
            Self::Media { .. } => domain == "media",
            Self::Switcher
            | Self::TitleTake { .. }
            | Self::Playlist { edit: false }
            | Self::Cue { edit: false } => domain == "switcher",
            Self::History => matches!(domain, "project" | "switcher"),
        }
    }
}

struct Remote {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    client: Client,
    project_id: ProjectId,
    server: ServerIdentity,
    last_stream_status_sequence: Option<u64>,
    latest_stream_status: Option<StreamStatusMessage>,
}

impl Remote {
    fn connect(address: SocketAddr, role: Role) -> RemoteResult<Self> {
        if !address.ip().is_loopback() {
            return Err(RemoteFailure(format!(
                "development mode requires a loopback address, got {}",
                address.ip()
            ))
            .into());
        }

        let stream = TcpStream::connect_timeout(&address, REMOTE_IO_TIMEOUT)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(REMOTE_IO_TIMEOUT))?;
        stream.set_write_timeout(Some(REMOTE_IO_TIMEOUT))?;
        let mut remote = Self::uninitialized(stream)?;
        remote.write(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: format!("freemix-cli-{}", env!("CARGO_PKG_VERSION")),
            client_type: ClientType::Cli,
            desired_role: role,
            resume_cursor: None,
        }))?;

        let response = match remote.read()? {
            WireMessage::HandshakeResponse(response) => response,
            WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
            _ => {
                return Err(
                    RemoteFailure("expected handshake_response during handshake".into()).into(),
                );
            }
        };
        match &response.outcome {
            HandshakeOutcome::Snapshot { .. } => {}
            HandshakeOutcome::Resume { .. } => {
                return Err(
                    RemoteFailure("daemon selected resume without a client cursor".into()).into(),
                );
            }
            HandshakeOutcome::Rejected { error } => return Err(protocol_error(error).into()),
        }

        let project_id = project_id(&response.server)?;
        let server = response.server.clone();
        let mut client = Client::new(ClientConfig::new(
            env!("CARGO_PKG_VERSION"),
            ClientType::Cli,
            role,
            unique_client_id()?,
            project_id,
        ))?;
        client.start_connect()?;
        let request = client.transport_connected()?;
        if request.resume_cursor.is_some() {
            return Err(
                RemoteFailure("fresh client unexpectedly supplied a resume cursor".into()).into(),
            );
        }
        client.accept_handshake(response)?;
        match remote.read()? {
            WireMessage::Snapshot(snapshot) => {
                client.intake(WireMessage::Snapshot(snapshot))?;
            }
            WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
            _ => {
                return Err(
                    RemoteFailure("expected initial snapshot after handshake".into()).into(),
                );
            }
        }
        // The title, playlist, grade, title and scene animation, title and
        // scene layout, and animation preset states the snapshot announced
        // complete it.
        while client.state() != &ConnectionState::Ready {
            match remote.read()? {
                message @ (WireMessage::TitleState(_)
                | WireMessage::PlaylistState(_)
                | WireMessage::GradeState(_)
                | WireMessage::TitleAnimationState(_)
                | WireMessage::TitleLayoutState(_)
                | WireMessage::SceneAnimationState(_)
                | WireMessage::SceneLayoutState(_)
                | WireMessage::AnimationPresetState(_)
                | WireMessage::MediaState(_)
                | WireMessage::CueListState(_)) => {
                    client.intake(message)?;
                }
                WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
                _ => {
                    return Err(RemoteFailure(
                        "expected the snapshot's title, playlist, grade, animation, layout, and preset states"
                            .into(),
                    )
                    .into());
                }
            }
        }

        remote.client = client;
        remote.project_id = project_id;
        remote.server = server;
        Ok(remote)
    }

    fn uninitialized(stream: TcpStream) -> RemoteResult<Self> {
        let placeholder = ProjectId::new(NonZeroU128::MIN);
        Ok(Self {
            writer: stream.try_clone()?,
            reader: BufReader::new(stream),
            client: Client::new(ClientConfig::new(
                env!("CARGO_PKG_VERSION"),
                ClientType::Cli,
                Role::Operator,
                "uninitialized",
                placeholder,
            ))?,
            project_id: placeholder,
            server: ServerIdentity {
                engine_id: "uninitialized".into(),
                project_id: placeholder.to_string(),
                state_epoch: 0,
                log_id: "uninitialized".into(),
            },
            last_stream_status_sequence: None,
            latest_stream_status: None,
        })
    }

    fn execute(
        &mut self,
        payload: CommandPayload,
        key: String,
        expected_revision: Option<u64>,
        completion: CommandCompletion,
    ) -> RemoteResult<()> {
        let queued = self
            .client
            .queue_command(payload, key, expected_revision, None)?;
        let Some(Outbound::Command(command)) = self.client.pop_outbound() else {
            return Err(
                RemoteFailure("client did not queue the command for transport".into()).into(),
            );
        };
        debug_assert_eq!(command, queued);
        self.write(&WireMessage::Command(command.clone()))?;

        let result = loop {
            match self.read()? {
                WireMessage::CommandResult(result) => break result,
                message @ (WireMessage::Event(_) | WireMessage::RuntimeEvent(_)) => {
                    self.client.intake(message)?;
                }
                WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
                _ => return Err(RemoteFailure("expected command_result from daemon".into()).into()),
            }
        };
        if result_id(&result) != command.id {
            return Err(
                RemoteFailure("command result does not match the sent command".into()).into(),
            );
        }
        let intake = self
            .client
            .intake(WireMessage::CommandResult(result.clone()))?;
        if intake != Intake::ResultReconciled {
            return Err(RemoteFailure("client did not reconcile the command result".into()).into());
        }

        match result {
            CommandResult::Rejected {
                code,
                message,
                reason,
                ..
            } => Err(RemoteFailure(refusal_text(&code, &message, reason.as_ref())).into()),
            CommandResult::Accepted { revision, .. } => {
                let already_applied = self
                    .client
                    .model()
                    .reconnect_cursor()
                    .is_some_and(|cursor| cursor.revision.get() >= revision);
                if !already_applied {
                    self.read_command_events(revision, completion)?;
                }
                self.print_status()
            }
        }
    }

    #[allow(clippy::too_many_lines)] // one arm per durable event kind
    fn read_command_events(
        &mut self,
        revision: u64,
        completion: CommandCompletion,
    ) -> RemoteResult<()> {
        let mut durable = false;
        let mut realized = false;
        while !durable || !realized {
            match self.read()? {
                WireMessage::Event(event) => {
                    let matches_revision = event.cursor.revision == revision;
                    let expected_event = match &event.payload {
                        EventPayload::InputRenamed { .. }
                        | EventPayload::InputOrderChanged { .. }
                        | EventPayload::InputAdded { .. }
                        | EventPayload::InputRemoved { .. }
                        | EventPayload::InputLifecycleChanged { .. }
                        | EventPayload::GradeChanged(_)
                        | EventPayload::AnimationPresetChanged(_)
                        | EventPayload::TitleAnimationChanged { .. }
                        | EventPayload::SceneAnimationChanged { .. }
                        | EventPayload::SceneLayoutChanged { .. }
                        | EventPayload::DisplayOutputChanged { .. }
                        | EventPayload::NetworkStreamChanged { .. }
                        | EventPayload::CaptureSettingsChanged { .. }
                        | EventPayload::StreamTargetChanged { .. } => matches!(
                            completion,
                            CommandCompletion::Project | CommandCompletion::History
                        ),
                        EventPayload::EditLockChanged { .. } => {
                            completion == CommandCompletion::Padlock
                        }
                        EventPayload::TitleChanged { .. } => matches!(
                            completion,
                            CommandCompletion::Project
                                | CommandCompletion::History
                                | CommandCompletion::TitleTake { .. }
                        ),
                        EventPayload::DesiredSwitcher { .. }
                        | EventPayload::StingerSlotsChanged { .. }
                        | EventPayload::StreamsChanged { .. }
                        | EventPayload::RecordingChanged { .. } => !matches!(
                            completion,
                            CommandCompletion::Project
                                | CommandCompletion::Padlock
                                | CommandCompletion::TitleTake { .. }
                                | CommandCompletion::Playlist { .. }
                                | CommandCompletion::Cue { .. }
                        ),
                        EventPayload::PlaylistChanged { .. } => {
                            matches!(completion, CommandCompletion::Playlist { .. })
                        }
                        EventPayload::CueListChanged(_) | EventPayload::CueListRemoved { .. } => {
                            matches!(
                                completion,
                                CommandCompletion::Cue { edit: true } | CommandCompletion::History
                            )
                        }
                        EventPayload::CueRunChanged(_) => {
                            completion == CommandCompletion::Cue { edit: false }
                        }
                        EventPayload::TransitionFavoriteChanged { .. } => matches!(
                            completion,
                            CommandCompletion::Favorite | CommandCompletion::History
                        ),
                        EventPayload::MediaChanged { .. } => {
                            matches!(completion, CommandCompletion::Media { .. })
                        }
                        EventPayload::AudioMixerChanged(_) => matches!(
                            completion,
                            CommandCompletion::Mixer { .. } | CommandCompletion::History
                        ),
                        EventPayload::FadeToBlackOutputChanged { .. }
                        | EventPayload::FadeToBlackOutputDurationChanged { .. }
                        | EventPayload::FadeToBlackCurveChanged { .. } => matches!(
                            completion,
                            CommandCompletion::FadeToBlackOutput | CommandCompletion::History
                        ),
                    };
                    if matches_revision && !expected_event {
                        return Err(RemoteFailure(
                            "command event has the wrong durable domain".into(),
                        )
                        .into());
                    }
                    self.client.intake(WireMessage::Event(event))?;
                    durable |= matches_revision;
                }
                WireMessage::RuntimeEvent(runtime) => {
                    let matches_realization = runtime.revision == revision
                        && matches!(
                            &runtime.event,
                            RuntimeLifecycleEvent::Realized { domain, .. }
                                if completion.realizes(domain)
                        );
                    self.client.intake(WireMessage::RuntimeEvent(runtime))?;
                    realized |= matches_realization;
                }
                WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
                _ => {
                    return Err(RemoteFailure(
                        "expected durable/runtime command event from daemon".into(),
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    fn print_status(&self) -> RemoteResult<()> {
        let state = self
            .client
            .model()
            .state()
            .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?;
        let cursor = self
            .client
            .model()
            .reconnect_cursor()
            .ok_or_else(|| RemoteFailure("remote project cursor is unavailable".into()))?;
        let switcher = state.switcher();
        println!(
            "project_id={} show={:?} revision={} frame=unavailable Program(desired={}, realized={}) Preview(desired={}, realized={}) TBar(desired={}, realized={}) FTB(desired={}, realized={}) AudioStrips={} Streams={} Recording=desired={} Overlays(desired={}, realized={}) Inputs={} Outputs={} EditLocks={} History={} Titles={} Mixer={} FtbExcluded={} FtbDurations={} FtbCurve={}",
            self.project_id,
            state.show_name(),
            cursor.revision,
            switcher.desired.program,
            switcher.realized.program,
            switcher.desired.preview,
            switcher.realized.preview,
            format_manual_transition(switcher.desired_manual_transition),
            format_manual_transition(switcher.realized_manual_transition),
            format_fade_to_black(switcher.desired_fade_to_black),
            format_fade_to_black(switcher.realized_fade_to_black),
            format_input_audio_strips(state),
            format_streams(state.streams()),
            format_recording_desired(state),
            format_overlays(state.desired_overlays()),
            format_overlays(state.realized_overlays()),
            format_input_roster(state),
            format_output_roster(state),
            format_edit_locks(state.edit_locks()),
            format_edit_history(state.edit_history()),
            format_titles(state.titles()),
            format_audio_mixer(state),
            format_fade_to_black_excluded(state),
            format_fade_to_black_durations(state),
            state.fade_to_black_curve().token(),
        );
        if let Some(playlist) = state.playlist() {
            println!(
                "Playlist(frame={}, loop={:?}, state={:?}, entries={})",
                playlist.frame,
                playlist.loop_mode,
                playlist.state,
                playlist.entries.len()
            );
        }
        for status in state.media().values() {
            println!("{}", format_media_status(status));
        }
        for (slot, config) in state.display_outputs() {
            println!(
                "{}",
                format_display_output(*slot, config, &state.display_output_state(*slot))
            );
        }
        if let Some(status) = &self.latest_stream_status {
            for sample in &status.samples {
                println!(
                    "StreamStatus(target={}, realized={}, connected={}, muxed_bytes={}, enqueued_pairs={}, dropped_pairs={}, bitrate_kbps={}, reconnects={}, uptime_ms={}, failure={})",
                    sample.target,
                    stream_realized_name(sample.realized),
                    sample.connected,
                    sample.muxed_bytes,
                    sample.enqueued_pairs,
                    sample.dropped_pairs,
                    sample.bitrate_kbps,
                    sample.reconnects,
                    sample.uptime_ms,
                    sample.failure.as_deref().unwrap_or("none"),
                );
            }
        }
        Ok(())
    }

    /// Every configured destination with its durable roster entry and the
    /// latest status sample this session retained (OR-006).
    fn stream_target_rows(&self) -> RemoteResult<Vec<StreamTargetRow>> {
        let state = self
            .client
            .model()
            .state()
            .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?;
        Ok(state
            .stream_targets()
            .iter()
            .map(|entry| StreamTargetRow {
                entry: entry.clone(),
                desired_running: state
                    .streams()
                    .iter()
                    .find(|stream| stream.target == entry.target.get())
                    .map(|stream| stream.desired_running),
                sample: self.latest_stream_status.as_ref().and_then(|status| {
                    status
                        .samples
                        .iter()
                        .find(|sample| sample.target == entry.target)
                        .cloned()
                }),
            })
            .collect())
    }

    /// Reads (and replicates) daemon messages until the next stream status
    /// sample arrives or `wait` elapses. A timeout or closed connection only
    /// ends the wait: the listing then shows no sample.
    fn await_stream_status(&mut self, wait: Duration) {
        let deadline = Instant::now() + wait;
        loop {
            match self.read_until_inner(Some(deadline), true) {
                Ok(WireMessage::StreamStatus(_)) | Err(_) => return,
                Ok(message @ (WireMessage::Event(_) | WireMessage::RuntimeEvent(_))) => {
                    if self.client.intake(message).is_err() {
                        return;
                    }
                }
                Ok(_) => {}
            }
        }
    }

    /// Validates and retains one lossy stream-status record. Identity must
    /// match the established session and sequences must be strictly increasing;
    /// violations mirror the TCP session's disconnect-on-meters behavior.
    fn absorb_stream_status(&mut self, status: StreamStatusMessage) -> RemoteResult<()> {
        if status.server != self.server {
            return Err(
                RemoteFailure("stream status identity does not match the session".into()).into(),
            );
        }
        if self
            .last_stream_status_sequence
            .is_some_and(|sequence| status.sequence <= sequence)
        {
            return Err(
                RemoteFailure("stream status sequence is not strictly increasing".into()).into(),
            );
        }
        self.last_stream_status_sequence = Some(status.sequence);
        self.latest_stream_status = Some(status);
        Ok(())
    }

    fn write(&mut self, message: &WireMessage) -> RemoteResult<()> {
        let line = encode_line(message)?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    fn read(&mut self) -> RemoteResult<WireMessage> {
        self.read_until_inner(None, false)
    }

    fn read_until(&mut self, deadline: Instant) -> RemoteResult<WireMessage> {
        self.read_until_inner(Some(deadline), false)
    }

    /// Reads the next message. A stream status record is always absorbed;
    /// with `yield_stream_status` it is also returned instead of skipped.
    fn read_until_inner(
        &mut self,
        deadline: Option<Instant>,
        yield_stream_status: bool,
    ) -> RemoteResult<WireMessage> {
        let mut line = Vec::with_capacity(MAX_LINE_BYTES);
        loop {
            if let Some(deadline) = deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(RemoteFailure("daemon response timed out".into()).into());
                }
                self.reader.get_mut().set_read_timeout(Some(remaining))?;
            }
            let (count, complete) = {
                let buffer = match self.reader.fill_buf() {
                    Ok(buffer) => buffer,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                        ) =>
                    {
                        return Err(RemoteFailure("daemon response timed out".into()).into());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                };
                if buffer.is_empty() {
                    return Err(RemoteFailure("daemon closed the TCP connection".into()).into());
                }
                let complete = buffer.iter().position(|byte| *byte == b'\n');
                let count = complete.map_or(buffer.len(), |index| index + 1);
                if line.len() + count > MAX_LINE_BYTES {
                    return Err(fm_protocol::CodecError::LineTooLong.into());
                }
                line.extend_from_slice(&buffer[..count]);
                (count, complete.is_some())
            };
            self.reader.consume(count);
            if complete {
                let message = decode_line(core::str::from_utf8(&line)?)?;
                if let WireMessage::StreamStatus(status) = message {
                    if yield_stream_status {
                        self.absorb_stream_status(status.clone())?;
                        return Ok(WireMessage::StreamStatus(status));
                    }
                    self.absorb_stream_status(status)?;
                    line.clear();
                    continue;
                }
                return Ok(message);
            }
            if line.len() == MAX_LINE_BYTES {
                return Err(fm_protocol::CodecError::LineTooLong.into());
            }
        }
    }
}

fn format_edit_locks(locks: &fm_ui_model::EditLocks) -> String {
    format!(
        "project:{},inputs:[{}]",
        if locks.project { "on" } else { "off" },
        locks
            .inputs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(","),
    )
}

fn format_titles(titles: &[fm_ui_model::TitleStatus]) -> String {
    format!(
        "[{}]",
        titles
            .iter()
            .map(|title| {
                format!(
                    "{}:{:?}@{}{{{}}}",
                    title.input,
                    title.name,
                    title.field_revision,
                    title
                        .fields
                        .iter()
                        .map(|field| format!(
                            "{}:{:?}={}",
                            field.field,
                            field.name,
                            format_title_value(&field.value)
                        ))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn format_title_value(value: &fm_protocol::TitleFieldValue) -> String {
    match value {
        fm_protocol::TitleFieldValue::Text(text) => format!("{text:?}"),
        fm_protocol::TitleFieldValue::Number(number) => number.to_string(),
        fm_protocol::TitleFieldValue::Color {
            red,
            green,
            blue,
            alpha,
        } => format!("#{red:02x}{green:02x}{blue:02x}{alpha:02x}"),
        fm_protocol::TitleFieldValue::Image { asset_uri } => format!("image:{asset_uri:?}"),
    }
}

fn format_edit_history(history: fm_protocol::EditHistoryStatus) -> String {
    format!(
        "undo:{}@{},redo:{}@{}",
        history.undo_depth,
        optional_number(history.next_undo),
        history.redo_depth,
        optional_number(history.next_redo),
    )
}

fn format_input_audio_strips(state: &fm_ui_model::ProjectState) -> String {
    format!(
        "[{}]",
        state
            .input_audio_strips()
            .iter()
            .map(|status| {
                format!(
                    "{}:{:?}:{}:{}:{}:{}:{}:{}",
                    status.input,
                    state.input_name(status.input).unwrap_or(""),
                    status.gain_millidb,
                    status.balance_basis_points,
                    status.muted,
                    status.soloed,
                    status.follow_video,
                    status.delay_samples
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Strip pan, groups, and labels, then the group faders (AU-001):
/// `[input:pan:group|-:"label",...]/[n:"name":gain:muted,...]`.
fn format_audio_mixer(state: &fm_ui_model::ProjectState) -> String {
    let mixer = state.audio_mixer();
    let strips = state
        .input_audio_strips()
        .iter()
        .map(|status| {
            format!(
                "{}:{}:{}:{:?}",
                status.input,
                status.pan_basis_points,
                status
                    .group
                    .map_or_else(|| "-".to_owned(), |group| group.to_string()),
                mixer.label(fm_protocol::WireInputId::from_domain(status.input)),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let groups = mixer
        .groups
        .iter()
        .zip(1_u8..)
        .map(|(group, number)| {
            format!(
                "{number}:{:?}:{}:{}:{}:{}",
                group.name,
                group.gain_millidb,
                group.muted,
                group.soloed,
                group
                    .parent
                    .map_or_else(|| "-".to_owned(), |parent| parent.to_string())
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "[{strips}]/[{groups}]/solo={}/monitor={}:{}:{}",
        mixer.solo_mode.token(),
        mixer.monitor.device.as_deref().unwrap_or("-"),
        mixer.monitor.gain_millidb,
        mixer.monitor.muted
    )
}

/// The outputs Fade to Black leaves live (SW-004).
fn format_fade_to_black_excluded(state: &fm_ui_model::ProjectState) -> String {
    format!(
        "[{}]",
        state
            .fade_to_black_excluded_outputs()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Every output fading over its own duration, as `output:frames`.
fn format_fade_to_black_durations(state: &fm_ui_model::ProjectState) -> String {
    format!(
        "[{}]",
        state
            .fade_to_black_output_durations()
            .iter()
            .map(|(output, frames)| format!("{output}:{frames}"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn format_input_roster(state: &fm_ui_model::ProjectState) -> String {
    format!(
        "[{}]",
        state
            .inputs()
            .iter()
            .map(|input| format!("{}:{:?}", input, state.input_name(*input).unwrap_or("")))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn format_output_roster(state: &fm_ui_model::ProjectState) -> String {
    format!(
        "[{}]",
        state
            .outputs()
            .iter()
            .map(|output| format!("{}:{:?}", output.output, output.name))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Renders the latest stream projection the client retained from the snapshot
/// and `StreamsChanged` events.
fn format_streams(streams: &[fm_ui_model::StreamStatus]) -> String {
    format!(
        "[{}]",
        streams
            .iter()
            .map(|stream| format!(
                "{}:{:?}:desired={}:realized={}:detail={}",
                stream.target,
                stream.name,
                stream.desired_running,
                stream_realized_name(stream.realized),
                stream.detail.as_deref().unwrap_or("none"),
            ))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Renders both recorder slots' desired flags in slot order, for example
/// `[false,true]` for slot 1 stopped and slot 2 recording.
fn format_recording_desired(state: &ProjectState) -> String {
    let mut formatted = String::from("[");
    for (index, slot) in fm_model::RecorderSlot::ALL.into_iter().enumerate() {
        if index != 0 {
            formatted.push(',');
        }
        let recorder = fm_protocol::WireRecorderSlot::new(slot.get())
            .expect("model recorder slots are wire recorder slots");
        formatted.push_str(&state.recording_desired(recorder).to_string());
    }
    formatted.push(']');
    formatted
}

const fn stream_realized_name(state: fm_protocol::StreamRealizedState) -> &'static str {
    match state {
        StreamRealizedState::Stopped => "stopped",
        StreamRealizedState::Starting => "starting",
        StreamRealizedState::Live => "live",
        StreamRealizedState::WaitingToReconnect => "waiting-to-reconnect",
        StreamRealizedState::Congested => "congested",
        StreamRealizedState::Failed => "failed",
        StreamRealizedState::Unavailable => "unavailable",
    }
}

fn format_fade_to_black(state: FadeToBlackState) -> String {
    format!(
        "{}@{}/{}",
        if state.target_active { "black" } else { "live" },
        state.position.numerator(),
        fm_protocol::FadeToBlackPosition::DENOMINATOR,
    )
}

fn format_manual_transition(status: ManualTransitionStatus) -> String {
    match status {
        ManualTransitionStatus::Inactive => "inactive".to_owned(),
        ManualTransitionStatus::Active(state) => format!(
            "{}:{}->{}@{}",
            state.kind.token(),
            state.from,
            state.to,
            state.position.basis_points(),
        ),
    }
}

fn format_overlays(overlays: &[fm_ui_model::OverlayStatus]) -> String {
    format!(
        "[{}]",
        overlays
            .iter()
            .map(|overlay| format!(
                "{}:{}:{}:opacity={}:{}@{}:{}:{}:queue=[{}]",
                overlay.channel,
                overlay
                    .source
                    .map_or_else(|| "none".to_owned(), |source| source.to_string()),
                if overlay.active { "on" } else { "off" },
                overlay.opacity,
                crate::args::cli_token(overlay.transition.token()),
                overlay.duration_frames,
                match overlay.position {
                    fm_protocol::OverlayPositionPreset::FullFrame => "full-frame",
                    fm_protocol::OverlayPositionPreset::TopLeft => "top-left",
                    fm_protocol::OverlayPositionPreset::TopRight => "top-right",
                    fm_protocol::OverlayPositionPreset::BottomLeft => "bottom-left",
                    fm_protocol::OverlayPositionPreset::BottomRight => "bottom-right",
                },
                match overlay.border {
                    fm_protocol::OverlayBorderPreset::None => "none",
                    fm_protocol::OverlayBorderPreset::ThinWhite => "thin-white",
                    fm_protocol::OverlayBorderPreset::ThickWhite => "thick-white",
                },
                overlay
                    .queued_sources
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("+"),
            ))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn project_id(server: &ServerIdentity) -> RemoteResult<ProjectId> {
    let value = server.project_id.parse::<u128>()?;
    NonZeroU128::new(value)
        .map(ProjectId::new)
        .ok_or_else(|| RemoteFailure("daemon project ID is zero".into()).into())
}

fn protocol_error(error: &fm_protocol::StructuredError) -> RemoteFailure {
    RemoteFailure(format!("{}: {}", error.code, error.message))
}

fn result_id(result: &CommandResult) -> &str {
    match result {
        CommandResult::Accepted { id, .. } | CommandResult::Rejected { id, .. } => id,
    }
}

fn implicit_key(payload: &CommandPayload) -> RemoteResult<String> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!(
        "remote-cli:{timestamp:032x}:{:08x}:{payload:?}",
        std::process::id()
    ))
}

fn unique_client_id() -> RemoteResult<String> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!(
        "freemix-cli-{}-{timestamp:032x}",
        std::process::id()
    ))
}

#[derive(Debug)]
struct RemoteFailure(String);

impl core::fmt::Display for RemoteFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for RemoteFailure {}

/// A refused command as the CLI prints it: the coarse code, the daemon's
/// English text, and the typed reason in its stable wire form (Protocol
/// 2.45), such as `conflict: input 2 cannot be removed: it is a layer source
/// of scene 100 (reason: input_in_use:2:scene_layer:100)`.
pub(crate) fn refusal_text(
    code: &str,
    message: &str,
    reason: Option<&fm_protocol::RefusalReason>,
) -> String {
    match reason.map(fm_protocol::encode_refusal_reason) {
        Some(Ok(reason)) => format!("{code}: {message} (reason: {reason})"),
        _ => format!("{code}: {message}"),
    }
}

#[cfg(test)]
mod refusal_text_tests {
    use fm_protocol::{InputUse, RefusalReason};

    #[test]
    fn a_typed_reason_is_printed_in_its_wire_form() {
        let reason = RefusalReason::InputInUse {
            input: 2,
            usage: InputUse::SceneLayer { scene: 100 },
        };
        assert_eq!(
            super::refusal_text("conflict", "input 2 is in use", Some(&reason)),
            "conflict: input 2 is in use (reason: input_in_use:2:scene_layer:100)"
        );
        assert_eq!(
            super::refusal_text("internal", "boom", None),
            "internal: boom"
        );
    }
}
