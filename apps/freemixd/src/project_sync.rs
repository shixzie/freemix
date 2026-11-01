//! Syncing the engine back into the project: input references, lifecycle,
//! streams, recording, edit locks, titles, overlays, stingers, Fade to Black,
//! and the T-bar.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(unused_imports, clippy::wildcard_imports)]
// the binary items, some of them native only
use super::*;

/// The typed usage of a persisted project reference that keeps an input
/// from being removed.
pub(crate) fn input_reference_use(reference: fm_model::InputReference) -> fm_protocol::InputUse {
    use fm_model::InputReference;
    use fm_protocol::InputUse;
    match reference {
        InputReference::Program => InputUse::Program,
        InputReference::Preview => InputUse::Preview,
        InputReference::Stinger(slot) => InputUse::Stinger {
            slot: slot.number(),
        },
        InputReference::SceneLayer(scene) => InputUse::SceneLayer {
            scene: scene.get().get(),
        },
        InputReference::SceneAudioSource(scene_input) => InputUse::SceneAudioSource {
            scene_input: scene_input.get().get(),
        },
        InputReference::AliasTarget(alias) => InputUse::AliasTarget {
            alias: alias.get().get(),
        },
        InputReference::PlaylistEntry(entry) => InputUse::PlaylistEntry { entry },
        InputReference::CueAction { list, cue } => InputUse::CueAction { list, cue },
        // The wire has no reel-clip use yet; a reel angle is recorded for
        // replay, so report the closest existing use rather than widening
        // the protocol in the model-only slice.
        InputReference::ReplaySource | InputReference::ReelClip { .. } => InputUse::ReplaySource,
    }
}

/// Folds a live input addition or removal into the stored project.
///
/// The engine's show owns the input catalogue but not the input kinds, so an
/// addition reads its kind from the command being made durable (always a
/// journaled `input_add`, also when an undo resolved to it). A removal is
/// checked against the persisted references the engine does not model, Scene
/// layers, Scene audio sources and alias targets, and refused with the reason
/// when one still names the input. Any other divergence means the show and the
/// project disagree, which is fatal.
/// The refusal for removing an input the stored project still references.
pub(crate) fn input_in_use_refusal(
    input: InputId,
    reference: fm_model::InputReference,
) -> CommandRefusal {
    CommandRefusal {
        code: RejectionCode::Conflict,
        retryable: false,
        message: format!("input {input} cannot be removed: it is {reference}"),
        reason: Some(fm_protocol::RefusalReason::InputInUse {
            input: input.get().get(),
            usage: input_reference_use(reference),
        }),
    }
}

#[allow(clippy::too_many_lines)] // one arm per added input source
pub(crate) fn sync_project_inputs(
    project: &mut fm_model::Project,
    show: &ShowState,
    command: Option<&CommandMessage>,
) -> AppResult<()> {
    let removed = project
        .inputs()
        .iter()
        .map(|input| input.id)
        .filter(|input| !show.inputs().contains(input))
        .collect::<Vec<_>>();
    for input in removed {
        if let Some(reference) = project.input_reference(input) {
            return Err(input_in_use_refusal(input, reference).into());
        }
        project
            .remove_input(input)
            .map_err(|error| AppFailure(format!("input {input} could not be removed: {error}")))?;
    }
    let added = show
        .inputs()
        .iter()
        .zip(show.input_names())
        .filter(|(input, _)| !project.inputs().iter().any(|known| known.id == **input))
        .map(|(&input, name)| (input, name.clone()))
        .collect::<Vec<_>>();
    for (input, name) in added {
        let source = match command.map(|command| &command.payload) {
            Some(CommandPayload::AddInput {
                input: added,
                source,
                ..
            }) if added.to_domain() == input => source.clone(),
            Some(CommandPayload::InputLifecycle(
                fm_protocol::InputLifecycleOperation::Duplicate {
                    source,
                    input: added,
                    ..
                },
            )) if added.to_domain() == input => {
                project
                    .duplicate_input(source.to_domain(), input, name)
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be duplicated: {error}"))
                    })?;
                continue;
            }
            // A media list input (IN-006), created or recreated by a redo:
            // a media input playing its active item.
            Some(
                CommandPayload::InputLifecycle(fm_protocol::InputLifecycleOperation::Import {
                    ..
                })
                | CommandPayload::MediaList { .. }
                | CommandPayload::Redo { .. },
            ) if show.media_list(input).is_some()
                || matches!(
                    command.map(|command| &command.payload),
                    Some(CommandPayload::InputLifecycle(
                        fm_protocol::InputLifecycleOperation::Import { .. }
                    ))
                ) =>
            {
                let source = show
                    .input_source(input)
                    .ok_or_else(|| AppFailure(format!("imported input {input} has no source")))?;
                let playback = show.media_settings(input).copied().unwrap_or_default();
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: source.to_kind(playback),
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be imported: {error}"))
                    })?;
                continue;
            }
            _ => {
                return Err(AppFailure(format!(
                    "projected engine snapshot adds input {input} without an input_add command"
                ))
                .into());
            }
        };
        let video = match source {
            fm_protocol::LiveInputSource::Solid {
                red,
                green,
                blue,
                alpha,
            } => {
                fm_model::SimulatedVideo::Solid(fm_model::SolidColor::new(red, green, blue, alpha))
            }
            fm_protocol::LiveInputSource::Bars => fm_model::SimulatedVideo::Bars,
            fm_protocol::LiveInputSource::NetworkStream(config) => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::NetworkStream(*config),
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
            fm_protocol::LiveInputSource::Capture(config) => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::Device {
                            stable_key: config.stable_key,
                            capture: config.settings,
                        },
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
            fm_protocol::LiveInputSource::SystemAudio(config) => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::SystemAudio(*config),
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
            fm_protocol::LiveInputSource::Image(config) => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::Image(*config),
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
            fm_protocol::LiveInputSource::Slideshow(config) => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::Slideshow(*config),
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
            fm_protocol::LiveInputSource::ScreenCapture(config) => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::ScreenCapture(*config),
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
            fm_protocol::LiveInputSource::Replay => {
                project
                    .add_input_checked(fm_model::Input {
                        id: input,
                        name,
                        kind: fm_model::InputKind::Replay,
                        required_capabilities: Vec::new(),
                    })
                    .map_err(|error| {
                        AppFailure(format!("input {input} could not be added: {error}"))
                    })?;
                continue;
            }
        };
        project
            .add_input_checked(fm_model::Input {
                id: input,
                name,
                kind: fm_model::InputKind::Simulated(fm_model::SimulatedInput::new(
                    video,
                    fm_model::SimulatedAudio::Silence,
                )),
                required_capabilities: Vec::new(),
            })
            .map_err(|error| AppFailure(format!("input {input} could not be added: {error}")))?;
    }
    Ok(())
}

/// Folds replaced input sources, the category list, and every input's
/// lifecycle into the stored project (IN-026). The engine's show owns all
/// three; a source the project does not hold is a replacement.
pub(crate) fn sync_project_input_lifecycle(
    project: &mut fm_model::Project,
    show: &ShowState,
) -> AppResult<()> {
    for (&input, source) in show.input_sources() {
        let Some(current) = project
            .inputs()
            .iter()
            .find(|candidate| candidate.id == input)
        else {
            continue;
        };
        if fm_model::InputSourceSpec::from_kind(&current.kind).as_ref() == Some(source) {
            continue;
        }
        let playback = show.media_settings(input).copied().unwrap_or_default();
        let capabilities = current.required_capabilities.clone();
        project
            .replace_input_source(input, source.to_kind(playback), capabilities)
            .map_err(|error| {
                AppFailure(format!(
                    "input {input}'s source could not be replaced: {error}"
                ))
            })?;
    }
    let stale = project
        .input_lifecycles()
        .keys()
        .copied()
        .filter(|input| !show.input_lifecycles().contains_key(input))
        .collect::<Vec<_>>();
    for input in stale {
        project.restore_input_lifecycle(input, fm_model::InputLifecycle::default());
    }
    project.restore_input_categories(show.input_categories().to_vec());
    for (&input, &lifecycle) in show.input_lifecycles() {
        project.restore_input_lifecycle(input, lifecycle);
    }
    Ok(())
}

/// Folds the show's stream destinations and their desired running flags into
/// the stored project.
///
/// StreamStart/StreamStop, the group start/stop, and destination edits
/// (OR-006) are engine commands, so the projected show is the authority for
/// both the configurations and the desired switcher's running flags;
/// mirroring them here makes an accepted command survive restart through
/// journal replay and checkpoint, exactly as renames and audio strips do.
/// Removed destinations leave first, then every remaining one is replaced or
/// appended in the show's order.
pub(crate) fn sync_project_streams(
    project: &mut fm_model::Project,
    show: &ShowState,
) -> AppResult<()> {
    let desired = show.desired_switcher();
    let stale = project
        .stream_targets()
        .iter()
        .map(fm_model::StreamTarget::id)
        .filter(|id| {
            !show
                .stream_targets()
                .iter()
                .any(|target| target.id() == *id)
        })
        .collect::<Vec<_>>();
    for target_id in stale {
        project.remove_stream_target(target_id).map_err(|error| {
            AppFailure(format!(
                "stream target {target_id} could not be removed: {error}"
            ))
        })?;
    }
    for target in show.stream_targets() {
        let target_id = SwitcherStreamTargetId::from_non_zero(target.id().get());
        let running = desired.stream_running(target_id).ok_or_else(|| {
            AppFailure(format!(
                "projected engine snapshot is missing stream target {target_id}"
            ))
        })?;
        let target = target.clone().set_running(running);
        let target_id = target.id();
        match project
            .stream_targets()
            .iter()
            .find(|existing| existing.id() == target_id)
        {
            Some(existing) if *existing == target => {}
            Some(_) => project.replace_stream_target(target).map_err(|error| {
                AppFailure(format!(
                    "stream target {target_id} could not be persisted: {error}"
                ))
            })?,
            None => project.add_stream_target_checked(target).map_err(|error| {
                AppFailure(format!(
                    "stream target {target_id} could not be persisted: {error}"
                ))
            })?,
        }
    }
    // The show's order is authoritative: an undone removal comes back at its
    // old place, not at the end where an add appends it.
    for (index, target) in show.stream_targets().iter().enumerate() {
        if project
            .stream_targets()
            .get(index)
            .map(fm_model::StreamTarget::id)
            != Some(target.id())
        {
            project
                .move_stream_target(target.id(), index)
                .map_err(|error| {
                    AppFailure(format!(
                        "stream target {} could not be reordered: {error}",
                        target.id()
                    ))
                })?;
        }
    }
    Ok(())
}

/// Folds both slots' desired recording flags into the stored project before
/// the accepted command is acknowledged.
///
/// RecordStart/RecordStop are engine commands, so the projected snapshot's
/// desired switcher is the authority for every slot; mirroring the whole
/// roster here makes an accepted command survive restart through journal
/// replay and checkpoint, exactly as the stream roster does. A slot the
/// command did not address keeps its previously projected flag, so one
/// slot's start never rewrites the other's stop.
pub(crate) fn sync_project_recording(project: &mut fm_model::Project, desired: &SwitcherState) {
    for slot in ModelRecorderSlot::ALL {
        let active = desired.recording_desired(switcher_recorder_slot(slot));
        project.set_recording_desired(slot, active);
    }
}

/// Folds the engine's edit padlock into the stored project.
///
/// `SetEditLock` is a durable engine command, so the projected show is the
/// authority for both the project flag and the per-input set; mirroring the
/// whole state makes an accepted lock change survive restart through journal
/// replay and checkpoint alike.
pub(crate) fn sync_project_edit_locks(
    project: &mut fm_model::Project,
    show: &ShowState,
) -> AppResult<()> {
    project.set_project_locked(show.is_project_locked());
    let stale = project
        .locked_inputs()
        .difference(show.locked_inputs())
        .copied()
        .collect::<Vec<_>>();
    for input in stale {
        project.set_input_locked(input, false);
    }
    for &input in show.locked_inputs() {
        project
            .set_input_locked_checked(input, true)
            .map_err(|error| AppFailure(format!("edit lock could not be persisted: {error}")))?;
    }
    Ok(())
}

/// Persists every title's live field values and revisions (the title's and each field's).
pub(crate) fn sync_project_titles(
    project: &mut fm_model::Project,
    show: &ShowState,
) -> AppResult<()> {
    for (&input, title) in show.titles() {
        let fields = title
            .fields
            .values()
            .iter()
            .map(|(field, value)| fm_model::TitleFieldEntry {
                id: fm_model::TitleFieldId::new(field.get()),
                value: title_render::model_field_value(value),
                revision: title.fields.field_revision(*field).unwrap_or(0),
            })
            .collect();
        project
            .set_title_fields(input, title.fields.revision(), fields)
            .map_err(|error| {
                AppFailure(format!(
                    "title input {input} fields could not be persisted: {error:?}"
                ))
            })?;
    }
    Ok(())
}

pub(crate) fn persisted_overlays(
    desired: &SwitcherState,
    realized: &SwitcherState,
) -> RuntimeOverlays {
    RuntimeOverlays {
        desired: std::array::from_fn(|index| persisted_overlay(&desired.overlays()[index])),
        realized: std::array::from_fn(|index| persisted_overlay(&realized.overlays()[index])),
    }
}

pub(crate) fn persisted_overlay(channel: &OverlayChannelState) -> RuntimeOverlayChannel {
    RuntimeOverlayChannel {
        source: channel.source(),
        active: channel.is_active(),
        transition: RuntimeOverlayTransition::from_token(
            fm_control::wire_overlay_transition(channel.transition()).token(),
        )
        .unwrap_or_default(),
        duration_frames: channel.duration_frames(),
        position: persisted_overlay_position(channel.position()),
        border: persisted_overlay_border(channel.border()),
        queued_sources: channel.queued_sources().to_vec(),
        included_outputs: channel.included_outputs().to_vec(),
    }
}

pub(crate) fn persisted_overlay_position(
    position: OverlayPositionPreset,
) -> RuntimeOverlayPosition {
    match position {
        OverlayPositionPreset::FullFrame => RuntimeOverlayPosition::FullFrame,
        OverlayPositionPreset::TopLeft => RuntimeOverlayPosition::TopLeft,
        OverlayPositionPreset::TopRight => RuntimeOverlayPosition::TopRight,
        OverlayPositionPreset::BottomLeft => RuntimeOverlayPosition::BottomLeft,
        OverlayPositionPreset::BottomRight => RuntimeOverlayPosition::BottomRight,
    }
}

pub(crate) fn restored_overlay_position(position: RuntimeOverlayPosition) -> OverlayPositionPreset {
    match position {
        RuntimeOverlayPosition::FullFrame => OverlayPositionPreset::FullFrame,
        RuntimeOverlayPosition::TopLeft => OverlayPositionPreset::TopLeft,
        RuntimeOverlayPosition::TopRight => OverlayPositionPreset::TopRight,
        RuntimeOverlayPosition::BottomLeft => OverlayPositionPreset::BottomLeft,
        RuntimeOverlayPosition::BottomRight => OverlayPositionPreset::BottomRight,
    }
}

pub(crate) fn persisted_overlay_border(border: OverlayBorderPreset) -> RuntimeOverlayBorder {
    match border {
        OverlayBorderPreset::None => RuntimeOverlayBorder::None,
        OverlayBorderPreset::ThinWhite => RuntimeOverlayBorder::ThinWhite,
        OverlayBorderPreset::ThickWhite => RuntimeOverlayBorder::ThickWhite,
    }
}

pub(crate) fn restored_overlay_border(border: RuntimeOverlayBorder) -> OverlayBorderPreset {
    match border {
        RuntimeOverlayBorder::None => OverlayBorderPreset::None,
        RuntimeOverlayBorder::ThinWhite => OverlayBorderPreset::ThinWhite,
        RuntimeOverlayBorder::ThickWhite => OverlayBorderPreset::ThickWhite,
    }
}

pub(crate) fn restore_overlays(
    show: &mut ShowState,
    realized: &mut SwitcherState,
    overlays: &RuntimeOverlays,
) -> AppResult<()> {
    for (index, (desired, realized_state)) in
        overlays.desired.iter().zip(&overlays.realized).enumerate()
    {
        let Some(channel) = OverlayChannelId::from_index(index) else {
            continue;
        };
        restore_overlay(show.desired_switcher_mut(), channel, desired)?;
        restore_overlay(realized, channel, realized_state)?;
    }
    Ok(())
}

pub(crate) fn restore_overlay(
    switcher: &mut SwitcherState,
    channel: OverlayChannelId,
    state: &RuntimeOverlayChannel,
) -> AppResult<()> {
    switcher.configure_overlay_transition(
        channel,
        fm_control::domain_overlay_transition(
            fm_protocol::OverlayTransitionKind::from_token(state.transition.token())
                .unwrap_or_default(),
        ),
        state.duration_frames,
    )?;
    let _ = switcher.configure_overlay_appearance(
        channel,
        restored_overlay_position(state.position),
        restored_overlay_border(state.border),
    );
    if let Some(source) = state.source {
        if state.active {
            switcher.take_overlay(channel, source)?;
        } else {
            switcher.update_overlay(channel, source)?;
        }
    }
    for source in &state.queued_sources {
        switcher.queue_overlay(channel, *source)?;
    }
    for output in &state.included_outputs {
        let _ = switcher.set_overlay_output_inclusion(channel, *output, true);
    }
    Ok(())
}

pub(crate) fn sync_project_stingers(
    project: &mut fm_model::Project,
    desired: &SwitcherState,
    realized: &SwitcherState,
) -> AppResult<()> {
    let Ok(last) = u8::try_from(StingerSlotNumber::COUNT) else {
        return Ok(());
    };
    for number in 1..=last {
        let (Some(model_slot), Some(slot)) =
            (StingerSlotNumber::new(number), StingerSlotId::new(number))
        else {
            continue;
        };
        let desired_state = desired.stinger(slot);
        if desired_state != realized.stinger(slot) {
            // Realization has not caught up with the desired slot yet. That
            // is runtime state, not corruption: refuse the command so it can
            // be retried once the slot settles, instead of persisting a slot
            // that was never realized.
            return Err(CommandRefusal {
                code: RejectionCode::Conflict,
                retryable: true,
                message: format!(
                    "command was refused because Stinger slot {number} has not finished \
                     realizing; retry once it has"
                ),
                reason: Some(fm_protocol::RefusalReason::StingerRealizing { slot: number }),
            }
            .into());
        }
        let _ = project.remove_stinger(model_slot);
        let Some(descriptor) = desired_state.descriptor() else {
            continue;
        };
        project.set_stinger(StingerConfig::new(
            model_slot,
            descriptor.media_input,
            descriptor.preload,
            descriptor.cut_point_frames,
            match descriptor.audio_policy {
                StingerAudioPolicy::Muted => ModelStingerAudioPolicy::Muted,
                StingerAudioPolicy::StingerOnly => ModelStingerAudioPolicy::StingerOnly,
                StingerAudioPolicy::MixWithProgram => ModelStingerAudioPolicy::MixWithProgram,
            },
            match descriptor.missing_media_fallback {
                MissingMediaFallback::Cut => StingerMissingMediaFallback::Cut,
                MissingMediaFallback::Fade => StingerMissingMediaFallback::Fade,
                MissingMediaFallback::KeepProgram => StingerMissingMediaFallback::KeepProgram,
            },
        ));
    }
    Ok(())
}

pub(crate) fn persisted_fade_to_black(
    state: fm_engine::EngineFadeToBlackState,
) -> PersistedFadeToBlackState {
    PersistedFadeToBlackState::new(
        state.active,
        u16::try_from(state.position.numerator()).unwrap_or(u16::MAX),
    )
}

pub(crate) fn persisted_t_bar(state: TBarState) -> PersistedManualTransitionState {
    let kind = match state.kind() {
        TransitionKind::Fade => PersistedManualTransitionKind::Fade,
        TransitionKind::Wipe => PersistedManualTransitionKind::Wipe,
        TransitionKind::AlphaFade => PersistedManualTransitionKind::AlphaFade,
        TransitionKind::Slide => PersistedManualTransitionKind::Slide,
        TransitionKind::VerticalWipe => PersistedManualTransitionKind::VerticalWipe,
        TransitionKind::VerticalSlide => PersistedManualTransitionKind::VerticalSlide,
        TransitionKind::Zoom => PersistedManualTransitionKind::Zoom,
        TransitionKind::Fly => PersistedManualTransitionKind::Fly,
        TransitionKind::FlyRotate => PersistedManualTransitionKind::FlyRotate,
        TransitionKind::Cube => PersistedManualTransitionKind::Cube,
        TransitionKind::CubeZoom => PersistedManualTransitionKind::CubeZoom,
        TransitionKind::CrossZoom => PersistedManualTransitionKind::CrossZoom,
        TransitionKind::Merge => PersistedManualTransitionKind::Merge,
        TransitionKind::Stinger(slot) => PersistedManualTransitionKind::Stinger {
            slot: slot.number(),
            clip_frames: state.clip_frames(),
        },
    };
    PersistedManualTransitionState {
        kind,
        from_id: state.from(),
        to_id: state.to(),
        interval_start_basis_points: state
            .interval_start()
            .basis_points()
            .min(PersistedManualTransitionState::MAX_POSITION),
        position_basis_points: state
            .position()
            .basis_points()
            .min(PersistedManualTransitionState::MAX_POSITION),
    }
}

pub(crate) fn restored_t_bar(state: PersistedManualTransitionState) -> AppResult<TBarState> {
    let kind = match state.kind {
        PersistedManualTransitionKind::Fade => TransitionKind::Fade,
        PersistedManualTransitionKind::Wipe => TransitionKind::Wipe,
        PersistedManualTransitionKind::AlphaFade => TransitionKind::AlphaFade,
        PersistedManualTransitionKind::Slide => TransitionKind::Slide,
        PersistedManualTransitionKind::VerticalWipe => TransitionKind::VerticalWipe,
        PersistedManualTransitionKind::VerticalSlide => TransitionKind::VerticalSlide,
        PersistedManualTransitionKind::Zoom => TransitionKind::Zoom,
        PersistedManualTransitionKind::Fly => TransitionKind::Fly,
        PersistedManualTransitionKind::FlyRotate => TransitionKind::FlyRotate,
        PersistedManualTransitionKind::Cube => TransitionKind::Cube,
        PersistedManualTransitionKind::CubeZoom => TransitionKind::CubeZoom,
        PersistedManualTransitionKind::CrossZoom => TransitionKind::CrossZoom,
        PersistedManualTransitionKind::Merge => TransitionKind::Merge,
        PersistedManualTransitionKind::Stinger { slot, .. } => TransitionKind::Stinger(
            fm_switcher::StingerSlotId::new(slot)
                .ok_or_else(|| AppFailure("invalid persisted manual Stinger slot".into()))?,
        ),
    };
    let interval_start = TBarPosition::new(state.interval_start_basis_points)
        .ok_or_else(|| AppFailure("invalid persisted manual-transition interval start".into()))?;
    let position = TBarPosition::new(state.position_basis_points)
        .ok_or_else(|| AppFailure("invalid persisted manual-transition position".into()))?;
    Ok(
        TBarState::restore(kind, state.from_id, state.to_id, interval_start, position)
            .with_clip_frames(state.kind.clip_frames()),
    )
}
