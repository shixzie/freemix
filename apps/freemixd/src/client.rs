//! The daemon's client path: the test harness's client loop, session
//! handshakes and heartbeats, command admission, and the native group
//! realization hooks those commands use.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(unused_imports, clippy::wildcard_imports)]
// the binary items, some of them native only
use super::*;

#[cfg(test)]
pub(crate) fn configure_client_socket(stream: &TcpStream) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(CLIENT_READ_POLL_INTERVAL))?;
    stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn handle_client(
    stream: TcpStream,
    server: &Server<ControlHandle>,
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    authority: &ServerIdentity,
    native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
    once_client_outcome: &mut OnceClientOutcome,
) -> AppResult<()> {
    let handshake_deadline = Instant::now()
        .checked_add(Duration::from_millis(
            server.config().session_limits.heartbeat_timeout_ms,
        ))
        .ok_or_else(|| AppFailure("handshake deadline exceeds Instant range".into()))?;
    configure_client_socket(&stream)?;
    let mut writer = stream.try_clone()?;
    let mut reader = MessageReader::new(stream);
    let mut native = native;
    let first = match read_client_message(
        &mut reader,
        control,
        authority,
        &mut native,
        process_shutdown,
        || Ok(Instant::now() < handshake_deadline),
    )? {
        ClientRead::Message(message) => message,
        ClientRead::Closed | ClientRead::DaemonShutdown(_) => return Ok(()),
    };
    let (hello, handshake_outcome) = if let WireMessage::HandshakeRequest(request) = first {
        current_handshake(&request, control, durable.project().id())
    } else {
        write_error(
            &mut writer,
            "handshake_required",
            "first message must be handshake_request",
        )?;
        return Ok(());
    };

    let handshake = match server.handshake(&hello, principal, now_millis()?) {
        Ok(handshake) => handshake,
        Err(error) => {
            write_message(
                &mut writer,
                &WireMessage::HandshakeResponse(rejected_handshake_response(
                    server,
                    control,
                    durable.project().id(),
                    &hello,
                    handshake_code(&error),
                    &error.to_string(),
                )),
            )?;
            *once_client_outcome = OnceClientOutcome::HandshakeResponseWritten;
            return Ok(());
        }
    };
    let mut session = handshake.session;
    let session_identity = server_identity(&handshake.server_hello, durable.project().id());
    let response = WireMessage::HandshakeResponse(handshake_response(
        &handshake.server_hello,
        session_identity.clone(),
        reconciled_handshake_outcome(handshake_outcome, &handshake.sync),
    ));
    write_session_message(&mut writer, &mut session, &response)?;
    *once_client_outcome = OnceClientOutcome::HandshakeResponseWritten;
    match handshake.sync {
        SyncPayload::Snapshot(snapshot) => {
            write_session_message(&mut writer, &mut session, &WireMessage::Snapshot(*snapshot))?;
            for state in control.borrow().title_states() {
                write_session_message(&mut writer, &mut session, &WireMessage::TitleState(state))?;
            }
            let playlist = control.borrow().playlist_state();
            if let Some(state) = playlist {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::PlaylistState(state),
                )?;
            }
            for state in control.borrow().grade_states() {
                write_session_message(&mut writer, &mut session, &WireMessage::GradeState(state))?;
            }
            for state in control.borrow().title_animation_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::TitleAnimationState(state),
                )?;
            }
            for state in control.borrow().title_layout_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::TitleLayoutState(state),
                )?;
            }
            for state in control.borrow().scene_animation_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::SceneAnimationState(state),
                )?;
            }
            for state in control.borrow().scene_layout_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::SceneLayoutState(state),
                )?;
            }
            for state in control.borrow().animation_preset_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::AnimationPresetState(state),
                )?;
            }
            for state in control.borrow().media_states() {
                write_session_message(&mut writer, &mut session, &WireMessage::MediaState(state))?;
            }
            for state in control.borrow().cue_list_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::CueListState(state),
                )?;
            }
            for state in control.borrow().script_states() {
                write_session_message(&mut writer, &mut session, &WireMessage::ScriptState(state))?;
            }
            for state in control.borrow().shortcut_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::ShortcutState(state),
                )?;
            }
            for state in control.borrow().still_states() {
                write_session_message(&mut writer, &mut session, &WireMessage::StillState(state))?;
            }
            for state in control.borrow().media_list_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::MediaListState(state),
                )?;
            }
            for state in control.borrow().strip_dsp_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::AudioStripDspState(state),
                )?;
            }
            for state in control.borrow().strip_send_states() {
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::AudioStripSendState(state),
                )?;
            }
            for state in control.borrow().paged_states() {
                write_session_message(&mut writer, &mut session, &WireMessage::PagedState(state))?;
            }
        }
        SyncPayload::Resume(events) => {
            for event in events {
                write_session_message(&mut writer, &mut session, &WireMessage::Event(event))?;
            }
        }
    }

    loop {
        let message = match read_client_message(
            &mut reader,
            control,
            authority,
            &mut native,
            process_shutdown,
            || session_heartbeat_active(&mut session),
        )? {
            ClientRead::Message(message) => message,
            ClientRead::DaemonShutdown(DaemonShutdownReason::ProcessSignal) => {
                let _ = write_session_message(&mut writer, &mut session, &shutdown_message());
                session.disconnect(DisconnectReason::ServerShutdown);
                break;
            }
            ClientRead::Closed | ClientRead::DaemonShutdown(_) => break,
        };
        match message {
            WireMessage::Command(command) => process_command(
                &mut writer,
                &mut session,
                control,
                store,
                durable,
                principal,
                &session_identity,
                &command,
                native.as_deref_mut(),
                process_shutdown,
            )?,
            WireMessage::Heartbeat(heartbeat) => {
                match record_heartbeat(&mut session, control, &session_identity, &heartbeat) {
                    Ok(received_at_ms) => write_session_message(
                        &mut writer,
                        &mut session,
                        &WireMessage::HeartbeatAcknowledgement(HeartbeatAcknowledgementMessage {
                            server: session_identity.clone(),
                            heartbeat_sequence: heartbeat.sequence,
                            received_at_ms,
                        }),
                    )?,
                    Err(message) => write_session_error(
                        &mut writer,
                        &mut session,
                        "invalid_heartbeat",
                        &message,
                    )?,
                }
            }
            WireMessage::DiagnosticsRequest(request) => {
                let encoded_bytes =
                    encode_line(&WireMessage::DiagnosticsRequest(request.clone()))?.len();
                if let Err(error) =
                    session.admit_diagnostics(&request, encoded_bytes, now_millis()?)
                {
                    let current_revision = control.borrow().diagnostics().current_revision;
                    write_session_message(
                        &mut writer,
                        &mut session,
                        &WireMessage::Error(ErrorMessage {
                            request_id: Some(request.request_id),
                            current_revision: Some(current_revision),
                            error: structured_session_error(&error),
                        }),
                    )?;
                } else {
                    write_session_message(
                        &mut writer,
                        &mut session,
                        &WireMessage::DiagnosticsResponse(diagnostics_response(
                            control,
                            request,
                            native
                                .as_deref()
                                .map_or_else(Vec::new, NativeDaemon::source_sync_samples),
                            fm_protocol::DaemonHealth::default(),
                        )?),
                    )?;
                }
            }
            WireMessage::SlideshowSlidesRequest(request) => {
                let slides = control.borrow().slideshow_slides(&request);
                write_session_message(
                    &mut writer,
                    &mut session,
                    &WireMessage::SlideshowSlides(slides),
                )?;
            }
            _ => write_session_error(
                &mut writer,
                &mut session,
                "unexpected_message",
                "only command and heartbeat messages are accepted after the handshake",
            )?,
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum ClientRead {
    Message(WireMessage),
    Closed,
    DaemonShutdown(DaemonShutdownReason),
}

#[cfg(test)]
pub(crate) fn read_client_message(
    reader: &mut MessageReader,
    control: &SharedControl,
    server: &ServerIdentity,
    native: &mut Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
    mut idle: impl FnMut() -> AppResult<bool>,
) -> AppResult<ClientRead> {
    if let Some(reason) = requested_daemon_shutdown(native.as_deref(), process_shutdown) {
        return Ok(ClientRead::DaemonShutdown(reason));
    }
    if !idle()? {
        return Ok(ClientRead::Closed);
    }
    let message = reader.read_message_with_idle(|| {
        if requested_daemon_shutdown(native.as_deref(), process_shutdown).is_some() || !idle()? {
            return Ok(false);
        }
        if let Some(native) = native.as_deref_mut() {
            native.tick_if_due(&mut control.borrow_mut(), server)?;
        }
        Ok(true)
    })?;
    if let Some(reason) = requested_daemon_shutdown(native.as_deref(), process_shutdown) {
        Ok(ClientRead::DaemonShutdown(reason))
    } else if message.is_some() && !idle()? {
        Ok(ClientRead::Closed)
    } else {
        Ok(message.map_or(ClientRead::Closed, ClientRead::Message))
    }
}

pub(crate) fn session_heartbeat_active(session: &mut Session) -> AppResult<bool> {
    match session.check_heartbeat(now_millis()?) {
        Ok(()) => Ok(true),
        Err(SessionError::Disconnected(DisconnectReason::HeartbeatTimeout)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn reconciled_handshake_outcome(
    outcome: ProtocolHandshakeOutcome,
    sync: &SyncPayload,
) -> ProtocolHandshakeOutcome {
    if matches!(&outcome, ProtocolHandshakeOutcome::Resume { .. })
        && matches!(sync, SyncPayload::Snapshot(_))
    {
        ProtocolHandshakeOutcome::Snapshot {
            reason: fm_protocol::SnapshotReason::HistoryUnavailable,
        }
    } else {
        outcome
    }
}

pub(crate) fn current_handshake(
    request: &HandshakeRequest,
    control: &SharedControl,
    project_id: ProjectId,
) -> (HandshakeRequest, ProtocolHandshakeOutcome) {
    let diagnostics = control.borrow().diagnostics();
    let server = ServerIdentity {
        engine_id: diagnostics.engine.engine_id.clone(),
        project_id: project_id.to_string(),
        state_epoch: diagnostics.engine.state_epoch,
        log_id: diagnostics.engine.log_id.clone(),
    };
    let available_from_revision = diagnostics
        .oldest_retained_revision
        .unwrap_or_else(|| diagnostics.current_revision.saturating_add(1));
    let outcome = choose_handshake_outcome(
        &server,
        diagnostics.current_revision,
        available_from_revision,
        request.resume_cursor.as_ref(),
    );
    let mut reconciled = request.clone();
    if !matches!(outcome, ProtocolHandshakeOutcome::Resume { .. }) {
        reconciled.resume_cursor = None;
    }
    (reconciled, outcome)
}

pub(crate) fn handshake_response(
    hello: &ServerHello,
    server: ServerIdentity,
    outcome: ProtocolHandshakeOutcome,
) -> HandshakeResponse {
    HandshakeResponse {
        protocol: hello.protocol,
        granted_role: hello.granted_role,
        permissions: hello.permissions.clone(),
        capabilities: CapabilityReportSummary {
            digest: hello.capabilities_digest.clone(),
            total: 0,
            available: 0,
            degraded: 0,
            unavailable: 0,
        },
        server,
        current_revision: hello.current_revision,
        outcome,
    }
}

pub(crate) fn rejected_handshake_response(
    server: &Server<ControlHandle>,
    control: &SharedControl,
    project_id: ProjectId,
    hello: &HandshakeRequest,
    code: &str,
    message: &str,
) -> HandshakeResponse {
    let diagnostics = control.borrow().diagnostics();
    HandshakeResponse {
        protocol: PROTOCOL_VERSION,
        granted_role: hello.desired_role,
        permissions: Vec::new(),
        capabilities: CapabilityReportSummary {
            digest: server.config().capabilities_digest.clone(),
            total: 0,
            available: 0,
            degraded: 0,
            unavailable: 0,
        },
        server: ServerIdentity {
            engine_id: diagnostics.engine.engine_id,
            project_id: project_id.to_string(),
            state_epoch: diagnostics.engine.state_epoch,
            log_id: diagnostics.engine.log_id,
        },
        current_revision: diagnostics.current_revision,
        outcome: ProtocolHandshakeOutcome::Rejected {
            error: StructuredError {
                code: code.into(),
                message: message.into(),
                fields: Vec::new(),
                retryable: false,
            },
        },
    }
}

pub(crate) fn server_identity(hello: &ServerHello, project_id: ProjectId) -> ServerIdentity {
    ServerIdentity {
        engine_id: hello.engine.engine_id.clone(),
        project_id: project_id.to_string(),
        state_epoch: hello.engine.state_epoch,
        log_id: hello.engine.log_id.clone(),
    }
}

pub(crate) fn control_server_identity(
    control: &ControlService<Policy>,
    project_id: ProjectId,
) -> ServerIdentity {
    let engine = control.diagnostics().engine;
    ServerIdentity {
        engine_id: engine.engine_id,
        project_id: project_id.to_string(),
        state_epoch: engine.state_epoch,
        log_id: engine.log_id,
    }
}

pub(crate) fn event_cursor(cursor: &ResumeCursor) -> EventCursor {
    EventCursor {
        engine: fm_protocol::EngineIdentity {
            engine_id: cursor.server.engine_id.clone(),
            state_epoch: cursor.server.state_epoch,
            log_id: cursor.server.log_id.clone(),
        },
        revision: cursor.revision,
    }
}

pub(crate) fn record_heartbeat(
    session: &mut Session,
    control: &SharedControl,
    server: &ServerIdentity,
    heartbeat: &HeartbeatMessage,
) -> Result<u64, String> {
    if heartbeat.server != *server {
        return Err("heartbeat server identity does not match the session".into());
    }
    let last_applied = if let Some(cursor) = &heartbeat.last_applied {
        if cursor.server != *server {
            return Err("heartbeat cursor identity does not match the session".into());
        }
        if cursor.revision > control.borrow().diagnostics().current_revision {
            return Err("heartbeat cursor is ahead of the server revision".into());
        }
        Some(event_cursor(cursor))
    } else {
        None
    };
    let received_at_ms = now_millis().map_err(|error| error.to_string())?;
    session
        .record_heartbeat(
            Heartbeat {
                last_applied,
                clock_sample_ms: heartbeat.sent_at_ms,
            },
            received_at_ms,
        )
        .map_err(|error| error.to_string())?;
    Ok(received_at_ms)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_command(
    writer: &mut TcpStream,
    session: &mut Session,
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<()> {
    let delivery = execute_session_command(
        session,
        control,
        store,
        durable,
        principal,
        server,
        command,
        native,
        process_shutdown,
    )?;
    write_session_message(
        writer,
        session,
        &WireMessage::CommandResult(delivery.result),
    )?;
    for event in delivery.events {
        write_session_message(writer, session, &WireMessage::Event(event))?;
    }
    for event in delivery.runtime_events {
        write_session_message(writer, session, &WireMessage::RuntimeEvent(event))?;
    }
    Ok(())
}

pub(crate) struct CommandDelivery {
    pub(crate) result: CommandResult,
    #[cfg(test)]
    pub(crate) events: Vec<EventMessage>,
    #[cfg(test)]
    pub(crate) runtime_events: Vec<RuntimeEventMessage>,
}

impl CommandDelivery {
    pub(crate) fn answer(result: CommandResult) -> Self {
        Self {
            result,
            #[cfg(test)]
            events: Vec::new(),
            #[cfg(test)]
            runtime_events: Vec::new(),
        }
    }
}

/// Admits `command` to `session`'s in-flight and rate budgets, or returns
/// the session's refusal of it. An admitted command holds an in-flight slot
/// until [`Session::command_completed`].
pub(crate) fn admit_session_command(
    session: &mut Session,
    control: &ControlService<Policy>,
    command: &CommandMessage,
    now: u64,
) -> AppResult<Option<CommandResult>> {
    let encoded_bytes = encode_line(&WireMessage::Command(command.clone()))?.len();
    // The session authorizes its scoped role against the classes fm-control
    // resolves now, including the edit an undo or redo would revert or reapply.
    let required = control.required_classes(&command.payload);
    Ok(session
        .admit_command(command, required, encoded_bytes, now)
        .err()
        .map(|error| session_rejection(command, &error, control.diagnostics().current_revision)))
}

/// Whether a session command may join a group commit
/// ([`execute_command_group`]). Commands that write files outside the
/// journal transaction as part of their append (a LUT import) or before it
/// (host media copied into the bundle) are executed on their own, exactly
/// as before group commit, so that their refusals and clean-ups stay those
/// of a single command.
pub(crate) fn group_commit_eligible(command: &CommandMessage) -> bool {
    !matches!(
        command.payload,
        CommandPayload::ImportGradeLut { .. }
            | CommandPayload::InputLifecycle(
                fm_protocol::InputLifecycleOperation::Import { .. }
                    | fm_protocol::InputLifecycleOperation::ReplaceSource {
                        source: fm_protocol::WireInputSource::Media { .. },
                        ..
                    }
            )
    )
}

/// Whether a session command may join a native daemon's group commit: a
/// [`group_commit_eligible`] one that the native runtime follows from the
/// project and engine state the group leaves alone. Stinger edits (which
/// preflight their clips on the native runtime), recorder and stream
/// commands, and cue, script and shortcut fires (which may start or stop
/// recorders and streams) hand work to the native runtime per command, and
/// a recording start's acknowledgement may be held, so they are executed on
/// their own, exactly as before group commit.
pub(crate) fn native_group_eligible(command: &CommandMessage) -> bool {
    group_commit_eligible(command)
        && !is_stinger_mutation(command)
        && recording_command_slot(&command.payload).is_none()
        && !matches!(
            command.payload,
            CommandPayload::StreamStart { .. }
                | CommandPayload::StreamStop { .. }
                | CommandPayload::StreamRetry { .. }
                | CommandPayload::StreamStartAll
                | CommandPayload::StreamStopAll
                | CommandPayload::CueControl { .. }
                | CommandPayload::CueAdvance { .. }
                | CommandPayload::RunScript { .. }
                | CommandPayload::FireShortcut { .. }
        )
}

/// The native runtime's refusal of a group member that leaves `candidate`
/// from `current`, or `None`. The runtime follows the catalogue only once
/// the group is durable, so `budget` (taken before the group) still
/// describes the catalogue of `base`, the project before the group: a member
/// that changes the catalogue is vetted for every change the group made up
/// to it, as one-by-one execution vets each against the catalogue the
/// command before it left. A member that changes no input is never refused.
#[cfg(feature = "native-media")]
pub(crate) fn native_group_vet(
    budget: &native_catalogue::NativeCatalogueBudget,
    base: &StoredProject,
    current: &StoredProject,
    candidate: &StoredProject,
) -> Option<CommandRefusal> {
    if native_catalogue::catalogue_keys(current.project())
        == native_catalogue::catalogue_keys(candidate.project())
    {
        return None;
    }
    budget.refusal(base.project(), candidate.project())
}

/// Executes one member of a native group commit on the group's control
/// copy: as [`execute_native_authority_command`] does (host media it names
/// comes into the bundle first, through `store`, which a group that cannot
/// be made durable discards), but its first frame is not realized here. `frame_due` is set when the command would have
/// realized one, and [`realize_native_group`] realizes it for the whole
/// group once the group is durable.
pub(crate) fn execute_native_group_member(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    member: &GroupMember,
    now: u64,
    vet: Option<&ProjectVet<'_>>,
    frame_due: &mut bool,
) -> AppResult<CommandDelivery> {
    let command = &member.command;
    let current_revision = control.diagnostics().current_revision;
    if let Some(result) = playlist_clock::playlist_command_refusal(command, current_revision)
        .or_else(|| cue_clock::cue_command_refusal(command, current_revision))
    {
        return Ok(CommandDelivery::answer(result));
    }
    let imported = import_host_media(control, store, &member.principal, command, now);
    let command = imported.command.as_ref().unwrap_or(command);
    let execution = execute_durable_command_with_ticks(
        control,
        store,
        durable,
        &member.principal,
        &member.server,
        command,
        now,
        vet,
        Realize::First,
        |_, _| {
            *frame_due = true;
            Ok(Vec::new())
        },
    )?;
    imported.discard_unused(store, durable.project());
    Ok(durable_delivery(execution))
}

/// Realizes what a durable native group commit leaves, on the live service
/// (`control`, which adopted the group) and `durable` (the project after
/// its last member): the frame its members began on, when any of them
/// realizes one (waiting for the pacer's deadline, as a single command
/// does), then the native runtime's sources, titles, documents and stream
/// targets follow the project, as after a single command.
#[cfg_attr(not(feature = "native-media"), allow(unused_variables))]
pub(crate) fn realize_native_group(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &StoredProject,
    native: &mut NativeDaemon,
    server: &ServerIdentity,
    frame_due: bool,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<()> {
    native.invalidate_projection();
    if frame_due {
        // A media list stepped to the item its standby opened switches on
        // this very frame (IN-006).
        #[cfg(feature = "native-media")]
        native.adopt_prefetched_media(control);
        if native
            .wait_and_tick(control, server, process_shutdown)?
            .is_none()
        {
            control.tick_for_shutdown(server)?;
        }
    }
    #[cfg(feature = "native-media")]
    {
        native.titles.observe(durable.project());
        native.titles.observe_templates(&control.title_templates());
        if let Some(pages) = store.documents() {
            native.documents.observe(durable.project(), pages);
        }
        native.sync_input_catalogue(control, durable.project())?;
        native.sync_media_list_prefetch(durable.project());
        native.sync_stream_targets(durable.project(), control.frame_cursor())?;
        native.sync_input_lifecycle(control)?;
    }
    Ok(())
}
