//! The daemon's command window: admitting a client command, executing it
//! alone or as one group commit, importing host media, staging stinger
//! mutations, and the durable delivery and receipt bookkeeping.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(unused_imports, clippy::wildcard_imports)]
// the binary items, some of them native only
use super::*;

/// One client command of a group commit and who sent it.
pub(crate) struct GroupMember {
    pub(crate) principal: Principal,
    pub(crate) server: ServerIdentity,
    pub(crate) command: CommandMessage,
}

/// What a group commit answers one member.
pub(crate) struct GroupOutcome {
    pub(crate) delivery: CommandDelivery,
    /// The session admitted the command, so it holds an in-flight slot the
    /// caller releases.
    pub(crate) admitted: bool,
}

/// Executes several clients' commands as one group commit: one journal
/// append and one disk sync for all of them, where executing them one by
/// one costs one each.
///
/// The members are executed in order, exactly as one-by-one execution
/// would (each sees the state the previous one left), but on a speculative
/// copy of the control service ([`ControlService::speculate`]) that
/// publishes nothing, and their journal records are collected instead of
/// appended ([`GroupRecorder`]). Then:
///
/// - the whole group is appended in one transaction
///   ([`DurableStore::record_group`]); only once it is durable does the
///   copy replace the live service ([`ControlService::adopt`]), which then
///   publishes the members' events in order, and are the members answered;
/// - if the append fails, the copy is dropped: no member applied, nothing
///   was published, no receipt was kept, and every admitted member is
///   refused as not durable (retryable), as a single command whose append
///   failed is. Members refused or answered from a receipt the group made
///   are refused the same way, since their answers were decided against
///   the group's unrecorded state. A member answered from the receipt of a
///   command durable before the group keeps that answer (it touched no
///   journal), and a member the session refused keeps its refusal, at the
///   live revision.
///
/// On a native daemon (`native`), the members ([`native_group_eligible`]
/// ones) all begin on the frame the pacer realizes next: none realizes a
/// frame of its own, so each is prepared on the state the one before it
/// left, on the same frame cursor, and journaled with that frame, which is
/// exactly what replay reproduces (it ticks no idle frame between records
/// that began on one frame). Once the group is durable and adopted, that
/// frame is realized once for all of them, as a single command's is
/// ([`realize_native_group`]), before anyone is answered. A group that could
/// not be made durable realizes nothing; the pacer realizes the frame idle.
///
/// A refused command is never journaled: its receipt rides the next
/// checkpoint, as for one command. `admit` runs before each member on the
/// copy (catch-up and session admission) and returns the session's refusal
/// of a member it did not admit. An error executing any member stops the
/// daemon, as it does for one command; no member of the group was made
/// durable or answered then.
#[allow(clippy::too_many_lines)]
#[cfg_attr(not(feature = "native-media"), allow(unused_mut))]
pub(crate) fn execute_command_group(
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    members: &[GroupMember],
    mut native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
    mut admit: impl FnMut(
        usize,
        &mut ControlService<Policy>,
        &StoredProject,
        u64,
    ) -> AppResult<Option<CommandResult>>,
) -> AppResult<Vec<GroupOutcome>> {
    let speculation: SharedControl = Rc::new(RefCell::new(control.borrow().speculate()));
    let recorder = GroupRecorder::new(store);
    let mut staged = durable.clone();
    let mut outcomes = Vec::with_capacity(members.len());
    // A native group's members are vetted against the catalogue the native
    // runtime holds now, before any of them: it follows their changes only
    // once the group is durable (see `native_group_vet`).
    #[cfg(feature = "native-media")]
    let budget = native.as_deref_mut().map(NativeDaemon::catalogue_budget);
    #[cfg(feature = "native-media")]
    let base = native.is_some().then(|| durable.clone());
    #[cfg(feature = "native-media")]
    let vet = |current: &StoredProject, candidate: &StoredProject| {
        native_group_vet(budget.as_ref()?, base.as_ref()?, current, candidate)
    };
    #[cfg(feature = "native-media")]
    let vet: Option<&ProjectVet<'_>> = Some(&vet);
    #[cfg(not(feature = "native-media"))]
    let vet: Option<&ProjectVet<'_>> = None;
    // Whether a native member's first frame is due: the group realizes it
    // once, after its append (see `realize_native_group`).
    let mut frame_due = false;
    for (index, member) in members.iter().enumerate() {
        let now = now_millis()?;
        if let Some(result) = admit(index, &mut speculation.borrow_mut(), &staged, now)? {
            outcomes.push(GroupOutcome {
                delivery: CommandDelivery::answer(result),
                admitted: false,
            });
            continue;
        }
        let delivery = if native.is_some() {
            execute_native_group_member(
                &mut speculation.borrow_mut(),
                &recorder,
                &mut staged,
                member,
                now,
                vet,
                &mut frame_due,
            )?
        } else {
            execute_admitted_command(
                &speculation,
                &recorder,
                &mut staged,
                &member.principal,
                &member.server,
                &member.command,
                now,
                None,
                process_shutdown,
            )?
        };
        outcomes.push(GroupOutcome {
            delivery,
            admitted: true,
        });
    }
    let records = recorder.records.take();
    let durability = if records.is_empty() {
        Ok(())
    } else {
        store.record_group(durable, &records, &staged)
    };
    match durability {
        Ok(()) => {
            let speculation = Rc::try_unwrap(speculation)
                .map_err(|_| AppFailure("the group commit's control copy is still shared".into()))?
                .into_inner();
            let _ = control.borrow_mut().adopt(speculation);
            *durable = staged;
            if let (Some(native), Some(member)) = (native, members.first()) {
                realize_native_group(
                    &mut control.borrow_mut(),
                    store,
                    durable,
                    native,
                    &member.server,
                    frame_due,
                    process_shutdown,
                )?;
            }
        }
        Err(error) => {
            drop(speculation);
            recorder.discard()?;
            let current_revision = control.borrow().diagnostics().current_revision;
            let base_revision = durable.position().revision;
            let attempted = staged.position().revision;
            for (outcome, member) in outcomes.iter_mut().zip(members) {
                match &mut outcome.delivery.result {
                    // Answered from the receipt of a command durable before
                    // the group: exactly the answer it gets alone, which
                    // touches no journal. Denying it now would contradict
                    // the acknowledgement it already had.
                    CommandResult::Accepted { revision, .. } if *revision <= base_revision => {
                        continue;
                    }
                    // A session's refusal stands, but at the revision the
                    // daemon is really at, not one the dropped group reached.
                    CommandResult::Rejected {
                        current_revision: reported,
                        ..
                    } if !outcome.admitted => {
                        *reported = current_revision;
                    }
                    _ => {}
                }
                if outcome.admitted {
                    eprintln!(
                        "FREEMIXD_DURABILITY\tv=1\tcommand={}\trevision={attempted}\terror={error}",
                        member.command.id
                    );
                    outcome.delivery = CommandDelivery::answer(
                        refused_submission(&member.command, current_revision, error.as_ref())
                            .output
                            .result,
                    );
                }
            }
        }
    }
    Ok(outcomes)
}

/// The store a group commit's members execute against: it collects each
/// member's journal record for [`DurableStore::record_group`] instead of
/// appending it, and hands everything else (staging template animations,
/// importing media) to the daemon's store, remembering what it wrote so a
/// group that cannot be made durable removes it again.
pub(crate) struct GroupRecorder<'a> {
    store: &'a dyn DurableStore,
    pub(crate) records: RefCell<Vec<journal::GroupRecord>>,
    staged: RefCell<Vec<(u64, Vec<String>)>>,
    imported: RefCell<Vec<String>>,
}

impl<'a> GroupRecorder<'a> {
    pub(crate) fn new(store: &'a dyn DurableStore) -> Self {
        Self {
            store,
            records: RefCell::new(Vec::new()),
            staged: RefCell::new(Vec::new()),
            imported: RefCell::new(Vec::new()),
        }
    }

    /// Removes what the group staged and imported, as each member would
    /// have on its own failed append.
    pub(crate) fn discard(&self) -> AppResult<()> {
        for asset in self.imported.take() {
            if let Err(error) = self.store.discard_imported_media(&asset) {
                eprintln!("FREEMIXD_IMPORT\tv=1\tasset={asset}\terror={error}");
            }
        }
        for (revision, uris) in self.staged.take() {
            self.store
                .discard_staged_title_animations(revision, &uris)?;
        }
        Ok(())
    }
}

impl DurableStore for GroupRecorder<'_> {
    fn record(
        &self,
        command: &CommandMessage,
        now_millis: u64,
        began_frame: u64,
        previous: &StoredProject,
        updated: &StoredProject,
    ) -> AppResult<()> {
        self.records.borrow_mut().push(journal::GroupRecord {
            command: command.clone(),
            now_millis,
            began_frame,
            base_revision: previous.position().revision,
            position: updated.position(),
        });
        Ok(())
    }

    fn checkpoint(&self, _project: &StoredProject) -> AppResult<()> {
        Err(AppFailure("a group commit checkpoints only when it is recorded".into()).into())
    }

    fn busy(&self) -> bool {
        self.store.busy()
    }

    fn documents(&self) -> Option<&Arc<documents::DocumentPages>> {
        self.store.documents()
    }

    fn stage_title_animations(
        &self,
        revision: u64,
        animations: &[(String, journal::StagedTemplate)],
    ) -> AppResult<()> {
        if animations.is_empty() {
            return Ok(());
        }
        self.staged.borrow_mut().push((
            revision,
            animations.iter().map(|(uri, _)| uri.clone()).collect(),
        ));
        self.store.stage_title_animations(revision, animations)
    }

    fn discard_staged_title_animations(&self, revision: u64, uris: &[String]) -> AppResult<()> {
        self.staged
            .borrow_mut()
            .retain(|(staged, _)| *staged != revision);
        self.store.discard_staged_title_animations(revision, uris)
    }

    fn recent_append_peak(&self) -> Option<Duration> {
        self.store.recent_append_peak()
    }

    fn import_media_path(&self, path: &Path) -> AppResult<Option<(String, bool)>> {
        let imported = self.store.import_media_path(path)?;
        if let Some((asset, true)) = &imported {
            self.imported.borrow_mut().push(asset.clone());
        }
        Ok(imported)
    }

    fn discard_imported_media(&self, asset_uri: &str) -> AppResult<()> {
        self.imported
            .borrow_mut()
            .retain(|asset| asset != asset_uri);
        self.store.discard_imported_media(asset_uri)
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_session_command(
    session: &mut Session,
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<CommandDelivery> {
    // A live fire's Preview and Program are the authority's to bind
    // (RC-004); only the console's selection comes from the client.
    let stripped =
        fm_control::client_fire_payload(&command.payload).map(|payload| CommandMessage {
            payload,
            ..command.clone()
        });
    let command = stripped.as_ref().unwrap_or(command);
    let now = now_millis()?;
    if let Some(result) = admit_session_command(session, &control.borrow(), command, now)? {
        return Ok(CommandDelivery::answer(result));
    }
    // Admission reserved an in-flight slot; it is released whether execution
    // succeeds, is refused, or fails, so no exit path can leak the slot.
    let delivery = execute_admitted_command(
        control,
        store,
        durable,
        principal,
        server,
        command,
        now,
        native,
        process_shutdown,
    );
    let completed = session.command_completed();
    let delivery = delivery?;
    completed?;
    Ok(delivery)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::used_underscore_binding
)]
pub(crate) fn execute_admitted_command(
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now: u64,
    native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<CommandDelivery> {
    let current_revision = control.borrow().diagnostics().current_revision;
    let refusal = playlist_clock::playlist_command_refusal(command, current_revision)
        .or_else(|| cue_clock::cue_command_refusal(command, current_revision));
    if let Some(result) = refusal {
        return Ok(CommandDelivery {
            result,
            #[cfg(test)]
            events: Vec::new(),
            #[cfg(test)]
            runtime_events: Vec::new(),
        });
    }
    execute_authority_command(
        control,
        store,
        durable,
        principal,
        server,
        command,
        now,
        native,
        process_shutdown,
    )
}

/// Executes one command a client may send, or one the daemon itself issues
/// (the playlist clock's `playlist_advance`), through the simulated or native
/// realization path.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::used_underscore_binding
)]
pub(crate) fn execute_authority_command(
    control: &SharedControl,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now: u64,
    native: Option<&mut NativeDaemon>,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<CommandDelivery> {
    let mut control = control.borrow_mut();
    if let Some(native) = native {
        return execute_native_authority_command(
            &mut control,
            store,
            durable,
            principal,
            server,
            command,
            now,
            native,
            process_shutdown,
        );
    }
    let imported = import_host_media(&mut control, store, principal, command, now);
    let command = imported.command.as_ref().unwrap_or(command);
    // As on the native daemon, the command realizes only its first frame
    // and is answered then; the session loop's simulated pacer realizes
    // the rest of a timed transition or title phase at the show's frame
    // rate (see `sessions::SimulatedPacer`). A clock's advance first
    // realizes the idle frames wall time has already played, so its first
    // frame is the one it is due on.
    catch_up_simulated_advance(&mut control, server, &command.payload)?;
    let execution = execute_durable_command_with_ticks(
        &mut control,
        store,
        durable,
        principal,
        server,
        command,
        now,
        None,
        Realize::First,
        |control, server| Ok(control.tick(server)?.runtime_events),
    )?;
    imported.discard_unused(store, durable.project());
    // Imported, duplicated, replaced and reopened media inputs play from a
    // synthetic source as soon as they exist (IN-026).
    attach_synthetic_media(&mut control, durable.project())?;
    Ok(durable_delivery(execution))
}

/// Simulated daemon only: before a clock's `playlist_advance` or
/// `cue_advance` for `frame` executes, ticks the engine idle until `frame`
/// is the next frame it renders. The simulated engine renders no idle frames, so its cursor
/// trails the show's wall-clock frame the clock fired at; those frames were
/// played already and are realized at once (never onto another pending
/// boundary or waiting cue), and the advance's own first frame is `frame`.
/// Its journal record names the frame it was committed at, so replay ticks
/// the same frames.
pub(crate) fn catch_up_simulated_advance(
    control: &mut ControlService<Policy>,
    server: &ServerIdentity,
    payload: &CommandPayload,
) -> AppResult<()> {
    let (CommandPayload::PlaylistAdvance { frame } | CommandPayload::CueAdvance { frame, .. }) =
        *payload
    else {
        return Ok(());
    };
    // The cursor is the next frame to render: it stops on `frame`, and on
    // any other boundary or cue frame before it, unrendered.
    let mut target = frame;
    if let Some(boundary) = control.playlist_advance_frame() {
        target = target.min(boundary);
    }
    if let Some((_, due)) = control.cue_advance_frame() {
        target = target.min(due);
    }
    while control.frame_cursor() < target {
        control.tick(server)?;
    }
    Ok(())
}

/// Executes one command through the native realization path, with the
/// control service already borrowed: from the session loop, and from frame
/// loops that run before it (the playlist clock during the recorder startup
/// barrier).
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::used_underscore_binding
)]
pub(crate) fn execute_native_authority_command(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now: u64,
    native: &mut NativeDaemon,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<CommandDelivery> {
    // Live catalogue changes (IN-026): host media an import or a source
    // replacement names comes into the bundle first, and the native
    // runtime vets the catalogue a command would leave before it is staged.
    let imported = import_host_media(control, store, principal, command, now);
    let command = imported.command.as_ref().unwrap_or(command);
    #[cfg(feature = "native-media")]
    let budget = native.catalogue_budget();
    #[cfg(feature = "native-media")]
    let vet = move |current: &StoredProject, candidate: &StoredProject| {
        budget.refusal(current.project(), candidate.project())
    };
    #[cfg(feature = "native-media")]
    let vet: Option<&ProjectVet<'_>> = Some(&vet);
    #[cfg(not(feature = "native-media"))]
    let vet: Option<&ProjectVet<'_>> = None;
    native.invalidate_projection();
    let execution = if is_stinger_mutation(command) {
        match execute_native_stinger_mutation(
            control,
            store,
            durable,
            principal,
            server,
            command,
            now,
            native,
            process_shutdown,
        )? {
            Ok(execution) => execution,
            Err(failure) => {
                return Ok(CommandDelivery {
                    result: failure.result,
                    #[cfg(test)]
                    events: Vec::new(),
                    #[cfg(test)]
                    runtime_events: failure.runtime_events,
                });
            }
        }
    } else {
        execute_durable_command_with_ticks(
            control,
            store,
            durable,
            principal,
            server,
            command,
            now,
            vet,
            Realize::First,
            |control, server| {
                // A media list stepped to the item its standby opened
                // switches on this very frame (IN-006).
                #[cfg(feature = "native-media")]
                native.adopt_prefetched_media(control);
                native
                    .wait_and_tick(control, server, process_shutdown)?
                    .map_or_else(|| Ok(control.tick_for_shutdown(server)?.runtime_events), Ok)
            },
        )?
    };
    // Live sink changes and recording segments are realized by the
    // native runtime right here, after durability and the engine
    // commit have settled but before the acknowledgement is queued.
    // A recording start only begins here; the session loop holds its
    // acknowledgement until the segment is mux-ready.
    #[cfg(feature = "native-media")]
    native.observe_stream_command(command, &execution.submission.output.result);
    #[cfg(feature = "native-media")]
    native.observe_recording_command(
        command,
        &execution.submission.output.result,
        durable,
        control.frame_cursor(),
    );
    // A cue's record and stream actions start and stop the same workers
    // (RC-017); its acknowledgement is not held for a segment's readiness.
    #[cfg(feature = "native-media")]
    for worker in cue_worker_commands(command, &execution.submission) {
        native.observe_stream_command(&worker, &execution.submission.output.result);
        native.observe_recording_command(
            &worker,
            &execution.submission.output.result,
            durable,
            control.frame_cursor(),
        );
    }
    // Changed title fields re-render on the title worker; this only
    // compares and hands over state.
    #[cfg(feature = "native-media")]
    native.titles.observe(durable.project());
    #[cfg(feature = "native-media")]
    native.titles.observe_templates(&control.title_templates());
    // A document input's page renders on the page worker (IN-014).
    #[cfg(feature = "native-media")]
    if let Some(pages) = store.documents() {
        native.documents.observe(durable.project(), pages);
    }
    #[cfg(feature = "native-media")]
    native.sync_input_catalogue(control, durable.project())?;
    #[cfg(feature = "native-media")]
    native.sync_media_list_prefetch(durable.project());
    // Destination edits (OR-006) rebuild only the edited destination.
    #[cfg(feature = "native-media")]
    native.sync_stream_targets(durable.project(), control.frame_cursor())?;
    #[cfg(feature = "native-media")]
    native.sync_input_lifecycle(control)?;
    imported.discard_unused(store, durable.project());
    Ok(durable_delivery(execution))
}

#[allow(clippy::used_underscore_binding)]
pub(crate) fn durable_delivery(execution: DurableExecution) -> CommandDelivery {
    let DurableExecution {
        submission,
        runtime_events: _runtime_events,
    } = execution;
    CommandDelivery {
        result: submission.output.result,
        #[cfg(test)]
        events: submission.output.events,
        #[cfg(test)]
        runtime_events: _runtime_events,
    }
}

pub(crate) fn is_stinger_mutation(command: &CommandMessage) -> bool {
    matches!(
        command.payload,
        CommandPayload::ConfigureStinger { .. } | CommandPayload::RemoveStinger { .. }
    )
}

#[cfg(feature = "native-media")]
pub(crate) fn native_stinger_preflight_rejection(
    command: &CommandMessage,
    current_revision: u64,
) -> CommandResult {
    CommandResult::Rejected {
        id: command.id.clone(),
        code: RejectionCode::Unavailable.as_str().to_owned(),
        message: "native Stinger resources could not be prepared".to_owned(),
        fields: Vec::new(),
        current_revision,
        retryable: false,
        reason: None,
    }
}

/// The host media files one batch import or source replacement brought
/// into the bundle (IN-026): the command to execute with their `asset://`
/// URIs, and the assets newly written for it.
#[derive(Default)]
pub(crate) struct HostMediaImport {
    /// The rewritten command, or `None` to execute the command as sent.
    pub(crate) command: Option<CommandMessage>,
    /// Assets this import wrote (not ones it found already in the bundle).
    pub(crate) copied: Vec<String>,
}

impl HostMediaImport {
    /// Removes every asset this import wrote that `project` (the project
    /// after the command) does not name: the command was refused, or an
    /// item it was copied for was. A copy that cannot be removed is only
    /// reported; it is an unreferenced file, never a broken project.
    pub(crate) fn discard_unused(&self, store: &dyn DurableStore, project: &fm_model::Project) {
        for uri in &self.copied {
            let used = project.inputs().iter().any(|input| match &input.kind {
                fm_model::InputKind::Media { asset_uri, .. }
                | fm_model::InputKind::Slideshow(fm_types::still::SlideshowConfig {
                    source: fm_types::still::SlideshowSource::Folder { asset_uri },
                    ..
                }) => asset_uri == uri,
                kind => kind.file_asset_uris().contains(&uri.as_str()),
            }) || project
                .media_lists()
                .values()
                .any(|list| list.items.iter().any(|item| item.asset_uri == *uri));
            if !used && let Err(error) = store.discard_imported_media(uri) {
                eprintln!("FREEMIXD_IMPORT\tv=1\tasset={uri}\terror={error}");
            }
        }
    }
}

/// The host path `uri` names when a batch import or a source replacement
/// should bring it into the bundle: an absolute path to a regular media or
/// picture file.
pub(crate) fn importable_host_path(uri: &str) -> Option<&Path> {
    let path = Path::new(uri);
    (!uri.starts_with(fm_model::MEDIA_ASSET_URI_PREFIX)
        && path.is_absolute()
        && (fm_model::is_importable_file_name(uri) || fm_types::still::is_image_file_name(uri))
        && path.is_file())
    .then_some(path)
}

/// The host folder a folder slideshow names when it should be brought into
/// the bundle: an absolute path to a folder (IN-009).
pub(crate) fn importable_host_folder(uri: &str) -> Option<&Path> {
    let path = Path::new(uri);
    (!uri.starts_with(fm_model::MEDIA_ASSET_URI_PREFIX) && path.is_absolute() && path.is_dir())
        .then_some(path)
}

/// The media URIs of a batch import (every item), a media, image, or
/// slideshow source replacement, or an image or slideshow input addition,
/// mutably, each with whether it names a folder.
pub(crate) fn host_media_uris(payload: &mut CommandPayload) -> Vec<(&mut String, bool)> {
    match payload {
        CommandPayload::InputLifecycle(fm_protocol::InputLifecycleOperation::Import { items }) => {
            items
                .iter_mut()
                .map(|item| (&mut item.uri, false))
                .collect()
        }
        CommandPayload::InputLifecycle(fm_protocol::InputLifecycleOperation::ReplaceSource {
            source: fm_protocol::WireInputSource::Media { uri },
            ..
        }) => vec![(uri, false)],
        CommandPayload::InputLifecycle(fm_protocol::InputLifecycleOperation::ReplaceSource {
            source: fm_protocol::WireInputSource::Image(image),
            ..
        }) => vec![(&mut image.asset_uri, false)],
        CommandPayload::InputLifecycle(fm_protocol::InputLifecycleOperation::ReplaceSource {
            source: fm_protocol::WireInputSource::Slideshow(slideshow),
            ..
        }) => match &mut slideshow.source {
            fm_types::still::SlideshowSource::Folder { asset_uri } => vec![(asset_uri, true)],
            fm_types::still::SlideshowSource::List(items) => items
                .iter_mut()
                .map(|item| (&mut item.asset_uri, false))
                .collect(),
        },
        CommandPayload::AddInput {
            source: fm_protocol::LiveInputSource::Image(image),
            ..
        } => vec![(&mut image.asset_uri, false)],
        CommandPayload::AddInput {
            source: fm_protocol::LiveInputSource::Slideshow(slideshow),
            ..
        } => match &mut slideshow.source {
            fm_types::still::SlideshowSource::Folder { asset_uri } => vec![(asset_uri, true)],
            fm_types::still::SlideshowSource::List(items) => items
                .iter_mut()
                .map(|item| (&mut item.asset_uri, false))
                .collect(),
        },
        // Every item of a media list being created or edited (IN-006).
        CommandPayload::MediaList {
            operation:
                fm_protocol::MediaListOperation::Create { items, .. }
                | fm_protocol::MediaListOperation::Set { items, .. },
            ..
        } => items
            .iter_mut()
            .map(|item| (&mut item.uri, false))
            .collect(),
        _ => Vec::new(),
    }
}

/// Brings the host media files a batch import or a source replacement names
/// into the bundle (IN-026): each absolute host path to a regular media file
/// becomes the `asset://` URI of its imported copy, and the command is
/// executed (and journaled) with those URIs. A path that cannot be imported
/// is left as it is, and the engine refuses that item as unavailable.
///
/// Nothing is copied for a command the engine would refuse, nor for an
/// import item it would refuse: the command is first prepared (and
/// abandoned) with each host path standing in as the URI its import tries
/// first. A refused command then executes with those stand-ins, so it is
/// refused exactly as prepared, with its own reason. A stand-in can only
/// differ from the final URI by a numeric suffix, so the prepared refusals
/// are the executed ones (a stand-in shared by two items is refused as a
/// duplicate source, so such an item is copied and the engine decides). The
/// caller discards whatever the executed command does not keep
/// ([`HostMediaImport::discard_unused`]).
///
/// Copying runs on the command loop, like every other durable write before
/// the journal append: the command is journaled with its final URIs, so it
/// cannot be acknowledged before its files are in the bundle. Only files the
/// command keeps are copied.
pub(crate) fn import_host_media(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    principal: &Principal,
    command: &CommandMessage,
    now_millis: u64,
) -> HostMediaImport {
    let mut provisional = command.clone();
    let mut hosts = Vec::new();
    for (index, (uri, folder)) in host_media_uris(&mut provisional.payload)
        .into_iter()
        .enumerate()
    {
        let path = if folder {
            importable_host_folder(uri)
        } else {
            importable_host_path(uri)
        };
        if let Some(path) = path {
            hosts.push((index, path.to_path_buf(), folder));
            *uri = fm_persistence::media_import_asset_uri(path);
        }
    }
    if hosts.is_empty() {
        return HostMediaImport::default();
    }
    // Which of the importable items the engine would keep; `None` when it
    // cannot tell (copy them all, as the executed command decides).
    let kept: Option<Vec<usize>> =
        match control.prepare_submit(principal, provisional.clone(), now_millis) {
            Ok(PrepareSubmitOutcome::Replayed(_)) => {
                // A replay answers from its receipt whatever the payload.
                return HostMediaImport::default();
            }
            Ok(PrepareSubmitOutcome::Prepared(prepared))
                if !prepared.submission().is_accepted() =>
            {
                drop(prepared);
                return HostMediaImport {
                    command: Some(provisional),
                    copied: Vec::new(),
                };
            }
            Ok(PrepareSubmitOutcome::Prepared(prepared)) => {
                let refused: Vec<usize> = prepared
                    .output()
                    .events
                    .iter()
                    .filter_map(|event| match &event.payload {
                        fm_protocol::EventPayload::InputLifecycleChanged { refused, .. } => {
                            Some(refused)
                        }
                        _ => None,
                    })
                    .flatten()
                    .filter(|(_, refusal)| *refusal != fm_protocol::ImportRefusal::DuplicateSource)
                    .map(|(index, _)| usize::from(*index))
                    .collect();
                Some(
                    hosts
                        .iter()
                        .map(|(index, _, _)| *index)
                        .filter(|index| !refused.contains(index))
                        .collect(),
                )
            }
            Err(_) => None,
        };
    let mut rewritten = provisional;
    let mut copied = Vec::new();
    let mut uris = host_media_uris(&mut rewritten.payload);
    for (index, path, folder) in hosts {
        if kept.as_ref().is_some_and(|kept| !kept.contains(&index)) {
            // Refused as prepared; the stand-in keeps its reason.
            continue;
        }
        let uri = &mut *uris[index].0;
        if folder {
            // A folder is always copied anew, so the bundle folder it
            // becomes is the slideshow's own to follow.
            match store.import_picture_folder_path(&path) {
                Ok(Some(asset)) => {
                    copied.push(asset.clone());
                    *uri = asset;
                }
                Ok(None) | Err(_) => path.to_str().unwrap_or_default().clone_into(uri),
            }
            continue;
        }
        match store.import_media_path(&path) {
            Ok(Some((asset, wrote))) => {
                if wrote {
                    copied.push(asset.clone());
                }
                *uri = asset;
            }
            // Not imported: the host path makes the engine refuse the item
            // as unavailable.
            Ok(None) | Err(_) => path.to_str().unwrap_or_default().clone_into(uri),
        }
    }
    HostMediaImport {
        command: Some(rewritten),
        copied,
    }
}

pub(crate) struct NativeMutationFailure {
    pub(crate) result: CommandResult,
    #[cfg(test)]
    pub(crate) runtime_events: Vec<RuntimeEventMessage>,
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
#[cfg(feature = "native-media")]
pub(crate) fn execute_native_stinger_mutation(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now_millis: u64,
    native: &mut NativeDaemon,
    process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<Result<DurableExecution, NativeMutationFailure>> {
    // Every step before a `record` only stages; failures there refuse the
    // command as `staging_refusal` classifies them (dropping the staged
    // native resources), failures from a commit on are fatal.
    let current_revision = control.diagnostics().current_revision;
    let began_frame = control.frame_cursor();
    let first_preparation = match control.prepare_submit(principal, command.clone(), now_millis) {
        Ok(preparation) => preparation,
        Err(error) => {
            return refuse_staged_command(command, current_revision, error.into(), Vec::new())
                .map(Ok);
        }
    };
    let first_prepared = match first_preparation {
        PrepareSubmitOutcome::Replayed(submission) => {
            return Ok(Ok(DurableExecution {
                submission,
                runtime_events: Vec::new(),
            }));
        }
        PrepareSubmitOutcome::Prepared(prepared) => prepared,
    };
    if !first_prepared.submission().is_accepted() {
        let staged = first_prepared
            .project_settled(0)
            .1
            .map_err(Into::into)
            .and_then(|projected| {
                stored_project_from_snapshot(
                    durable,
                    &projected,
                    command,
                    command,
                    &first_prepared.output().result,
                )
            });
        let updated = match staged {
            Ok(updated) => updated,
            Err(error) => {
                drop(first_prepared);
                return refuse_staged_command(command, current_revision, error, Vec::new()).map(Ok);
            }
        };
        // A durability failure refuses the command; it does not take the daemon
        // down. Dropping the preparation abandons it without touching the
        // engine, exactly as the simulated path does.
        if let Err(error) = store.record(command, now_millis, began_frame, durable, &updated) {
            drop(first_prepared);
            return Ok(Ok(DurableExecution {
                submission: refuse_undurable(
                    command,
                    control.diagnostics().current_revision,
                    updated.position().revision,
                    error.as_ref(),
                ),
                runtime_events: Vec::new(),
            }));
        }
        let submission = first_prepared.commit()?;
        *durable = updated;
        return Ok(Ok(DurableExecution {
            submission,
            runtime_events: Vec::new(),
        }));
    }

    let staged = first_prepared
        .project_settled(1)
        .1
        .map_err(Into::into)
        .and_then(|projected| {
            stored_project_from_snapshot(
                durable,
                &projected,
                command,
                command,
                &first_prepared.output().result,
            )
        });
    drop(first_prepared);
    let candidate = match staged {
        Ok(candidate) => candidate,
        Err(error) => {
            return refuse_staged_command(command, current_revision, error, Vec::new()).map(Ok);
        }
    };

    let (mutation, mut preflight_runtime_events) = match native
        .preflight_stinger_mutation_with_ticks(
            candidate.clone(),
            control,
            server,
            process_shutdown,
        )? {
        Some((mutation, events)) => (mutation, events),
        None => (Err(()), Vec::new()),
    };
    let Ok(mutation) = mutation else {
        return Ok(Err(NativeMutationFailure {
            result: native_stinger_preflight_rejection(
                command,
                control.diagnostics().current_revision,
            ),
            #[cfg(test)]
            runtime_events: preflight_runtime_events,
        }));
    };

    // The preflight realized frames: the command commits on this one.
    let began_frame = control.frame_cursor();
    let second_preparation = match control.prepare_submit(principal, command.clone(), now_millis) {
        Ok(preparation) => preparation,
        Err(error) => {
            native.stinger_retirements.discard(mutation)?;
            return refuse_staged_command(
                command,
                current_revision,
                error.into(),
                preflight_runtime_events,
            )
            .map(Ok);
        }
    };
    let second_prepared = match second_preparation {
        PrepareSubmitOutcome::Replayed(submission) => {
            native.stinger_retirements.discard(mutation)?;
            return Ok(Ok(DurableExecution {
                submission,
                runtime_events: preflight_runtime_events,
            }));
        }
        PrepareSubmitOutcome::Prepared(prepared) => prepared,
    };
    if !second_prepared.submission().is_accepted() {
        native.stinger_retirements.discard(mutation)?;
        let staged = second_prepared
            .project_settled(0)
            .1
            .map_err(Into::into)
            .and_then(|projected| {
                stored_project_from_snapshot(
                    durable,
                    &projected,
                    command,
                    command,
                    &second_prepared.output().result,
                )
            });
        let updated = match staged {
            Ok(updated) => updated,
            Err(error) => {
                drop(second_prepared);
                return refuse_staged_command(
                    command,
                    current_revision,
                    error,
                    preflight_runtime_events,
                )
                .map(Ok);
            }
        };
        if let Err(error) = store.record(command, now_millis, began_frame, durable, &updated) {
            drop(second_prepared);
            return Ok(Ok(DurableExecution {
                submission: refuse_undurable(
                    command,
                    control.diagnostics().current_revision,
                    updated.position().revision,
                    error.as_ref(),
                ),
                runtime_events: preflight_runtime_events,
            }));
        }
        let submission = second_prepared.commit()?;
        *durable = updated;
        return Ok(Ok(DurableExecution {
            submission,
            runtime_events: preflight_runtime_events,
        }));
    }

    let staged = second_prepared
        .project_settled(1)
        .1
        .map_err(Into::into)
        .and_then(|projected| {
            stored_project_from_snapshot(
                durable,
                &projected,
                command,
                command,
                &second_prepared.output().result,
            )
        });
    let updated = match staged {
        Ok(updated) => updated,
        Err(error) => {
            drop(second_prepared);
            native.stinger_retirements.discard(mutation)?;
            return refuse_staged_command(
                command,
                current_revision,
                error,
                preflight_runtime_events,
            )
            .map(Ok);
        }
    };
    let retained = native
        .video
        .validate_retained_byte_limit(mutation.ordinary_video_limit)
        .unwrap_or(false);
    if candidate.project().stingers() != updated.project().stingers() || !retained {
        drop(second_prepared);
        native.stinger_retirements.discard(mutation)?;
        return Ok(Err(NativeMutationFailure {
            result: native_stinger_preflight_rejection(
                command,
                control.diagnostics().current_revision,
            ),
            #[cfg(test)]
            runtime_events: preflight_runtime_events,
        }));
    }
    if let Err(error) = store.record(command, now_millis, began_frame, durable, &updated) {
        // The staged native resources are released with the preparation: the
        // command was never applied, so retrying it prepares them again.
        drop(second_prepared);
        native.stinger_retirements.discard(mutation)?;
        return Ok(Ok(DurableExecution {
            submission: refuse_undurable(
                command,
                control.diagnostics().current_revision,
                updated.position().revision,
                error.as_ref(),
            ),
            runtime_events: preflight_runtime_events,
        }));
    }
    let submission = second_prepared.commit()?;
    // The preflight's ticks projected the next frame between realizations;
    // the command just committed realizes on it, so that projection is
    // stale (as it is after any other command, which invalidates before it
    // executes).
    native.invalidate_projection();
    native.stage_stinger_mutation(mutation);
    let runtime_events = match native.wait_and_tick(control, server, process_shutdown)? {
        Some(events) => events,
        None => control.tick_for_shutdown(server)?.runtime_events,
    };
    preflight_runtime_events.extend(runtime_events);
    *durable = updated;
    Ok(Ok(DurableExecution {
        submission,
        runtime_events: preflight_runtime_events,
    }))
}

#[allow(clippy::too_many_arguments)]
#[cfg(not(feature = "native-media"))]
pub(crate) fn execute_native_stinger_mutation(
    _control: &mut ControlService<Policy>,
    _store: &dyn DurableStore,
    _durable: &mut StoredProject,
    _principal: &Principal,
    _server: &ServerIdentity,
    _command: &CommandMessage,
    _now_millis: u64,
    _native: &mut NativeDaemon,
    _process_shutdown: Option<&ProcessShutdown>,
) -> AppResult<Result<DurableExecution, NativeMutationFailure>> {
    Err(AppFailure("native-media support was not compiled in".into()).into())
}

pub(crate) struct DurableExecution {
    pub(crate) submission: CommandSubmission,
    pub(crate) runtime_events: Vec<RuntimeEventMessage>,
}

/// Refuses a command that could not be made durable, and says so.
///
/// Every native command path answers a durability failure the same way,
/// because it is the same failure: a full disk during a show must cost one
/// refused command the operator can retry, never the daemon.
#[cfg(feature = "native-media")]
pub(crate) fn refuse_undurable(
    command: &CommandMessage,
    current_revision: u64,
    attempted_revision: u64,
    error: &(dyn Error + 'static),
) -> CommandSubmission {
    eprintln!(
        "FREEMIXD_DURABILITY\tv=1\tcommand={}\trevision={attempted_revision}\terror={error}",
        command.id
    );
    refused_submission(command, current_revision, error)
}

/// The answer to a command that could not be made durable.
///
/// It is refused, not failed: the engine never saw it, so retrying it is safe
/// and the operator is told which command did not take effect instead of being
/// told nothing or being told it succeeded. A store that decided the refusal
/// itself (a [`CommandRefusal`], such as a LUT import that would replace a
/// bundled file) is answered with that refusal instead.
pub(crate) fn refused_submission(
    command: &CommandMessage,
    current_revision: u64,
    error: &(dyn Error + 'static),
) -> CommandSubmission {
    if let Some(refusal) = error.downcast_ref::<CommandRefusal>() {
        return rejected_submission(command, current_revision, refusal);
    }
    rejected_submission(
        command,
        current_revision,
        &CommandRefusal {
            code: RejectionCode::Unavailable,
            retryable: true,
            message: format!("command was refused because it could not be made durable: {error}"),
            reason: Some(fm_protocol::RefusalReason::NotDurable),
        },
    )
}

/// The answer to a command refused before anything about it was recorded:
/// no receipt, no revision, no events, so resubmitting it runs it afresh.
pub(crate) fn rejected_submission(
    command: &CommandMessage,
    current_revision: u64,
    refusal: &CommandRefusal,
) -> CommandSubmission {
    CommandSubmission {
        output: ResultBeforeEvents {
            result: CommandResult::Rejected {
                id: command.id.clone(),
                code: refusal.code.as_str().to_owned(),
                message: refusal.message.clone(),
                fields: Vec::new(),
                current_revision,
                retryable: refusal.retryable,
                reason: refusal.reason.clone(),
            },
            events: Vec::new(),
        },
        replayed: false,
        accepted: None,
        subscriber_failures: Vec::new(),
    }
}

/// A command refused while it was being staged, before anything about it was
/// made durable or committed to the engine.
///
/// Raised directly where the refusal is decided (a Stinger slot still
/// realizing) or derived by [`staging_refusal`] from a staging error.
use fm_daemon_journal::CommandRefusal;

/// Classifies an error raised while staging a command, before
/// `DurableStore::record`, into a refusal of that command, or returns the
/// error itself when it must stay fatal.
///
/// Every staging step only works on a preparation, which the caller drops on
/// failure, so a refusal leaves the engine, its receipts and event log, the
/// journal, and the in-memory project exactly as they were: refusing is
/// always safe for durability. What stays fatal is what says the daemon's
/// own state can no longer be trusted, so it fails closed:
///
/// * Refused, the daemon keeps running:
///   - [`CommandRefusal`]: decided where it was raised (for example a Stinger
///     slot whose desired and realized state still differ: `conflict`,
///     retryable once realization settles);
///   - the engine could not settle this command's realization to an idle
///     frame within the ticks it was given (`SnapshotError`): `conflict`,
///     retryable;
///   - the engine's scheduler could not take the command's action
///     (`EngineError::Schedule`): `resource_exhausted`, retryable;
///   - the realized switcher refused the command's action while it was
///     projected (`EngineError::RuntimeSwitcher`): `conflict`;
///   - the persisted-project schema refused the staged project
///     (`ProjectValidationError`), for example a receipt with a blank command
///     id: the command cannot be made durable, so it is `invalid_command`.
/// * Fatal, the daemon stops:
///   - an engine that cannot advance or whose authority is stale (clock,
///     tick, counter exhaustion, stale preparation, runtime identity or
///     sequencing errors): every later command would fail the same way;
///   - the stored project disagreeing with the engine about state the command
///     did not stage (a missing staged receipt, audio strip, or stream target,
///     a rename or reorder the project refuses after the engine accepted it):
///     the durable project no longer describes the engine, and anything
///     persisted from it would be corrupt.
///
/// Anything that fails after `record` returned is outside this function and
/// always fatal: the command is journaled, so it must never be reported as
/// rejected, and a restart replays it.
pub(crate) fn staging_refusal(error: Box<dyn Error>) -> Result<CommandRefusal, Box<dyn Error>> {
    let error = match error.downcast::<CommandRefusal>() {
        Ok(refusal) => return Ok(*refusal),
        Err(error) => error,
    };
    if let Some(validation) = error.downcast_ref::<fm_persistence::ProjectValidationError>() {
        return Ok(CommandRefusal {
            code: RejectionCode::InvalidCommand,
            retryable: false,
            message: format!(
                "command was refused because its result cannot be persisted: {validation}"
            ),
            reason: None,
        });
    }
    let Some(fm_control::ControlError::Engine(engine)) =
        error.downcast_ref::<fm_control::ControlError>()
    else {
        return Err(error);
    };
    let (code, retryable, reason) = match engine {
        fm_engine::EngineError::Snapshot(_) => (
            RejectionCode::Conflict,
            true,
            "its realization does not settle to an idle frame",
        ),
        fm_engine::EngineError::Schedule(_) => (
            RejectionCode::ResourceExhausted,
            true,
            "the engine scheduler cannot take its action",
        ),
        fm_engine::EngineError::RuntimeSwitcher(_) => (
            RejectionCode::Conflict,
            false,
            "the realized switcher refuses its action",
        ),
        // Clock, tick, counter exhaustion, stale preparation, and anything
        // added later: fail closed.
        _ => return Err(error),
    };
    Ok(CommandRefusal {
        code,
        retryable,
        message: format!("command was refused because {reason}: {engine}"),
        reason: None,
    })
}

/// Answers a staging failure: a refused command, or the fatal error.
pub(crate) fn refuse_staged_command(
    command: &CommandMessage,
    current_revision: u64,
    error: Box<dyn Error>,
    runtime_events: Vec<RuntimeEventMessage>,
) -> AppResult<DurableExecution> {
    let refusal = staging_refusal(error)?;
    Ok(DurableExecution {
        submission: rejected_submission(command, current_revision, &refusal),
        runtime_events,
    })
}

/// Executes `command` realizing every frame it projects at once: the
/// tests' way to drive a command to where it settles.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_durable_command(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now_millis: u64,
) -> AppResult<DurableExecution> {
    execute_durable_command_with_ticks(
        control,
        store,
        durable,
        principal,
        server,
        command,
        now_millis,
        None,
        Realize::All,
        |control, server| Ok(control.tick(server)?.runtime_events),
    )
}

/// How many of the frames an accepted command projects
/// [`execute_durable_command_with_ticks`] realizes before it returns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Realize {
    /// Every one: tests that drive a command to where it settles.
    #[cfg(test)]
    All,
    /// Only its first, so the command is realized on the frame right after
    /// its append and answered then; the frame loop realizes the rest (a
    /// timed transition, a title phase) one frame per pacer tick, serving
    /// every session in between: the native pacer, or the simulated
    /// daemon's wall-clock pacer in the session loop.
    First,
    /// None: journal replay, which ticks up to each recorded command's
    /// first frame itself (see [`replay_idle_frames`]).
    None,
}

impl Realize {
    /// The frames realized now of the `projected` an accepted (or not)
    /// command settles after. A refusal realizes nothing: it changes no
    /// realization, and projects past frames only while earlier work is
    /// in flight, which the frame loop realizes.
    pub(crate) fn now(self, projected: u64, accepted: bool) -> u64 {
        match self {
            _ if !accepted => 0,
            #[cfg(test)]
            Self::All => projected,
            Self::First => projected.min(1),
            Self::None => 0,
        }
    }
}

/// Refuses an accepted command by the project it would leave (the durable
/// project, then the staged one) before anything is recorded.
pub(crate) type ProjectVet<'a> =
    dyn Fn(&StoredProject, &StoredProject) -> Option<CommandRefusal> + 'a;

/// Stages, records, commits, and realizes one command.
///
/// `vet` may refuse an accepted command by the project it would leave;
/// nothing is staged or recorded for a refused one.
///
/// Failures before `store.record` refuse the command as classified by
/// [`staging_refusal`]; a failing `record` refuses it as undurable; failures
/// from the commit on are fatal because the command is already journaled.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn execute_durable_command_with_ticks(
    control: &mut ControlService<Policy>,
    store: &dyn DurableStore,
    durable: &mut StoredProject,
    principal: &Principal,
    server: &ServerIdentity,
    command: &CommandMessage,
    now_millis: u64,
    vet: Option<&ProjectVet<'_>>,
    realize: Realize,
    mut tick: impl FnMut(
        &mut ControlService<Policy>,
        &ServerIdentity,
    ) -> AppResult<Vec<RuntimeEventMessage>>,
) -> AppResult<DurableExecution> {
    // Every document's page count is known before the command runs, live
    // and in replay alike, so a page step resolves the same way (IN-014).
    if let Some(documents) = store.documents() {
        documents::sync(control, server, documents)?;
    }
    // Nothing commits before `record`, so a refusal reports this revision.
    let current_revision = control.diagnostics().current_revision;
    let began_frame = control.frame_cursor();
    // What a template edit leaves unchanged (shared `Arc`s, so cheap).
    let templates_before = control.title_templates();
    let preparation = match control.prepare_submit(principal, command.clone(), now_millis) {
        Ok(preparation) => preparation,
        Err(error) => {
            return refuse_staged_command(command, current_revision, error.into(), Vec::new());
        }
    };
    let prepared = match preparation {
        PrepareSubmitOutcome::Replayed(submission) => {
            return Ok(DurableExecution {
                submission,
                runtime_events: Vec::new(),
            });
        }
        PrepareSubmitOutcome::Prepared(prepared) => prepared,
    };

    // The command is durable as the project it leaves once it settles: its
    // own frames, extended until its scene and title phases end and, while
    // an earlier command's work is still in flight, until that ends (see
    // `fm_control::PreparedSubmission::settle_ticks`).
    let accepted = prepared.submission().is_accepted();
    let (ticks, projected) = prepared.project_settled(if accepted {
        command_base_ticks(command, &prepared)
    } else {
        0
    });
    // An accepted undo or redo is journaled as the edit it resolved to:
    // replay restores an engine whose session history is empty. The project
    // sync reads the same resolved command, so an undone input removal and a
    // live input addition both know the input kind they persist.
    let journaled = prepared.replay_command(command);
    let staged = projected.map_err(Into::into).and_then(|projected| {
        stored_project_from_snapshot(
            durable,
            &projected,
            command,
            &journaled,
            &prepared.output().result,
        )
    });
    let updated = match staged {
        Ok(updated) => updated,
        Err(error) => {
            // Dropping the preparation abandons it without touching the
            // engine, exactly as a failed `record` does below.
            drop(prepared);
            return refuse_staged_command(command, current_revision, error, Vec::new());
        }
    };
    if prepared.submission().is_accepted()
        && let Some(refusal) = vet.and_then(|vet| vet(durable, &updated))
    {
        drop(prepared);
        return Ok(DurableExecution {
            submission: rejected_submission(command, current_revision, &refusal),
            runtime_events: Vec::new(),
        });
    }
    // An animation edit's template is staged before its batch, so whatever
    // replays the batch finds the template the manifest it lands on needs.
    let animations = staged_title_animations(&prepared, &updated, &templates_before);
    let staged_uris: Vec<String> = animations.iter().map(|(uri, _)| uri.clone()).collect();
    let revision = updated.position().revision;
    if let Err(error) = store.stage_title_animations(revision, &animations) {
        drop(prepared);
        // A staged copy left behind would be committed by the checkpoint
        // after the next command at this revision: stop instead.
        store.discard_staged_title_animations(revision, &staged_uris)?;
        eprintln!(
            "FREEMIXD_DURABILITY\tv=1\tcommand={}\trevision={revision}\terror={error}",
            command.id
        );
        return Ok(DurableExecution {
            submission: refused_submission(
                command,
                control.diagnostics().current_revision,
                error.as_ref(),
            ),
            runtime_events: Vec::new(),
        });
    }
    // Durability comes first: the mutation survives a crash before the engine
    // commits it, before its events are published, and before the acceptance is
    // returned to the caller for acknowledgement.
    if let Err(error) = store.record(&journaled, now_millis, began_frame, durable, &updated) {
        store.discard_staged_title_animations(revision, &staged_uris)?;
        // Nothing was made durable, so nothing may be acknowledged. Dropping
        // the preparation abandons it without touching the engine: no revision,
        // no receipt, no events, so the same command may simply be sent again.
        drop(prepared);
        eprintln!(
            "FREEMIXD_DURABILITY\tv=1\tcommand={}\trevision={}\terror={error}",
            command.id,
            updated.position().revision
        );
        return Ok(DurableExecution {
            submission: refused_submission(
                command,
                control.diagnostics().current_revision,
                error.as_ref(),
            ),
            runtime_events: Vec::new(),
        });
    }

    let submission = prepared.commit()?;
    // A document the command added or replaced reports its pages now.
    if let Some(documents) = store.documents() {
        documents::sync(control, server, documents)?;
    }
    let mut runtime_events = Vec::new();
    for _ in 0..realize.now(ticks, accepted) {
        runtime_events.extend(tick(control, server)?);
    }
    // The project as it settles: a command whose later frames the native
    // frame loop realizes is durable (and checkpointed) as it ends.
    *durable = updated;

    Ok(DurableExecution {
        submission,
        runtime_events,
    })
}

/// The templates an accepted command leaves `updated` with: one per
/// `title_animation_changed` or `title_exposure_changed` event, keyed by the
/// stored template URI its titles name. A staged template is whole, so the
/// part the event does not change (the hidden fields of an animation edit,
/// the animation of an exposure edit) comes from `before`, the engine's
/// templates as the command found them.
pub(crate) fn staged_title_animations(
    prepared: &fm_control::PreparedSubmission<'_, Policy>,
    updated: &StoredProject,
    before: &[fm_engine::EngineTitleTemplate],
) -> Vec<(String, journal::StagedTemplate)> {
    if !prepared.submission().is_accepted() {
        return Vec::new();
    }
    let template = |uri: &str| {
        before
            .iter()
            .find(|template| &*template.key == uri)
            .map(|template| &template.template)
    };
    let uri_of = |inputs: &[fm_protocol::WireInputId]| {
        title_render::title_of_input(updated.project(), inputs.first()?.to_domain())
            .map(|title| title.template_uri.clone())
    };
    prepared
        .output()
        .events
        .iter()
        .filter_map(|event| match &event.payload {
            fm_protocol::EventPayload::TitleAnimationChanged {
                inputs,
                animation_revision,
                clip,
            } => {
                let uri = uri_of(inputs)?;
                let clip = fm_control::title_clip_from_wire(clip).ok()?;
                let hidden = template(&uri).map_or_else(Vec::new, |template| {
                    template
                        .fields
                        .iter()
                        .filter(|field| !field.exposed)
                        .map(|field| field.id)
                        .collect()
                });
                let animation = fm_titles::TitleAnimation::new(clip, *animation_revision);
                Some((uri, journal::StagedTemplate { animation, hidden }))
            }
            fm_protocol::EventPayload::TitleExposureChanged { inputs, fields } => {
                let uri = uri_of(inputs)?;
                let animation = template(&uri)?.animation.clone();
                let hidden = fields
                    .iter()
                    .filter(|item| !item.exposed)
                    .map(|item| fm_titles::FieldId::new(item.field.get()))
                    .collect();
                Some((uri, journal::StagedTemplate { animation, hidden }))
            }
            _ => None,
        })
        .collect()
}

/// The frames an accepted command realizes on its own: its transition,
/// title phase, or target frame. Its projection extends them until every
/// scene and title phase it started has played (GX-011 stage 5) and any
/// work in flight before it has ended (see
/// `fm_control::PreparedSubmission::settle_ticks`).
#[allow(clippy::too_many_lines)] // one arm per command payload
pub(crate) fn command_base_ticks(
    command: &CommandMessage,
    prepared: &fm_control::PreparedSubmission<'_, Policy>,
) -> u64 {
    // A playlist command may target a later frame and start a Program
    // transition there.
    if matches!(
        command.payload,
        CommandPayload::SetPlaylist { .. }
            | CommandPayload::PlaylistControl { .. }
            | CommandPayload::PlaylistAdvance { .. }
    ) {
        return prepared.playlist_ticks();
    }
    // A cue operation or script run may target a later frame and start
    // transitions.
    if matches!(
        command.payload,
        CommandPayload::CueControl { .. }
            | CommandPayload::CueAdvance { .. }
            | CommandPayload::RunScript { .. }
            | CommandPayload::FireShortcut { .. }
    ) {
        return prepared.cue_ticks();
    }
    u64::from(match command.payload {
        CommandPayload::Fade { duration_frames }
        | CommandPayload::AlphaFade { duration_frames }
        | CommandPayload::Slide { duration_frames }
        | CommandPayload::Zoom { duration_frames }
        | CommandPayload::Wipe { duration_frames }
        | CommandPayload::Transition {
            duration_frames, ..
        }
        | CommandPayload::FadeToBlack {
            duration_frames, ..
        } => duration_frames,
        CommandPayload::Stinger {
            duration_frames, ..
        } => {
            if prepared.project(1).is_ok() {
                1
            } else {
                duration_frames
            }
        }
        CommandPayload::TakeOverlay { channel, .. }
        | CommandPayload::OverlayOff { channel }
        | CommandPayload::TakeNextOverlay { channel } => {
            let channel =
                OverlayChannelId::new(channel.number()).unwrap_or(OverlayChannelId::ALL[0]);
            prepared.overlay_transition_ticks(channel)
        }
        // A title take realizes its whole In phase, as a fade realizes its
        // whole duration.
        CommandPayload::TakeTitle { input, channel, .. } => {
            let channel =
                OverlayChannelId::new(channel.number()).unwrap_or(OverlayChannelId::ALL[0]);
            prepared.title_take_ticks(input.to_domain(), channel)
        }
        CommandPayload::TitleOffAir { input } => prepared.title_off_air_ticks(input.to_domain()),
        CommandPayload::RenameInput { .. }
        | CommandPayload::SetTitleFields { .. }
        | CommandPayload::ReorderInputs { .. }
        | CommandPayload::SetInputAudioStrip { .. }
        | CommandPayload::SelectPreview { .. }
        | CommandPayload::Cut
        | CommandPayload::ConfigureStinger { .. }
        | CommandPayload::RemoveStinger { .. }
        | CommandPayload::SetTransitionFavorite { .. }
        | CommandPayload::Media { .. }
        | CommandPayload::PageNavigate { .. }
        | CommandPayload::MediaList { .. }
        | CommandPayload::InputLifecycle(_)
        | CommandPayload::SceneLayer(_)
        | CommandPayload::AudioMixer(_)
        | CommandPayload::SetFadeToBlackOutput { .. }
        | CommandPayload::SetFadeToBlackOutputDuration { .. }
        | CommandPayload::SetFadeToBlackCurve { .. }
        | CommandPayload::SetDisplayOutput { .. }
        | CommandPayload::ClearDisplayOutput { .. }
        | CommandPayload::SetLiveLan { .. }
        | CommandPayload::SetAlertRule(_)
        | CommandPayload::Replay(_)
        | CommandPayload::Reel(_)
        | CommandPayload::SetNetworkStream { .. }
        | CommandPayload::SetCaptureSettings { .. }
        | CommandPayload::UpdateOverlay { .. }
        | CommandPayload::SetOverlayOutputInclusion { .. }
        | CommandPayload::ConfigureOverlayTransition { .. }
        | CommandPayload::ConfigureOverlayAppearance { .. }
        | CommandPayload::QueueOverlay { .. }
        | CommandPayload::StartManualTransition { .. }
        | CommandPayload::SetManualTransitionPosition { .. }
        | CommandPayload::CommitManualTransition
        | CommandPayload::CancelManualTransition
        | CommandPayload::StreamStart { .. }
        | CommandPayload::StreamStop { .. }
        | CommandPayload::StreamRetry { .. }
        | CommandPayload::StreamStartAll
        | CommandPayload::StreamStopAll
        | CommandPayload::SetStreamTarget { .. }
        | CommandPayload::RemoveStreamTarget { .. }
        | CommandPayload::RecordStart { .. }
        | CommandPayload::RecordStop { .. }
        | CommandPayload::Undo { .. }
        | CommandPayload::Redo { .. }
        | CommandPayload::SetEditLock { .. }
        | CommandPayload::AddInput { .. }
        | CommandPayload::RemoveInput { .. }
        | CommandPayload::SetPlaylist { .. }
        | CommandPayload::PlaylistControl { .. }
        | CommandPayload::PlaylistAdvance { .. }
        | CommandPayload::SetCueList { .. }
        | CommandPayload::RemoveCueList { .. }
        | CommandPayload::CueControl { .. }
        | CommandPayload::CueAdvance { .. }
        | CommandPayload::SetScript { .. }
        | CommandPayload::RemoveScript { .. }
        | CommandPayload::RunScript { .. }
        | CommandPayload::StopScript { .. }
        | CommandPayload::SetTitleAnimation { .. }
        | CommandPayload::SetTitleFieldExposure { .. }
        | CommandPayload::ApplyTitleAnimationPreset { .. }
        | CommandPayload::StretchTitleAnimation { .. }
        | CommandPayload::SetSceneAnimation { .. }
        | CommandPayload::ApplySceneAnimationPreset { .. }
        | CommandPayload::StretchSceneAnimation { .. }
        | CommandPayload::SetInputGrade { .. }
        | CommandPayload::SetLayerGrade { .. }
        | CommandPayload::SetProgramGrade { .. }
        | CommandPayload::AddLibraryGrade { .. }
        | CommandPayload::UpdateLibraryGrade { .. }
        | CommandPayload::RemoveLibraryGrade { .. }
        | CommandPayload::ImportGradeLut { .. }
        | CommandPayload::RemoveGradeLut { .. }
        | CommandPayload::AddAnimationPreset { .. }
        | CommandPayload::RenameAnimationPreset { .. }
        | CommandPayload::RemoveAnimationPreset { .. }
        | CommandPayload::ApplyTitleAnimationUserPreset { .. }
        | CommandPayload::ApplySceneAnimationUserPreset { .. }
        | CommandPayload::SetShortcut { .. }
        | CommandPayload::RemoveShortcut { .. }
        | CommandPayload::FireShortcut { .. } => 1,
    })
}

pub(crate) fn stored_project_from_snapshot(
    durable: &StoredProject,
    snapshot: &EngineSnapshot,
    command: &CommandMessage,
    journaled: &CommandMessage,
    result: &CommandResult,
) -> AppResult<StoredProject> {
    let mut receipts = snapshot
        .receipts()
        .iter()
        .map(persisted_engine_receipt)
        .collect::<Vec<_>>();

    if !receipts
        .iter()
        .any(|receipt| receipt.key() == command.idempotency_key)
    {
        // Refused before the engine: an authorization denial, or an edit
        // carrying a hidden title value marker (Protocol 2.104).
        if !matches!(
            result,
            CommandResult::Rejected { code, .. }
                if code == "permission_denied" || code == "invalid_command"
        ) {
            return Err(AppFailure(
                "projected engine snapshot is missing the staged command receipt".into(),
            )
            .into());
        }
        receipts.push(persisted_result(command, result)?);
    }

    stored_project_with_receipts(
        durable,
        snapshot,
        retained_receipts(receipts, Some(&command.idempotency_key)),
        Some(journaled),
    )
}

pub(crate) fn stored_project_checkpoint(
    durable: &StoredProject,
    snapshot: &EngineSnapshot,
) -> AppResult<StoredProject> {
    stored_project_with_receipts(
        durable,
        snapshot,
        retained_receipts(
            snapshot
                .receipts()
                .iter()
                .map(persisted_engine_receipt)
                .collect(),
            None,
        ),
        None,
    )
}

/// Keeps the newest [`MAX_PERSISTED_RECEIPTS`] receipts.
///
/// Recency is the revision a receipt was recorded at; a rejection recorded at
/// revision `r` happened after the command that produced `r`, so it ranks just
/// above that accepted receipt. Ties break by key so retention is
/// deterministic. The receipt for `pinned` (the command being made durable)
/// is always kept, so its answer stays replayable after a restart.
pub(crate) fn retained_receipts(
    mut receipts: Vec<IdempotencyReceipt>,
    pinned: Option<&str>,
) -> Vec<IdempotencyReceipt> {
    if receipts.len() <= MAX_PERSISTED_RECEIPTS {
        return receipts;
    }
    receipts.sort_by(|left, right| {
        receipt_recency(left, pinned)
            .cmp(&receipt_recency(right, pinned))
            .then_with(|| left.key().cmp(right.key()))
    });
    receipts.split_off(receipts.len() - MAX_PERSISTED_RECEIPTS)
}

pub(crate) fn receipt_recency(
    receipt: &IdempotencyReceipt,
    pinned: Option<&str>,
) -> (bool, u64, bool) {
    let (revision, rejected) = match receipt.outcome() {
        ReceiptOutcome::Accepted { revision, .. } => (*revision, false),
        ReceiptOutcome::Rejected {
            current_revision, ..
        } => (*current_revision, true),
    };
    (pinned == Some(receipt.key()), revision, rejected)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn stored_project_with_receipts(
    durable: &StoredProject,
    snapshot: &EngineSnapshot,
    receipts: Vec<IdempotencyReceipt>,
    command: Option<&CommandMessage>,
) -> AppResult<StoredProject> {
    let show = snapshot.show();
    let desired = show.desired_switcher();
    let realized = snapshot.realized_switcher();
    let mut project = durable.project().clone();
    project.set_main_mix(MainMix::new(desired.program(), desired.preview()));
    sync_project_inputs(&mut project, show, command)?;
    if project
        .inputs()
        .iter()
        .map(|input| input.id)
        .ne(show.inputs().iter().copied())
    {
        project.reorder_inputs(show.inputs())?;
    }
    for (&input, name) in show.inputs().iter().zip(show.input_names()) {
        if project
            .inputs()
            .iter()
            .find(|candidate| candidate.id == input)
            .is_none_or(|candidate| candidate.name != *name)
        {
            project.rename_input(input, name.clone())?;
        }
    }
    for (&input, &state) in show.input_audio_strips() {
        if !project.set_input_audio_strip(input, model_audio_strip_state(state)) {
            return Err(
                AppFailure(format!("project is missing audio strip for input {input}")).into(),
            );
        }
    }
    sync_project_stingers(&mut project, desired, realized)?;
    sync_project_streams(&mut project, show)?;
    sync_project_recording(&mut project, desired);
    sync_project_edit_locks(&mut project, show)?;
    sync_project_titles(&mut project, show)?;
    sync_project_scenes(&mut project, show)?;
    sync_project_input_lifecycle(&mut project, show)?;
    sync_project_audio_mixer(&mut project, show)?;
    sync_project_display_outputs(&mut project, show)?;
    project.set_playlist(fm_engine::playlist_to_model(show.playlist()));
    project.set_cue_lists(show.cue_lists().to_vec());
    project.set_scripts(show.scripts().to_vec());
    project.set_script_run(show.script_run().clone());
    project.set_shortcuts(show.shortcuts().to_vec());
    project.set_media_lists(show.media_lists().clone());
    project.set_title_timelines(snapshot.title_timelines().clone());
    project.set_grading(show.grading().clone());
    project.set_animation_presets(show.animation_presets().clone());
    project.set_transition_favorites(*show.transition_favorites());
    for (&input, settings) in show.media_inputs() {
        if project.media_playback(input) != Some(settings) {
            project
                .set_media_playback(input, *settings)
                .map_err(|error| {
                    AppFailure(format!(
                        "project cannot take media input {input}'s playback: {error}"
                    ))
                })?;
        }
    }
    // A document's page lives beside its asset (IN-014).
    for (&input, &page) in show.paged_inputs() {
        if project
            .document(input)
            .is_some_and(|document| document.page != page)
            && !project.set_document_page(input, page)
        {
            return Err(
                AppFailure(format!("project cannot take document input {input}'s page")).into(),
            );
        }
    }
    // Capture settings live beside the device source (IN-001).
    for (input, _, settings) in show.capture_inputs() {
        if project.capture_input(input).map(|(_, current)| current) != Some(settings) {
            project
                .set_capture_settings(input, settings.clone())
                .map_err(|error| {
                    AppFailure(format!(
                        "project cannot take capture input {input}'s settings: {error}"
                    ))
                })?;
        }
    }
    StoredProject::from_project_with_complete_runtime_state(
        project,
        RuntimeRouting {
            desired_program_id: Some(desired.program()),
            realized_program_id: Some(realized.program()),
            desired_preview_id: Some(desired.preview()),
            realized_preview_id: Some(realized.preview()),
        },
        RuntimeManualTransitions {
            desired: desired.t_bar().map(persisted_t_bar),
            realized: realized.t_bar().map(persisted_t_bar),
        },
        RuntimeFadeToBlack {
            desired: persisted_fade_to_black(snapshot.desired_fade_to_black()),
            realized: persisted_fade_to_black(snapshot.realized_fade_to_black()),
        },
        persisted_overlays(desired, realized),
        snapshot_position(snapshot),
        receipts,
    )
    .map_err(Into::into)
}
