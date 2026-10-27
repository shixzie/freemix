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
    HeartbeatMessage, MAX_LINE_BYTES, Role, RuntimeLifecycleEvent, ServerIdentity,
    StreamRealizedState, StreamStatusMessage, WireMessage, decode_line, encode_line,
};
use fm_types::ProjectId;
use fm_ui_model::{ManualTransitionStatus, ProjectState};

type RemoteResult<T> = Result<T, Box<dyn Error>>;

const REMOTE_IO_TIMEOUT: Duration = Duration::from_secs(2);
/// How often a command waiting for its realization heartbeats: well inside
/// the daemon's 15 s session timeout.
const REALIZATION_HEARTBEAT_EVERY: Duration = Duration::from_secs(1);
/// The longest a command waits for its realization while the daemon keeps
/// answering: an hour, past any transition, title phase, or cue wait.
const REALIZATION_WAIT_LIMIT: Duration = Duration::from_hours(1);
/// The longest `remote-reel-export` waits for its job to finish: an hour,
/// like a command waiting for its realization.
const REEL_EXPORT_WAIT_LIMIT: Duration = Duration::from_hours(1);
/// How often `remote-reel-export` asks for the reel status: the daemon
/// paces its own pushes at 250 ms while a job changes.
const REEL_EXPORT_POLL_INTERVAL: Duration = Duration::from_millis(250);

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

/// Asks the daemon for its replay status (OR-012, Protocol 2.106).
fn request_replay_status(remote: &mut Remote) -> RemoteResult<fm_protocol::ReplayStatusMessage> {
    let deadline = Instant::now() + REMOTE_IO_TIMEOUT;
    let request_id = format!("cli-replay-{}", unique_client_id()?);
    remote.write(&WireMessage::ReplayStatusRequest(
        fm_protocol::ReplayStatusRequest {
            request_id: request_id.clone(),
        },
    ))?;
    loop {
        match remote.read_until(deadline)? {
            // The answer echoes this request's id; a pushed record has none
            // and carries the same latest-wins replay state, so taking it
            // avoids waiting out the timeout when a change races the request.
            WireMessage::ReplayStatus(status)
                if status.request_id.as_deref() == Some(request_id.as_str())
                    || status.request_id.is_none() =>
            {
                return Ok(*status);
            }
            message @ (WireMessage::Event(_) | WireMessage::RuntimeEvent(_)) => {
                remote.client.intake(message)?;
            }
            WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
            // Another client's answer is not ours; meters, stream status,
            // and snapshots are not the answer either. Keep waiting.
            _ => {}
        }
    }
}

/// Acknowledges, silences, or unsilences one daemon alert rule (RC-015,
/// Protocol 2.109), then prints the daemon's health as it answered. Only an
/// Operator or Admin may.
pub fn alert(
    address: SocketAddr,
    rule: fm_protocol::HealthAlertRule,
    action: fm_protocol::HealthAlertAction,
) -> RemoteResult<()> {
    let mut remote = Remote::connect(address, Role::Operator)?;
    let request_id = format!("cli-alert-{}", unique_client_id()?);
    remote.write(&WireMessage::HealthAlertRequest(
        fm_protocol::HealthAlertRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: request_id.clone(),
            rule,
            action,
        },
    ))?;
    let deadline = Instant::now() + REMOTE_IO_TIMEOUT;
    loop {
        match remote.read_until(deadline)? {
            WireMessage::HealthStatus(status)
                if status.request_id.as_deref() == Some(request_id.as_str()) =>
            {
                for line in format_health(&status.health) {
                    println!("{line}");
                }
                return Ok(());
            }
            WireMessage::Error(error)
                if error.request_id.as_deref() == Some(request_id.as_str()) =>
            {
                return Err(protocol_error(&error.error).into());
            }
            message @ (WireMessage::Event(_) | WireMessage::RuntimeEvent(_)) => {
                remote.client.intake(message)?;
            }
            _ => {}
        }
    }
}

/// One `AlertRule(...)` line per rule: the show's setting, in the rule's
/// unit, and whether it is the rule's default (Protocol 2.112).
pub(crate) fn format_alert_rules(settings: &[fm_protocol::AlertRuleSetting]) -> Vec<String> {
    fm_protocol::HealthAlertRule::ALL
        .into_iter()
        .map(|rule| {
            let setting = fm_protocol::effective_alert_rule(settings, rule);
            let reading = rule.reading();
            let shown = |milli| {
                let value = reading.shown(milli);
                let text = format!("{value:.3}");
                let text = text.trim_end_matches('0').trim_end_matches('.');
                format!("{text}{}", reading.unit())
            };
            format!(
                "AlertRule(rule={}, enabled={}, fires_at={}, clears_at={}, hold_ms={}, default={})",
                rule.token(),
                if setting.enabled { "on" } else { "off" },
                shown(setting.trigger_milli),
                shown(setting.clear_milli),
                setting.hold_ms,
                setting.is_default(),
            )
        })
        .collect()
}

/// Prints every alert rule's setting for the show (Protocol 2.112).
pub fn alert_rules(address: SocketAddr) -> RemoteResult<()> {
    let remote = Remote::connect(address, Role::Viewer)?;
    let state = remote
        .client
        .model()
        .state()
        .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?;
    for line in format_alert_rules(state.alert_rules()) {
        println!("{line}");
    }
    Ok(())
}

/// Changes one alert rule's setting for the show and prints every rule's
/// (Protocol 2.112).
pub fn set_alert_rule(
    address: SocketAddr,
    rule: fm_protocol::HealthAlertRule,
    edit: &crate::args::AlertRuleEdit,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> RemoteResult<()> {
    let mut remote = Remote::connect(address, Role::Admin)?;
    let current = fm_protocol::effective_alert_rule(
        remote
            .client
            .model()
            .state()
            .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?
            .alert_rules(),
        rule,
    );
    let setting = edit.apply(current);
    setting
        .check()
        .map_err(|error| RemoteFailure(format!("invalid alert rule setting: {error}")))?;
    let payload = CommandPayload::SetAlertRule(setting);
    let key = match key {
        Some(key) => key,
        None => implicit_key(&payload)?,
    };
    remote.execute(payload, key, expected_revision, CommandCompletion::Project)?;
    let state = remote
        .client
        .model()
        .state()
        .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?;
    for line in format_alert_rules(state.alert_rules()) {
        println!("{line}");
    }
    Ok(())
}

/// Largest picture `remote-scene-layer-auto-key` asks the daemon for: the
/// size Studio's auto key samples (SW-008).
const AUTO_KEY_PICTURE: (u16, u16) = (
    fm_protocol::MAX_TITLE_PREVIEW_WIDTH,
    fm_protocol::MAX_TITLE_PREVIEW_HEIGHT,
);

/// What `remote-scene-layer-auto-key` reads from the daemon (SW-008): the
/// layer's key now and, when asked for, the picture of the input it shows.
pub(crate) struct LayerKeySource {
    pub(crate) key: Option<fm_protocol::WireLayerKey>,
    pub(crate) picture: Option<fm_protocol::TitlePreviewImage>,
}

/// `width`x`height` scaled to fit within `bound`, keeping its aspect, each
/// side at least 1.
fn fit_within(width: u32, height: u32, bound: (u16, u16)) -> (u16, u16) {
    let (bound_width, bound_height) = (u64::from(bound.0), u64::from(bound.1));
    let (width, height) = (u64::from(width.max(1)), u64::from(height.max(1)));
    // Width-bound when the picture is at least as wide as the bound.
    let (fitted_width, fitted_height) = if width * bound_height >= height * bound_width {
        (
            bound_width.min(width),
            (height * bound_width.min(width)) / width,
        )
    } else {
        (
            (width * bound_height.min(height)) / height,
            bound_height.min(height),
        )
    };
    let side = |value: u64| u16::try_from(value.max(1)).unwrap_or(u16::MAX);
    (side(fitted_width), side(fitted_height))
}

/// Reads layer `layer` of `scene` from the daemon at `address`: its key and,
/// with `picture`, the picture of the input it shows at up to 640x360,
/// which the daemon renders as Studio's designer asks it to (a
/// `scene_preview_request` naming the input, Protocol 2.97).
pub(crate) fn scene_layer_key_source(
    address: SocketAddr,
    scene: fm_types::SceneId,
    layer: fm_protocol::WireSceneLayerId,
    picture: bool,
) -> RemoteResult<LayerKeySource> {
    // A preview needs project-edit authority, which the edit needs too.
    let mut remote = Remote::connect(address, Role::Graphics)?;
    let state = remote
        .client
        .model()
        .state()
        .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?;
    let layout = state
        .scene_layout(scene)
        .and_then(|status| status.layout.clone())
        .ok_or_else(|| RemoteFailure(format!("scene {scene} does not exist")))?;
    let outline = layout
        .layers
        .iter()
        .find(|outline| outline.id == layer)
        .ok_or_else(|| RemoteFailure(format!("scene {scene} has no layer {}", layer.get())))?;
    let key = outline.key;
    if !picture {
        return Ok(LayerKeySource { key, picture: None });
    }
    let fm_protocol::WireSceneLayerSource::Input(input) = outline.source else {
        return Err(RemoteFailure(
            "the layer shows a scene, which has no single input picture; pass --picture".into(),
        )
        .into());
    };
    let (width, height) = fit_within(layout.width, layout.height, AUTO_KEY_PICTURE);
    let request_id = format!("cli-auto-key-{}", unique_client_id()?);
    remote.write(&WireMessage::ScenePreviewRequest(
        fm_protocol::ScenePreviewRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id: request_id.clone(),
            input,
            sample: fm_protocol::TitlePreviewSample::Static,
            width,
            height,
            clip: None,
        },
    ))?;
    let deadline = Instant::now() + REMOTE_IO_TIMEOUT;
    let mut assembler = fm_protocol::TitlePreviewAssembler::new();
    loop {
        match remote.read_until(deadline)? {
            WireMessage::ScenePreviewFrame(part) if part.request_id == request_id => {
                let image = assembler.push(&part).map_err(|error| {
                    RemoteFailure(format!("the input picture is malformed: {error}"))
                })?;
                if let Some(image) = image {
                    return Ok(LayerKeySource {
                        key,
                        picture: Some(image),
                    });
                }
            }
            WireMessage::Error(error) if error.request_id.as_deref() == Some(&request_id) => {
                return Err(protocol_error(&error.error).into());
            }
            message @ (WireMessage::Event(_) | WireMessage::RuntimeEvent(_)) => {
                remote.client.intake(message)?;
            }
            // Status records and other answers are not the picture.
            _ => {}
        }
    }
}

/// Prints the daemon's replay recording, events, and Replay inputs (OR-012,
/// Protocol 2.106).
pub fn replay(address: SocketAddr) -> RemoteResult<()> {
    let mut remote = Remote::connect(address, Role::Viewer)?;
    print_replay(&request_replay_status(&mut remote)?);
    Ok(())
}

/// `settings` with `edit` applied (OR-012); a zero source is the CLI's own
/// argument error, so the local path never reports a remote failure.
pub(crate) fn edited_replay(
    settings: &fm_protocol::ReplaySettings,
    edit: &crate::args::ReplayEdit,
) -> RemoteResult<fm_protocol::ReplaySettings> {
    let sources = match &edit.sources {
        Some(sources) => sources
            .iter()
            .map(|input| {
                core::num::NonZeroU128::new(*input)
                    .map(fm_types::InputId::new)
                    .ok_or_else(|| crate::args::ArgsError::InvalidChoice {
                        field: "replay source",
                        value: input.to_string(),
                    })
            })
            .collect::<Result<_, _>>()?,
        None => settings.sources.clone(),
    };
    Ok(fm_protocol::ReplaySettings {
        sources,
        retention_seconds: edit.retention_seconds.unwrap_or(settings.retention_seconds),
        storage_root: edit
            .storage_root
            .clone()
            .unwrap_or_else(|| settings.storage_root.clone()),
        auto_return: edit.auto_return.unwrap_or(settings.auto_return),
    })
}

/// Edits what the daemon records for replay (OR-012, Protocol 2.106), then
/// prints the status.
pub fn set_replay(
    address: SocketAddr,
    edit: &crate::args::ReplayEdit,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> RemoteResult<()> {
    let current = {
        let mut remote = Remote::connect(address, Role::Viewer)?;
        request_replay_status(&mut remote)?.settings
    };
    let payload = CommandPayload::Replay(fm_protocol::ReplayOperation::Configure(Box::new(
        edited_replay(&current, edit)?,
    )));
    execute(address, payload, key, expected_revision)?;
    replay(address)
}

/// Sends one live replay command (OR-012, Protocol 2.106), then prints the
/// status.
pub fn replay_do(
    address: SocketAddr,
    operation: fm_protocol::ReplayOperation,
    key: Option<String>,
) -> RemoteResult<()> {
    execute(address, CommandPayload::Replay(operation), key, None)?;
    replay(address)
}

/// Asks the daemon for its highlight reel status (OR-013, Protocol 2.107).
fn request_reel_status(remote: &mut Remote) -> RemoteResult<fm_protocol::ReelStatusMessage> {
    let deadline = Instant::now() + REMOTE_IO_TIMEOUT;
    let request_id = format!("cli-reel-{}", unique_client_id()?);
    remote.write(&WireMessage::ReelStatusRequest(
        fm_protocol::ReelStatusRequest {
            request_id: request_id.clone(),
        },
    ))?;
    loop {
        match remote.read_until(deadline)? {
            // The answer echoes this request's id; a pushed record has none
            // and carries the same latest-wins reel state, so taking it
            // avoids waiting out the timeout when a change races the request.
            WireMessage::ReelStatus(status)
                if status.request_id.as_deref() == Some(request_id.as_str())
                    || status.request_id.is_none() =>
            {
                return Ok(*status);
            }
            message @ (WireMessage::Event(_) | WireMessage::RuntimeEvent(_)) => {
                remote.client.intake(message)?;
            }
            WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
            // Another client's answer is not ours; meters, stream status,
            // and snapshots are not the answer either. Keep waiting.
            _ => {}
        }
    }
}

/// Prints the daemon's highlight reels and export jobs (OR-013, Protocol
/// 2.107).
pub fn reel(address: SocketAddr) -> RemoteResult<()> {
    let mut remote = Remote::connect(address, Role::Viewer)?;
    print_reel_status(&request_reel_status(&mut remote)?);
    Ok(())
}

/// Replaces or creates a daemon's highlight reel (OR-013, Protocol 2.107),
/// then prints the status.
pub fn set_reel(
    address: SocketAddr,
    spec: Box<fm_protocol::ReelSpec>,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> RemoteResult<()> {
    execute(
        address,
        CommandPayload::Reel(fm_protocol::ReelOperation::Set(spec)),
        key,
        expected_revision,
    )?;
    reel(address)
}

/// Deletes a daemon's highlight reel (OR-013, Protocol 2.107), then prints
/// the status.
pub fn delete_reel(
    address: SocketAddr,
    id: u64,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> RemoteResult<()> {
    execute(
        address,
        CommandPayload::Reel(fm_protocol::ReelOperation::Delete { reel: id }),
        key,
        expected_revision,
    )?;
    reel(address)
}

/// Sends one live reel export control (OR-013, Protocol 2.107), then prints
/// the status.
pub fn reel_control(
    address: SocketAddr,
    operation: fm_protocol::ReelOperation,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> RemoteResult<()> {
    let payload = CommandPayload::Reel(operation);
    let mut remote = Remote::connect(address, Role::Operator)?;
    let key = match key {
        Some(key) => key,
        None => implicit_key(&payload)?,
    };
    remote.send_command(payload, key, expected_revision)?;
    // The export controls publish no durable event, so the status is read
    // on a fresh Viewer session whose snapshot already carries the accepted
    // revision.
    drop(remote);
    reel(address)
}

/// Starts a daemon's reel export (OR-013, Protocol 2.107), then polls the
/// reel status until the job is terminal and prints one `ReelExport` line.
pub fn export_reel(
    address: SocketAddr,
    job: u64,
    reel: u64,
    destination: String,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> RemoteResult<()> {
    let payload = CommandPayload::Reel(fm_protocol::ReelOperation::Export {
        job,
        reel,
        destination,
    });
    let mut remote = Remote::connect(address, Role::Operator)?;
    let key = match key {
        Some(key) => key,
        None => implicit_key(&payload)?,
    };
    remote.send_command(payload, key, expected_revision)?;
    // The export publishes no durable event; a fresh Viewer session sees
    // the job in the status and any runtime realization without a revision
    // ahead of its snapshot.
    drop(remote);
    let mut remote = Remote::connect(address, Role::Viewer)?;
    let deadline = Instant::now() + REEL_EXPORT_WAIT_LIMIT;
    loop {
        let status = request_reel_status(&mut remote)?;
        if let Some(export) = status.exports.iter().find(|export| export.job == job)
            && matches!(
                export.state,
                fm_protocol::ReelExportState::Completed
                    | fm_protocol::ReelExportState::Failed
                    | fm_protocol::ReelExportState::Cancelled
            )
        {
            print_reel_export(export);
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(RemoteFailure(format!(
                "reel export job {job} did not finish before the deadline"
            ))
            .into());
        }
        std::thread::sleep(REEL_EXPORT_POLL_INTERVAL);
    }
}

/// Prints the reels and export jobs of one status.
pub(crate) fn print_reel_status(status: &fm_protocol::ReelStatusMessage) {
    print_reels(&status.reels, &status.exports);
}

/// Prints the reels and export jobs a status carries.
pub(crate) fn print_reels(
    reels: &[fm_protocol::ReelStatus],
    exports: &[fm_protocol::ReelExportStatus],
) {
    let number =
        |value: Option<u64>| value.map_or_else(|| "none".to_owned(), |value| value.to_string());
    let text =
        |value: Option<&str>| value.map_or_else(|| "none".to_owned(), |value| format!("{value:?}"));
    for reel in reels {
        println!(
            "Reel(id={}, name={:?}, clips={}, total_frames={}, error={})",
            reel.id,
            reel.spec.name,
            reel.spec.clips.len(),
            number(reel.total_frames),
            text(reel.error.as_deref()),
        );
        for clip in &reel.spec.clips {
            println!(
                "ReelClip(reel={}, event={}, angle={}, in={}, out={}, speed={}, dissolve={})",
                reel.id,
                clip.event,
                clip.angle,
                clip.trim_in_frames,
                clip.trim_out_frames,
                clip.speed_percent,
                clip.dissolve_frames,
            );
        }
        if let Some(music) = &reel.spec.music {
            println!(
                "ReelMusic(reel={}, path={:?}, level={}, duck={})",
                reel.id, music.path, music.level_percent, music.duck_percent,
            );
        }
    }
    for export in exports {
        println!(
            "ReelExport(job={}, reel={}, destination={:?}, state={}, frames={}/{}, \
             bytes={}, failure={})",
            export.job,
            export.reel,
            export.destination,
            export.state.token(),
            export.completed_frames,
            number(export.total_frames),
            number(export.bytes),
            text(export.failure.as_deref()),
        );
    }
}

/// Prints one terminal export as the single `ReelExport` line
/// `remote-reel-export` reports.
pub(crate) fn print_reel_export(export: &fm_protocol::ReelExportStatus) {
    let bytes = export
        .bytes
        .map_or_else(|| "none".to_owned(), |bytes| bytes.to_string());
    let failure = export
        .failure
        .as_deref()
        .map_or_else(String::new, |failure| format!(", failure={failure:?}"));
    println!(
        "ReelExport(job={}, state={}, path={:?}, bytes={}{failure})",
        export.job,
        export.state.token(),
        export.destination,
        bytes,
    );
}

/// Prints one `ReplaySettings(...)` line.
pub(crate) fn print_replay_settings(settings: &fm_protocol::ReplaySettings) {
    println!(
        "ReplaySettings(sources=[{}], retention_seconds={}, storage_root={}, auto_return={})",
        settings
            .sources
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(","),
        settings.retention_seconds,
        settings
            .storage_root
            .as_deref()
            .map_or_else(|| "default".to_owned(), |root| format!("{root:?}")),
        settings
            .auto_return
            .map_or_else(|| "off".to_owned(), |frames| frames.to_string()),
    );
}

/// Prints the replay status: the settings, one line per recorded source,
/// event, and Replay input.
pub(crate) fn print_replay(status: &fm_protocol::ReplayStatusMessage) {
    let frame = |frame: Option<u64>| frame.map_or_else(|| "none".to_owned(), |f| f.to_string());
    print_replay_settings(&status.settings);
    println!(
        "Replay(feed={}, root={:?}, live_frame={}, mark_in={}, mark_out={})",
        status.feed.token(),
        status.root,
        frame(status.live_frame),
        frame(status.mark_in),
        frame(status.mark_out),
    );
    for source in &status.recorded {
        println!(
            "ReplaySource(input={}, segments={}, bytes={}, first_frame={}, last_frame={})",
            source.input,
            source.segments,
            source.bytes,
            frame(source.first_frame),
            frame(source.last_frame),
        );
    }
    for event in &status.events {
        println!(
            "ReplayEvent(id={}, name={:?}, in_frame={}, out_frame={}, angles=[{}], complete={})",
            event.id,
            event.name,
            event.in_frame,
            event.out_frame,
            event
                .angles
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
            event.complete,
        );
    }
    for player in &status.players {
        println!(
            "ReplayPlayer(input={}, event={}, angle={}, speed={}, playing={}, frame={})",
            player.input,
            player
                .event
                .map_or_else(|| "none".to_owned(), |event| event.to_string()),
            player
                .angle
                .map_or_else(|| "none".to_owned(), |angle| angle.to_string()),
            player.speed.percent(),
            player.playing,
            frame(player.frame),
        );
    }
}

/// Prints the daemon's `LiveLAN` settings and status (Protocol 2.57).
pub fn live_lan(address: SocketAddr) -> RemoteResult<()> {
    let remote = Remote::connect(address, Role::Viewer)?;
    let state = remote
        .client
        .model()
        .state()
        .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?;
    print_live_lan(state.live_lan(), &state.live_lan_status());
    Ok(())
}

/// Edits the daemon's `LiveLAN` viewer stream (Protocol 2.57): each absent
/// option keeps the daemon's current value, then prints the result.
pub fn set_live_lan(
    address: SocketAddr,
    edit: &crate::args::LiveLanEdit,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> RemoteResult<()> {
    let mut remote = Remote::connect(address, Role::Graphics)?;
    let current = *remote
        .client
        .model()
        .state()
        .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?
        .live_lan();
    let payload = CommandPayload::SetLiveLan {
        config: fm_protocol::LiveLanEntry {
            enabled: edit.enabled.unwrap_or(current.enabled),
            source: edit.source.unwrap_or(current.source),
            max_viewers: edit.max_viewers.unwrap_or(current.max_viewers),
            has_pin: false,
        },
        pin: edit.pin.clone(),
    };
    let key = match key {
        Some(key) => key,
        None => implicit_key(&payload)?,
    };
    remote.execute(payload, key, expected_revision, CommandCompletion::Project)?;
    let state = remote
        .client
        .model()
        .state()
        .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?;
    print_live_lan(state.live_lan(), &state.live_lan_status());
    Ok(())
}

/// Prints one `LiveLan(...)` line: the settings, whether a PIN is set (never
/// the PIN), and the status.
pub(crate) fn print_live_lan(
    entry: &fm_protocol::LiveLanEntry,
    status: &fm_protocol::LiveLanStatus,
) {
    println!(
        "LiveLan(enabled={}, source={}, max_viewers={}, pin={}, state={}, url={}, viewers={}, \
         egress_kbps={})",
        entry.enabled,
        entry.source,
        entry.max_viewers,
        if entry.has_pin { "set" } else { "none" },
        status.state.token(),
        if status.url.is_empty() {
            "none"
        } else {
            status.url.as_str()
        },
        status.viewers,
        status.egress_kbps
    );
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

/// Prints the scripts of the daemon at `address` (RC-011).
pub fn scripts(address: SocketAddr) -> RemoteResult<()> {
    let remote = Remote::connect(address, Role::Viewer)?;
    let state = remote
        .client
        .model()
        .state()
        .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?;
    print_scripts(
        state
            .scripts()
            .iter()
            .map(|script| (script, state.script_run_status(script.id))),
    );
    Ok(())
}

/// Prints one `Script(...)` line per script followed by its source lines,
/// each indented, or `Scripts=none`. A script whose run waits or failed
/// also prints its `ScriptRun(...)` line (Protocol 2.62).
pub(crate) fn print_scripts<'a>(
    scripts: impl IntoIterator<Item = (&'a fm_protocol::ScriptStatus, fm_protocol::ScriptRunStatus)>,
) {
    let mut any = false;
    for (script, run) in scripts {
        any = true;
        let syntax = fm_script::parse(&script.spec.source)
            .err()
            .map_or_else(|| "ok".to_owned(), |failure| failure.to_string());
        println!(
            "Script(id={}, name={:?}, bytes={}, syntax={syntax:?})",
            script.id,
            script.spec.name,
            script.spec.source.len()
        );
        if run != fm_protocol::ScriptRunStatus::Idle {
            print_script_run(script.id, &run);
        }
        for line in script.spec.source.lines() {
            println!("  | {line}");
        }
    }
    if !any {
        println!("Scripts=none");
    }
}

/// Prints an accepted run's report.
pub(crate) fn print_script_ran(script: u64, frame: u64, actions: u32, instructions: u32) {
    println!(
        "ScriptRan(script={script}, frame={frame}, actions={actions}, instructions={instructions})"
    );
}

/// Prints script `script`'s run state (Protocol 2.62):
/// `ScriptRun(script=…, state=idle)`, `state=waiting` with the run id and
/// the line of the wait it is paused at (Protocol 2.69), the frame the next
/// part runs on, and the waits so far, or `state=faulted` with the run id,
/// the failed part's frame, failure kind, position, and (Protocol 2.63)
/// what it was refused with.
pub(crate) fn print_script_run(script: u64, run: &fm_protocol::ScriptRunStatus) {
    match run {
        fm_protocol::ScriptRunStatus::Idle => println!("ScriptRun(script={script}, state=idle)"),
        fm_protocol::ScriptRunStatus::Waiting {
            run,
            started_frame,
            due_frame,
            waits,
            wait_frames,
            line,
        } => println!(
            "ScriptRun(script={script}, run={run}, state=waiting, line={line}, started_frame={started_frame}, due_frame={due_frame}, waits={waits}, wait_frames={wait_frames})"
        ),
        fm_protocol::ScriptRunStatus::Faulted {
            run,
            frame,
            failure,
            line,
            column,
            detail,
        } => println!(
            "ScriptRun(script={script}, run={run}, state=faulted, frame={frame}, failure={}, line={line}, column={column}, {})",
            failure.token(),
            fault_detail(detail)
        ),
    }
}

/// `reason=<token or ->, message="<text>"`: what a deferred part or cue
/// action was refused with (Protocol 2.63).
pub(crate) fn fault_detail(detail: &fm_protocol::FaultDetail) -> String {
    format!(
        "reason={}, message={:?}",
        detail
            .reason
            .as_ref()
            .and_then(fm_protocol::RefusalReason::token)
            .unwrap_or_else(|| "-".to_owned()),
        detail.message
    )
}

/// Edits or runs one script of the daemon at `address` (RC-011); a script
/// named by name is resolved against the daemon's scripts. An accepted run
/// prints `ScriptRan(...)`.
pub fn script(
    address: SocketAddr,
    payload: CommandPayload,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> RemoteResult<()> {
    let completion = CommandCompletion::for_payload(&payload);
    // A run also needs the class of every action the script asks for,
    // which only the daemon knows once it plans the run on its show; a
    // title text needs project-edit authority on top of transition, which
    // only Admin holds.
    let role = match payload {
        CommandPayload::RunScript { .. } => Role::Admin,
        // Stopping a run is a transition-class operator control.
        CommandPayload::StopScript { .. } => Role::Operator,
        _ => Role::Graphics,
    };
    let mut remote = Remote::connect(address, role)?;
    let key = match key {
        Some(key) => key,
        None => implicit_key(&payload)?,
    };
    let run = match payload {
        CommandPayload::RunScript { script } => Some(script),
        _ => None,
    };
    remote.execute(payload, key, expected_revision, completion)?;
    let Some(state) = remote.client.model().state() else {
        return Ok(());
    };
    if let Some(script) = run {
        if let Some(ran) = state.last_script_run() {
            print_script_ran(ran.script, ran.frame, ran.actions, ran.instructions);
        }
        print_script_run(script, &state.script_run_status(script));
    }
    Ok(())
}

/// Lists, exports, or imports the shortcut set of the daemon at `address`
/// (RC-004). An import sends one undoable `shortcut_set` per change, in
/// the plan's order.
pub fn shortcuts(
    address: SocketAddr,
    command: crate::args::ShortcutSetCommand,
) -> RemoteResult<()> {
    use crate::args::ShortcutSetCommand;
    let role = match command {
        ShortcutSetCommand::Import { .. } => Role::Graphics,
        _ => Role::Viewer,
    };
    let mut remote = Remote::connect(address, role)?;
    let state = remote
        .client
        .model()
        .state()
        .ok_or_else(|| RemoteFailure("remote project state is unavailable".into()))?;
    let existing = state.shortcuts().to_vec();
    match command {
        ShortcutSetCommand::List => {
            crate::shortcut_sets::print_shortcuts(&existing);
            Ok(())
        }
        ShortcutSetCommand::Export { file, name } => crate::shortcut_sets::export(
            &existing,
            &file,
            crate::shortcut_sets::set_name(name, state.show_name()),
            "remote-shortcut-export",
        ),
        ShortcutSetCommand::Import { file } => {
            let plan = crate::shortcut_sets::plan(&existing, &file)?;
            for set in &plan.sets {
                let payload = CommandPayload::SetShortcut {
                    shortcut: set.id,
                    spec: Box::new(set.spec.clone()),
                    position: None,
                };
                let key = implicit_key(&payload)?;
                remote.execute(payload, key, None, CommandCompletion::Shortcut)?;
            }
            crate::shortcut_sets::print_import("remote-shortcut-import", &plan);
            Ok(())
        }
    }
}

/// Resolves a script name against the daemon at `address`.
pub fn script_id(address: SocketAddr, name: &str) -> RemoteResult<Option<u64>> {
    let remote = Remote::connect(address, Role::Viewer)?;
    Ok(remote.client.model().state().and_then(|state| {
        state
            .scripts()
            .iter()
            .find(|script| script.spec.name == name)
            .map(|script| script.id)
    }))
}

/// Prints one `CueList(...)` line per cue list followed by one indented
/// `Cue(...)` line per cue, or `CueLists=none`.
pub(crate) fn print_cue_lists<'a>(lists: impl IntoIterator<Item = &'a fm_protocol::CueListStatus>) {
    let optional = |value: Option<u64>| value.map_or_else(|| "-".to_owned(), |id| id.to_string());
    let mut any = false;
    for list in lists {
        any = true;
        let state = &list.state;
        // The inputs the run's (and the armed GO's) dynamic targets are
        // bound to, when they bound any (Protocol 2.89, RC-004).
        let bound = |targets: fm_protocol::WireTargetBindings| {
            let id = |input: Option<fm_protocol::WireInputId>| {
                input.map_or_else(|| "-".to_owned(), |input| input.to_string())
            };
            format!(
                "preview:{},program:{},selected:{}",
                id(targets.preview),
                id(targets.program),
                id(targets.selected)
            )
        };
        println!(
            "CueList(id={}, name={:?}, standby={}, live={}, pending={}, played={:?}, fault={}, armed={}{})",
            list.id,
            list.spec.name,
            optional(state.standby),
            optional(state.live),
            state.pending.map_or_else(
                || "-".to_owned(),
                |pending| format!("{}.{}@{}", pending.cue, pending.action, pending.due_frame)
            ),
            state.played,
            state.fault.as_ref().map_or_else(
                || "-".to_owned(),
                |fault| format!(
                    "{}.{} ({})",
                    fault.cue,
                    fault.action,
                    fault_detail(&fault.detail)
                )
            ),
            state.armed.map_or_else(
                || "-".to_owned(),
                |armed| match armed.targets {
                    None => format!("{}@{}", armed.cue, armed.due_frame),
                    Some(targets) => {
                        format!("{}@{} [{}]", armed.cue, armed.due_frame, bound(targets))
                    }
                }
            ),
            state
                .targets
                .map_or_else(String::new, |targets| format!(", bound={}", bound(targets))),
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
                    sample.failure.map_or_else(|| "none".to_owned(), |failure| failure.to_string()),
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

/// Lists the system audio inputs of the daemon at `address` with their
/// latest status (AU-011).
pub fn system_audio_inputs(address: SocketAddr) -> RemoteResult<()> {
    let remote = Remote::connect(address, Role::Viewer)?;
    let view = remote
        .client
        .model()
        .view()
        .ok_or_else(|| RemoteFailure("remote project view is unavailable".into()))?;
    print_system_audio_inputs(view.system_audio_inputs.iter().cloned());
    Ok(())
}

/// Lists the screen capture inputs of the daemon at `address` with their
/// latest status (IN-011).
pub fn screen_capture_inputs(address: SocketAddr) -> RemoteResult<()> {
    let remote = Remote::connect(address, Role::Viewer)?;
    let view = remote
        .client
        .model()
        .view()
        .ok_or_else(|| RemoteFailure("remote project view is unavailable".into()))?;
    print_screen_capture_inputs(view.screen_capture_inputs.iter().cloned());
    Ok(())
}

/// The target of a screen capture configuration, as the CLI prints it.
fn screen_capture_target(config: &fm_protocol::ScreenCaptureConfig) -> String {
    match &config.target {
        fm_protocol::ScreenCaptureTarget::Screen { index } => format!("screen:{index}"),
        fm_protocol::ScreenCaptureTarget::Window(rule) => format!(
            "window(app={:?}, title={:?}, match={}, id={})",
            rule.app,
            rule.title,
            rule.title_match.token(),
            rule.window_id
                .map_or_else(|| "none".to_owned(), |id| id.to_string())
        ),
    }
}

/// Prints one `ScreenCaptureInput(...)` line per screen capture input, or
/// `ScreenCaptureInputs=none`.
pub(crate) fn print_screen_capture_inputs(
    entries: impl IntoIterator<Item = fm_protocol::ScreenCaptureEntry>,
) {
    let mut any = false;
    for entry in entries {
        any = true;
        let crop = entry.config.crop;
        println!(
            "ScreenCaptureInput(input={}, target={}, crop={},{},{},{}, cursor={}, fps={}, stall={}, audio={}, state={}, size={}x{}, reopens={}, last_error={}, audio_state={}, audio_error={})",
            entry.input,
            screen_capture_target(&entry.config),
            crop.left,
            crop.top,
            crop.right,
            crop.bottom,
            if entry.config.cursor { "on" } else { "off" },
            entry.config.fps,
            entry.config.slate.token(),
            entry.config.audio.token(),
            entry.status.state.token(),
            entry.status.width,
            entry.status.height,
            entry.status.reopens,
            quoted_or_none(entry.status.last_error.as_deref()),
            entry.status.audio.token(),
            quoted_or_none(entry.status.audio_error.as_deref()),
        );
    }
    if !any {
        println!("ScreenCaptureInputs=none");
    }
}

/// Lists the screens and windows the daemon's host can capture (IN-011).
pub fn screen_capture_targets(address: SocketAddr) -> RemoteResult<()> {
    // The daemon lists its devices and desktop only to a session that
    // may edit the project, which Graphics holds.
    let mut remote = Remote::connect(address, Role::Graphics)?;
    let deadline = Instant::now() + REMOTE_IO_TIMEOUT;
    let request_id = format!("cli-screen-capture-{}", unique_client_id()?);
    remote.write(&WireMessage::ScreenCaptureTargetsRequest(
        fm_protocol::ScreenCaptureTargetsRequest {
            request_id: request_id.clone(),
        },
    ))?;
    let reply = loop {
        match remote.read_until(deadline)? {
            WireMessage::ScreenCaptureTargets(reply) if reply.request_id == request_id => {
                break reply;
            }
            message @ (WireMessage::Event(_) | WireMessage::RuntimeEvent(_)) => {
                remote.client.intake(message)?;
            }
            WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
            _ => {
                return Err(
                    RemoteFailure("expected screen_capture_targets from daemon".into()).into(),
                );
            }
        }
    };
    print_screen_capture_targets(&reply.inventory);
    Ok(())
}

/// Asks the daemon's desktop to pick screen capture input `input`'s screen
/// or window again (IN-011, Protocol 2.85) and prints
/// `ScreenCapturePick(input=<id>)` once the daemon took it: nothing answers
/// an accepted pick, so a target list request sent after it marks the
/// point where a refusal would have arrived.
pub fn screen_capture_pick(address: SocketAddr, input: fm_types::InputId) -> RemoteResult<()> {
    // A pick needs project-edit authority, which Graphics holds.
    let mut remote = Remote::connect(address, Role::Graphics)?;
    let deadline = Instant::now() + REMOTE_IO_TIMEOUT;
    let unique = unique_client_id()?;
    let request_id = format!("cli-screen-capture-pick-{unique}");
    let marker = format!("cli-screen-capture-pick-marker-{unique}");
    remote.write(&WireMessage::ScreenCapturePickRequest(
        fm_protocol::ScreenCapturePickRequest {
            request_id: request_id.clone(),
            input: fm_protocol::WireInputId::from_domain(input),
        },
    ))?;
    remote.write(&WireMessage::ScreenCaptureTargetsRequest(
        fm_protocol::ScreenCaptureTargetsRequest {
            request_id: marker.clone(),
        },
    ))?;
    loop {
        match remote.read_until(deadline)? {
            WireMessage::ScreenCaptureTargets(reply) if reply.request_id == marker => break,
            message @ (WireMessage::Event(_) | WireMessage::RuntimeEvent(_)) => {
                remote.client.intake(message)?;
            }
            WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
            _ => {
                return Err(RemoteFailure(
                    "expected screen_capture_targets from daemon after the pick".into(),
                )
                .into());
            }
        }
    }
    println!("ScreenCapturePick(input={input})");
    Ok(())
}

/// Prints a `ScreenCaptureTargets(...)` header, then a `Screen(...)` line
/// per screen and a `Window(...)` line per window.
fn print_screen_capture_targets(inventory: &fm_protocol::ScreenCaptureInventory) {
    println!(
        "ScreenCaptureTargets(supported={}, backend={}, screens={}, windows={}, error={})",
        if inventory.supported { "on" } else { "off" },
        if inventory.backend.is_empty() {
            "none"
        } else {
            inventory.backend.as_str()
        },
        inventory.screens.len(),
        inventory.windows.len(),
        quoted_or_none(inventory.error.as_deref()),
    );
    for screen in &inventory.screens {
        println!(
            "Screen(index={}, name={:?}, primary={}, position={},{}, size={}x{})",
            screen.index,
            screen.name,
            if screen.primary { "on" } else { "off" },
            screen.x,
            screen.y,
            screen.width,
            screen.height,
        );
    }
    for window in &inventory.windows {
        println!(
            "Window(id={}, app={:?}, title={:?}, pid={}, position={},{}, size={}x{})",
            window.id,
            window.app,
            window.title,
            window
                .pid
                .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string()),
            window.x,
            window.y,
            window.width,
            window.height,
        );
    }
}

/// Prints one `SystemAudioInput(...)` line per system audio input, or
/// `SystemAudioInputs=none`.
pub(crate) fn print_system_audio_inputs(
    entries: impl IntoIterator<Item = fm_protocol::SystemAudioEntry>,
) {
    let mut any = false;
    for entry in entries {
        any = true;
        let (attempt, next_in_ms) = match entry.status.state {
            fm_protocol::SystemAudioState::Reopening {
                attempt,
                next_in_ms,
            } => (attempt, next_in_ms),
            _ => (0, 0),
        };
        println!(
            "SystemAudioInput(input={}, backend={}, device={:?}, state={}, attempt={attempt}, next_in_ms={next_in_ms}, reopens={}, last_error={})",
            entry.input,
            entry.config.backend.token(),
            entry.config.device,
            entry.status.state.token(),
            entry.status.reopens,
            quoted_or_none(entry.status.last_error.as_deref()),
        );
    }
    if !any {
        println!("SystemAudioInputs=none");
    }
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
    // The daemon lists its devices and desktop only to a session that
    // may edit the project, which Graphics holds.
    let mut remote = Remote::connect(address, Role::Graphics)?;
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

/// Lists the system audio capture devices of the daemon's host (AU-011,
/// Protocol 2.68).
pub fn system_audio_devices(address: SocketAddr) -> RemoteResult<()> {
    // The daemon lists its devices and desktop only to a session that
    // may edit the project, which Graphics holds.
    let mut remote = Remote::connect(address, Role::Graphics)?;
    let deadline = Instant::now() + REMOTE_IO_TIMEOUT;
    let request_id = format!("cli-system-audio-{}", unique_client_id()?);
    remote.write(&WireMessage::SystemAudioDevicesRequest(
        fm_protocol::SystemAudioDevicesRequest {
            request_id: request_id.clone(),
        },
    ))?;
    let reply = loop {
        match remote.read_until(deadline)? {
            WireMessage::SystemAudioDevices(reply) if reply.request_id == request_id => {
                break reply;
            }
            message @ (WireMessage::Event(_) | WireMessage::RuntimeEvent(_)) => {
                remote.client.intake(message)?;
            }
            WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
            _ => {
                return Err(
                    RemoteFailure("expected system_audio_devices from daemon".into()).into(),
                );
            }
        }
    };
    print_system_audio_devices(&reply.inventory);
    Ok(())
}

/// Lists the headphone output devices of the daemon's host (AU-001,
/// Protocol 2.102).
pub fn audio_output_devices(address: SocketAddr) -> RemoteResult<()> {
    // Like every device inventory, only a session that may edit the
    // project is answered, which Graphics holds.
    let mut remote = Remote::connect(address, Role::Graphics)?;
    let deadline = Instant::now() + REMOTE_IO_TIMEOUT;
    let request_id = format!("cli-audio-output-{}", unique_client_id()?);
    remote.write(&WireMessage::AudioOutputDevicesRequest(
        fm_protocol::AudioOutputDevicesRequest {
            request_id: request_id.clone(),
        },
    ))?;
    let reply = loop {
        match remote.read_until(deadline)? {
            WireMessage::AudioOutputDevices(reply) if reply.request_id == request_id => {
                break reply;
            }
            message @ (WireMessage::Event(_) | WireMessage::RuntimeEvent(_)) => {
                remote.client.intake(message)?;
            }
            WireMessage::Error(error) => return Err(protocol_error(&error.error).into()),
            _ => {
                return Err(
                    RemoteFailure("expected audio_output_devices from daemon".into()).into(),
                );
            }
        }
    };
    let inventory = &reply.inventory;
    println!(
        "AudioOutputDevices(supported={}, devices={}, error={})",
        if inventory.supported { "on" } else { "off" },
        inventory.devices.len(),
        quoted_or_none(inventory.error.as_deref())
    );
    for device in &inventory.devices {
        println!(
            "Output(device={:?}, description={:?}, channels={}, default={})",
            device.device,
            device.description,
            device
                .channels
                .map_or_else(|| "unknown".to_owned(), |channels| channels.to_string()),
            if device.default { "on" } else { "off" },
        );
    }
    Ok(())
}

/// Prints a `SystemAudioDevices(...)` header, an `Unlisted(...)` line per
/// audio system that could not be listed, and a `Device(...)` line per
/// device.
fn print_system_audio_devices(inventory: &fm_protocol::SystemAudioInventory) {
    println!(
        "SystemAudioDevices(supported={}, devices={})",
        if inventory.supported { "on" } else { "off" },
        inventory.devices.len()
    );
    for error in &inventory.errors {
        println!(
            "Unlisted(backend={}, error={:?})",
            error.backend.token(),
            error.error
        );
    }
    let flag = |on: bool| if on { "on" } else { "off" };
    for device in &inventory.devices {
        println!(
            "Device(backend={}, device={:?}, description={:?}, channels={}, default={}, \
             monitor={})",
            device.backend.token(),
            device.device,
            device.description,
            device
                .channels
                .map_or_else(|| "unknown".to_owned(), |channels| channels.to_string()),
            flag(device.default),
            flag(device.monitor),
        );
    }
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
    // The daemon lists its devices and desktop only to a session that
    // may edit the project, which Graphics holds.
    let mut remote = Remote::connect(address, Role::Graphics)?;
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
    for sample in &response.source_sync {
        println!("{}", format_source_sync(sample));
    }
    for line in format_health(&response.health) {
        println!("{line}");
    }
}

/// The daemon's health (RC-015): one `Health` line with the utilization,
/// then one `Alert` line per firing alert and one `AlertNotification` line
/// per recent notification.
pub(crate) fn format_health(health: &fm_protocol::DaemonHealth) -> Vec<String> {
    let mut lines = vec![format!(
        "Health(uptime_ms={}, cpu={}, gpu={}, alerts={}, notifications={})",
        health.uptime_ms,
        utilization(health.cpu_permille),
        utilization(health.gpu_permille),
        health.alerts.len(),
        health.notifications.len(),
    )];
    lines.extend(health.alerts.iter().map(|alert| {
        let acknowledged = alert
            .acknowledged_until_ms
            .map(|until| format!(", acknowledged_until_ms={until}"))
            .unwrap_or_default();
        format!(
            "Alert(rule={}, severity={}, since_ms={}, value={}{acknowledged})",
            alert.rule.token(),
            alert.severity.token(),
            alert.since_ms,
            milli_value(alert.value_milli),
        )
    }));
    lines.extend(health.silences.iter().map(|silence| {
        format!(
            "AlertSilence(rule={}, until_ms={})",
            silence.rule.token(),
            silence.until_ms
        )
    }));
    lines.extend(health.notifications.iter().map(|notification| {
        format!(
            "AlertNotification(sequence={}, rule={}, severity={}, kind={}, at_ms={}, value={})",
            notification.sequence,
            notification.rule.token(),
            notification.severity.token(),
            notification.kind.token(),
            notification.at_ms,
            milli_value(notification.value_milli),
        )
    }));
    lines
}

/// Permille as a percentage with one decimal, or `unavailable`.
fn utilization(permille: Option<u16>) -> String {
    permille.map_or_else(
        || "unavailable".to_owned(),
        |permille| format!("{}.{}%", permille / 10, permille % 10),
    )
}

/// An exact reading times 1000 as a decimal: `-0.005`, `1.000`, or `none`.
fn milli_value(value: Option<i64>) -> String {
    value.map_or_else(
        || "none".to_owned(),
        |value| {
            let sign = if value < 0 { "-" } else { "" };
            let magnitude = value.unsigned_abs();
            format!("{sign}{}.{:03}", magnitude / 1_000, magnitude % 1_000)
        },
    )
}

/// One live source's drift loop (AU-004) on one line.
pub(crate) fn format_source_sync(sample: &fm_protocol::SourceSyncSample) -> String {
    format!(
        "SourceSync(input={}, kind={}, state={}, drift_ppm={}, correction_ppm={}, ratio={:.8}, rate_hz={}/{}, buffer_ms={:.3}, target_ms={:.3}, resyncs={}, underruns={}, overruns={}, picture_drops={}, picture_repeats={}, restarts={})",
        sample.input,
        sample.kind.token(),
        sample.state.token(),
        milli_ppm(sample.drift_mppm),
        milli_ppm(sample.correction_mppm),
        sample.resampling_ratio(),
        sample.input_rate_hz,
        sample.output_rate_hz,
        sample.buffer_ms(),
        sample.target_ms(),
        sample.resyncs,
        sample.underruns,
        sample.overruns,
        sample.picture_drops,
        sample.picture_repeats,
        sample.restarts,
    )
}

/// Exact signed milli-ppm as ppm: `-12.345`, `+0.000`.
fn milli_ppm(value: i64) -> String {
    let sign = if value < 0 { '-' } else { '+' };
    let magnitude = value.unsigned_abs();
    format!("{sign}{}.{:03}", magnitude / 1_000, magnitude % 1_000)
}

/// One media input's settings and transport on one line (IN-003).
/// One paged input's replicated status (IN-014), its page one-based.
fn format_paged_status(status: &fm_protocol::PagedStatus) -> String {
    use std::fmt::Write as _;
    let count = if status.count == 0 {
        "unknown".to_owned()
    } else {
        status.count.to_string()
    };
    let mut line = format!(
        "input-page: input={} kind={} page={} count={count} state={}",
        status.input.to_domain(),
        status.kind.token(),
        status.page.saturating_add(1),
        status.state.token()
    );
    if let Some(title) = &status.title {
        let _ = write!(line, " title={title:?}");
    }
    if let Some(message) = &status.message {
        let _ = write!(line, " message={message:?}");
    }
    line
}

/// A played duration in seconds with millisecond precision.
fn format_seconds(micros: u64) -> String {
    format!("{}.{:03}s", micros / 1_000_000, micros % 1_000_000 / 1_000)
}

/// One media list (IN-006): its options and transition (Protocol 2.95),
/// the total it plays through its marks (`>=` while some lengths are
/// unknown), and each item `id:"name" in..out file_frames@rate played`,
/// the active item starred; a length the authority has not probed is `?`.
pub(crate) fn format_media_list(status: &fm_protocol::MediaListStatus) -> String {
    let options = &status.options;
    let (total, complete) = status.total_micros();
    let items = status
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let length = status.lengths.get(index).copied().flatten();
            format!(
                "{}{}:{:?} {}..{} {} {}",
                item.id,
                if item.id == status.active { "*" } else { "" },
                item.title.as_deref().unwrap_or(&item.uri),
                item.mark_in,
                item.mark_out
                    .map_or_else(|| "end".to_owned(), |out| out.to_string()),
                length.map_or_else(
                    || "?".to_owned(),
                    |length| format!(
                        "{}@{}/{}",
                        length.frames,
                        length.rate.numerator(),
                        length.rate.denominator()
                    )
                ),
                length.map_or_else(
                    || "?".to_owned(),
                    |length| format_seconds(length.played_micros(item.mark_in, item.mark_out))
                ),
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "MediaList(input={}, active={}, position={}/{}, cycle={}, auto_next={}, auto_first={}, loop={}, shuffle={}, transition={}/{}, total{}{}, items=[{}])",
        status.input,
        status.active,
        u64::from(status.position) + 1,
        status.items.len(),
        status.cycle,
        options.auto_next,
        options.auto_first,
        options.loop_list,
        options
            .shuffle_seed
            .map_or_else(|| "off".to_owned(), |seed| seed.to_string()),
        options.transition.kind.token(),
        options.transition.duration_frames,
        if complete { "=" } else { ">=" },
        format_seconds(total),
        items
    )
}

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
    // A GO also needs the class of every action of the chain it fires
    // (RC-017), which may include a title text only Admin may also edit;
    // a script run (RC-011) likewise. The other cue controls fire nothing.
    let fires = matches!(
        payload,
        CommandPayload::CueControl {
            operation: fm_protocol::CueOperation::Go { .. } | fm_protocol::CueOperation::Arm { .. },
            ..
        } | CommandPayload::RunScript { .. }
    );
    let role = match completion {
        _ if fires => Role::Admin,
        CommandCompletion::Project
        | CommandCompletion::Shortcut
        | CommandCompletion::Padlock
        | CommandCompletion::Playlist { edit: true }
        | CommandCompletion::Cue { edit: true }
        | CommandCompletion::Script { run: false }
        | CommandCompletion::Media { transport: false }
        | CommandCompletion::Replay { live: false }
        | CommandCompletion::Reel { live: false } => Role::Graphics,
        CommandCompletion::Audio | CommandCompletion::Mixer { .. } => Role::Audio,
        CommandCompletion::FadeToBlackOutput
        | CommandCompletion::Switcher
        | CommandCompletion::Favorite
        | CommandCompletion::Playlist { edit: false }
        | CommandCompletion::Cue { edit: false }
        | CommandCompletion::Script { run: true }
        | CommandCompletion::Media { transport: true }
        | CommandCompletion::Replay { live: true }
        | CommandCompletion::Reel { live: true }
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
    /// A script edit (`project`) or run or stop (`switcher`): completes on
    /// `script_changed`/`script_removed`, or `script_ran` or
    /// `script_run_changed` (Protocol 2.62), and that domain's realization
    /// (Protocol 2.58).
    Script {
        run: bool,
    },
    /// A shortcut edit (RC-004): completes on `shortcut_changed` or
    /// `shortcut_removed` and switcher realization.
    Shortcut,
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
    /// A replay command (OR-012): completes on `replay_changed` and
    /// project (settings) or replay (live operations) realization.
    Replay {
        live: bool,
    },
    /// A reel edit (`project`) or export (`reel`) (OR-013): completes on
    /// `reel_changed`/`reel_removed` and that domain's realization.
    Reel {
        live: bool,
    },
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
            | CommandPayload::SetLiveLan { .. }
            | CommandPayload::SetAlertRule(_)
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
            CommandPayload::Replay(operation) => Self::Replay {
                live: operation.is_live(),
            },
            CommandPayload::Reel(operation) => Self::Reel {
                live: operation.is_live(),
            },
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
            CommandPayload::SetScript { .. } | CommandPayload::RemoveScript { .. } => {
                Self::Script { run: false }
            }
            CommandPayload::RunScript { .. } | CommandPayload::StopScript { .. } => {
                Self::Script { run: true }
            }
            CommandPayload::SetShortcut { .. } | CommandPayload::RemoveShortcut { .. } => {
                Self::Shortcut
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
            | Self::Script { run: false }
            | Self::Mixer { live: false }
            | Self::Replay { live: false }
            | Self::Reel { live: false }
            | Self::FadeToBlackOutput => domain == "project",
            Self::Replay { live: true } => domain == "replay",
            Self::Reel { live: true } => domain == "reel",
            Self::Audio | Self::Mixer { live: true } => domain == "audio",
            Self::Media { .. } => domain == "media",
            Self::Switcher
            | Self::TitleTake { .. }
            | Self::Playlist { edit: false }
            | Self::Cue { edit: false }
            | Self::Script { run: true }
            // The authority realizes shortcut edits with the switcher.
            | Self::Shortcut => domain == "switcher",
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
                | WireMessage::CueListState(_)
                | WireMessage::ScriptState(_)
                | WireMessage::ShortcutState(_)
                | WireMessage::StillState(_)
                | WireMessage::MediaListState(_)
                | WireMessage::AudioStripDspState(_)
                | WireMessage::PagedState(_)) => {
                    client.intake(message)?;
                }
                // Lossy status records may interleave with the snapshot's
                // state; they are read on demand, not part of the model.
                WireMessage::ReelStatus(_) | WireMessage::ReplayStatus(_) => {}
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
        let revision = self.send_command(payload, key, expected_revision)?;
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

    /// Sends one command and waits for its result, without waiting for the
    /// durable event and realization that follow it. Live reel export
    /// control (OR-013) has no durable event: its state is the lossy
    /// `reel_status` record.
    fn send_command(
        &mut self,
        payload: CommandPayload,
        key: String,
        expected_revision: Option<u64>,
    ) -> RemoteResult<u64> {
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
                // The daemon's own status records are lossy and latest-wins;
                // a command result may follow one on a subscribed session.
                WireMessage::ReelStatus(_) | WireMessage::ReplayStatus(_) => {}
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
            CommandResult::Accepted { revision, .. } => Ok(revision),
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
        let waiting = Instant::now();
        let mut heartbeats = 0;
        while !durable || !realized {
            match self.read_awaiting_realization(waiting, &mut heartbeats)? {
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
                        | EventPayload::TitleExposureChanged { .. }
                        | EventPayload::SceneAnimationChanged { .. }
                        | EventPayload::SceneLayoutChanged { .. }
                        | EventPayload::DisplayOutputChanged { .. }
                        | EventPayload::LiveLanChanged(_)
                        | EventPayload::AlertRulesChanged(_)
                        | EventPayload::NetworkStreamChanged { .. }
                        | EventPayload::CaptureSettingsChanged { .. }
                        | EventPayload::StreamTargetChanged { .. }
                        | EventPayload::ShortcutChanged(_)
                        | EventPayload::ShortcutRemoved { .. } => matches!(
                            completion,
                            CommandCompletion::Project
                                | CommandCompletion::History
                                | CommandCompletion::Shortcut
                        ),
                        // The CLI never fires a shortcut.
                        EventPayload::ShortcutFired(_) => false,
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
                                | CommandCompletion::Script { .. }
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
                        EventPayload::ScriptChanged(_) | EventPayload::ScriptRemoved { .. } => {
                            matches!(
                                completion,
                                CommandCompletion::Script { run: false }
                                    | CommandCompletion::History
                            )
                        }
                        EventPayload::ScriptRan(_)
                        | EventPayload::ScriptRunChanged { .. }
                        | EventPayload::ScriptRunsStopped { .. } => {
                            completion == CommandCompletion::Script { run: true }
                        }
                        EventPayload::TransitionFavoriteChanged { .. } => matches!(
                            completion,
                            CommandCompletion::Favorite | CommandCompletion::History
                        ),
                        EventPayload::MediaChanged { .. } => {
                            matches!(completion, CommandCompletion::Media { .. })
                        }
                        EventPayload::ReplayChanged { .. } => matches!(
                            completion,
                            CommandCompletion::Replay { .. } | CommandCompletion::History
                        ),
                        EventPayload::ReelChanged { .. } | EventPayload::ReelRemoved { .. } => {
                            matches!(
                                completion,
                                CommandCompletion::Reel { .. } | CommandCompletion::History
                            )
                        }
                        // A slideshow step answers `page_navigated`, a
                        // document step `paged_changed` (IN-014).
                        EventPayload::PageNavigated { .. } | EventPayload::PagedChanged { .. } => {
                            completion == CommandCompletion::Switcher
                        }
                        EventPayload::MediaListChanged { operation, .. } => {
                            if matches!(*operation, "navigate" | "advance") {
                                matches!(completion, CommandCompletion::Media { .. })
                            } else {
                                matches!(
                                    completion,
                                    CommandCompletion::Project | CommandCompletion::History
                                )
                            }
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
                    // Each script step the cue operation ran (Protocol 2.61).
                    if matches_revision && let EventPayload::CueRunChanged(change) = &event.payload
                    {
                        for ran in &change.scripts {
                            print_script_ran(
                                ran.script,
                                change.frame,
                                ran.actions,
                                ran.instructions,
                            );
                        }
                    }
                    self.client.intake(WireMessage::Event(event))?;
                    durable |= matches_revision;
                }
                WireMessage::RuntimeEvent(runtime) => {
                    // A command another one interrupts mid motion (an
                    // overlay transition, a title phase, Fade to Black) is
                    // superseded instead of realized (Protocol 2.86).
                    let matches_realization = runtime.revision == revision
                        && match &runtime.event {
                            RuntimeLifecycleEvent::Realized { domain, .. } => {
                                completion.realizes(domain)
                            }
                            RuntimeLifecycleEvent::Superseded { .. } => true,
                            _ => false,
                        };
                    self.client.intake(WireMessage::RuntimeEvent(runtime))?;
                    realized |= matches_realization;
                }
                // Lossy status records can interleave with the command's
                // events; they are not part of its durable/realized outcome.
                WireMessage::ReelStatus(_) | WireMessage::ReplayStatus(_) => {}
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
        for status in state.media_lists().values() {
            println!("{}", format_media_list(status));
        }
        for status in state.paged_statuses() {
            println!("{}", format_paged_status(status));
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
                    sample
                        .failure
                        .map_or_else(|| "none".to_owned(), |failure| failure.to_string()),
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

    /// The next message of an accepted command's events. A native daemon
    /// answers a command once its first frame is realized and realizes the
    /// rest of a transition or title phase on its frame loop (Protocol
    /// 2.86), so the realization can follow the result by the whole
    /// transition. While nothing arrives the wait heartbeats every
    /// [`REALIZATION_HEARTBEAT_EVERY`] (`heartbeats` sent so far) and keeps
    /// waiting as long as the daemon acknowledges them, up to
    /// [`REALIZATION_WAIT_LIMIT`] after `waiting`; a daemon silent for
    /// [`REMOTE_IO_TIMEOUT`] times out as before. Acknowledgements are not
    /// returned.
    fn read_awaiting_realization(
        &mut self,
        waiting: Instant,
        heartbeats: &mut u64,
    ) -> RemoteResult<WireMessage> {
        let mut heard = Instant::now();
        loop {
            if self.readable_within(REALIZATION_HEARTBEAT_EVERY)? {
                self.reader
                    .get_mut()
                    .set_read_timeout(Some(REMOTE_IO_TIMEOUT))?;
                match self.read()? {
                    WireMessage::HeartbeatAcknowledgement(_) => {
                        heard = Instant::now();
                        continue;
                    }
                    message => return Ok(message),
                }
            }
            if heard.elapsed() >= REMOTE_IO_TIMEOUT || waiting.elapsed() >= REALIZATION_WAIT_LIMIT {
                return Err(RemoteFailure("daemon response timed out".into()).into());
            }
            *heartbeats += 1;
            let sent_at_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| {
                    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
                });
            self.write(&WireMessage::Heartbeat(HeartbeatMessage {
                server: self.server.clone(),
                sequence: *heartbeats,
                sent_at_ms,
                last_applied: None,
            }))?;
        }
    }

    /// Whether the daemon sent anything within `timeout`, without
    /// consuming it (or a partial line of it).
    fn readable_within(&mut self, timeout: Duration) -> RemoteResult<bool> {
        if !self.reader.buffer().is_empty() {
            return Ok(true);
        }
        self.reader.get_mut().set_read_timeout(Some(timeout))?;
        match self.reader.fill_buf() {
            // An empty buffer is the end of the stream, which the read
            // that follows reports.
            Ok(_) => Ok(true),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error.into()),
        }
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

#[cfg(test)]
mod media_list_tests {
    use std::num::NonZeroU128;

    use fm_protocol::{
        MediaListItemLength, MediaListItemSpec, MediaListOptionsSpec, MediaListStatus,
        MediaListTransitionKind, MediaListTransitionSpec, WireInputId,
    };
    use fm_types::FrameRate;

    #[test]
    fn a_media_list_prints_its_transition_and_every_items_length() {
        let item = |id: u32, uri: &str, mark_in: u64, mark_out: Option<u64>| MediaListItemSpec {
            id,
            uri: uri.into(),
            title: None,
            mark_in,
            mark_out,
        };
        let mut status = MediaListStatus {
            input: WireInputId::new(NonZeroU128::new(7).unwrap()),
            items: vec![
                item(1, "a.mkv", 10, Some(40)),
                item(2, "b.mkv", 0, None),
                item(3, "c.mkv", 0, None),
            ],
            options: MediaListOptionsSpec {
                auto_next: true,
                transition: MediaListTransitionSpec {
                    kind: MediaListTransitionKind::Dip,
                    duration_frames: 12,
                },
                ..MediaListOptionsSpec::default()
            },
            active: 2,
            cycle: 0,
            order: vec![1, 2, 3],
            position: 1,
            lengths: vec![
                Some(MediaListItemLength {
                    frames: 300,
                    rate: FrameRate::new(30, 1).unwrap(),
                }),
                Some(MediaListItemLength {
                    frames: 250,
                    rate: FrameRate::new(25, 1).unwrap(),
                }),
                None,
            ],
        };
        assert_eq!(
            super::format_media_list(&status),
            "MediaList(input=7, active=2, position=2/3, cycle=0, auto_next=true, auto_first=false, loop=false, shuffle=off, transition=dip/12, total>=11.000s, items=[1:\"a.mkv\" 10..40 300@30/1 1.000s, 2*:\"b.mkv\" 0..end 250@25/1 10.000s, 3:\"c.mkv\" 0..end ? ?])"
        );
        status.lengths[2] = status.lengths[1];
        assert!(super::format_media_list(&status).contains("total=21.000s"));
    }
}
