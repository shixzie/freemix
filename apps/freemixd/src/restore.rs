//! Restoring a stored project into the engine and syncing engine state back:
//! titles and their templates, media and preset sources, scenes, edit locks,
//! stingers, plugin chains, audio strips and mixer state, and display
//! outputs.
//!
//! Extracted from `main.rs` as a mechanical move.

#[allow(unused_imports, clippy::wildcard_imports)]
// the binary items, some of them native only
use super::*;

/// The stored template of every title input, keyed by input id.
pub(crate) type TitleTemplates = std::collections::BTreeMap<InputId, fm_titles::TitleTemplate>;

/// What a restored engine needs from the project bundle besides
/// `project.json`: every title input's template and every imported LUT's
/// text.
#[derive(Default)]
pub(crate) struct BundleAssets {
    pub(crate) titles: TitleTemplates,
    pub(crate) luts: std::collections::BTreeMap<fm_model::LutId, fm_engine::EngineLut>,
    /// Where document assets are read from (IN-014); `None` without a
    /// bundle.
    pub(crate) assets_root: Option<PathBuf>,
}

/// Loads the title templates (see [`load_title_templates`]) and the LUTs
/// (see [`grading::load_grade_luts`]) of `project`.
pub(crate) fn load_bundle_assets(
    store: &ProjectStore,
    project: &fm_model::Project,
) -> AppResult<BundleAssets> {
    Ok(BundleAssets {
        titles: load_title_templates(store, project)?,
        luts: grading::load_grade_luts(store, project),
        assets_root: Some(store.assets_root()),
    })
}

/// Loads the stored template of every title input in `project`.
///
/// Title field state is restored against its template, so a title whose
/// template does not load, or whose names do not fit the wire bounds of a
/// `title_state` record, makes the daemon refuse to start: serving it would
/// mean inventing or dropping the operator's field values.
pub(crate) fn load_title_templates(
    store: &ProjectStore,
    project: &fm_model::Project,
) -> AppResult<TitleTemplates> {
    let mut templates = TitleTemplates::new();
    for input in project.inputs() {
        let fm_model::InputKind::Title(title) = &input.kind else {
            continue;
        };
        let stored = store
            .load_title_template(&title.template_uri)
            .map_err(|error| {
                AppFailure(format!(
                    "title input {} template `{}` could not be loaded: {error}",
                    input.id, title.template_uri
                ))
            })?;
        let template = stored.template;
        if template.name.trim().is_empty()
            || template.name.len() > fm_protocol::MAX_TITLE_NAME_BYTES
        {
            return Err(AppFailure(format!(
                "title input {} template name must be nonblank and at most {} bytes",
                input.id,
                fm_protocol::MAX_TITLE_NAME_BYTES
            ))
            .into());
        }
        let names_bytes = template
            .fields
            .iter()
            .map(|field| field.name.len())
            .fold(0_usize, usize::saturating_add);
        if template
            .fields
            .iter()
            .any(|field| field.name.trim().is_empty())
            || names_bytes > fm_protocol::MAX_TITLE_FIELD_NAMES_BYTES
        {
            return Err(AppFailure(format!(
                "title input {} template field names must be nonblank and total at most {} bytes",
                input.id,
                fm_protocol::MAX_TITLE_FIELD_NAMES_BYTES
            ))
            .into());
        }
        // Every snapshot carries the animation in a `title_animation_state`
        // record, so one without a wire encoding would lock out every
        // session.
        let animation_bytes = fm_protocol::title_clip_wire_bytes(&fm_control::title_clip_to_wire(
            &template.animation.clip,
        ));
        if animation_bytes > fm_protocol::MAX_TITLE_ANIMATION_BYTES {
            return Err(AppFailure(format!(
                "title input {} template animation encodes to {animation_bytes} bytes, more \
                 than the {} a title_animation_state record carries",
                input.id,
                fm_protocol::MAX_TITLE_ANIMATION_BYTES
            ))
            .into());
        }
        templates.insert(input.id, template);
    }
    Ok(templates)
}

/// Seeds every title input's persisted field values and revisions into the
/// restored show, checked against its template.
pub(crate) fn restore_titles(
    show: &mut ShowState,
    project: &fm_model::Project,
    templates: &TitleTemplates,
) -> AppResult<()> {
    for input in project.inputs() {
        let fm_model::InputKind::Title(title) = &input.kind else {
            continue;
        };
        let template = templates.get(&input.id).ok_or_else(|| {
            AppFailure(format!("title input {} has no loaded template", input.id))
        })?;
        let fields = fm_titles::TitleFieldSet::restore_revisioned(
            template,
            title.field_revision,
            title.fields.iter().map(|entry| {
                (
                    fm_titles::FieldId::new(entry.id.get()),
                    title_render::field_value(&entry.value),
                    entry.revision,
                )
            }),
        )
        .map_err(|error| {
            AppFailure(format!(
                "title input {} fields do not match template `{}`: {error}",
                input.id, title.template_uri
            ))
        })?;
        // Every title input naming the same stored template shares one
        // animation, keyed by its URI (GX-011).
        show.set_title(
            input.id,
            EngineTitle::new(template.name.clone(), fields)
                .with_template(title.template_uri.clone(), template.clone()),
        )?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn restore_engine(project: &StoredProject) -> AppResult<Engine> {
    restore_engine_with_titles(project, &BundleAssets::default())
}

#[allow(clippy::too_many_lines)] // one restore step per project part
/// Opens every media input of `project` on its synthetic source (the
/// simulated engine's stand-in for the file's decoder, see
/// [`fm_engine::EngineMediaSource::synthetic`]), unless its source is
/// already open or the input is closed (IN-026). After a command this
/// opens imported, duplicated, replaced, and reopened media inputs.
pub(crate) fn attach_synthetic_media(
    control: &mut ControlService<Policy>,
    project: &fm_model::Project,
) -> AppResult<()> {
    let rate = project.settings().frame_rate;
    for input in project.inputs() {
        if project.input_lifecycle(input.id).closed || control.media_source_open(input.id) {
            continue;
        }
        if let fm_model::InputKind::Media { playback, .. } = &input.kind {
            control
                .set_media_source(
                    input.id,
                    Some(fm_engine::EngineMediaSource::synthetic(playback, rate)),
                )
                .map_err(|error| {
                    AppFailure(format!("media input {} cannot open: {error}", input.id))
                })?;
        }
    }
    Ok(())
}

/// The animation preset library, the favourite transition buttons, and
/// each media input's playback settings (IN-003).
pub(crate) fn restore_presets_favorites_and_media(
    show: &mut ShowState,
    canonical: &fm_model::Project,
) -> AppResult<()> {
    show.restore_animation_presets(canonical.animation_presets().clone())
        .map_err(|error| {
            AppFailure(format!(
                "project animation presets cannot be restored: {error}"
            ))
        })?;
    show.restore_transition_favorites(*canonical.transition_favorites());
    for input in canonical.inputs() {
        if let fm_model::InputKind::Media { playback, .. } = &input.kind {
            show.set_media_input(input.id, *playback)?;
        }
        // Each input's replaceable source, for replace and duplicate
        // (IN-026).
        if let Some(source) = fm_model::InputSourceSpec::from_kind(&input.kind) {
            show.set_input_source(input.id, source)?;
        }
        if let fm_model::InputKind::Device { capture, .. } = &input.kind {
            show.set_capture_settings(input.id, capture.clone())?;
        }
        // A document opens on the page it was left on (IN-014).
        if let fm_model::InputKind::Document(document) = &input.kind {
            show.set_paged_page(input.id, document.page)?;
        }
    }
    show.restore_input_lifecycle(
        canonical.input_categories().to_vec(),
        canonical.input_lifecycles(),
    )?;
    // What is recorded for replay (OR-012), once every input's source is
    // known.
    show.set_replay(canonical.replay().clone())
        .map_err(|error| AppFailure(format!("project replay cannot be restored: {error}")))?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(crate) fn restore_engine_with_titles(
    project: &StoredProject,
    bundle_assets: &BundleAssets,
) -> AppResult<Engine> {
    let canonical = project.project();
    let inputs = canonical
        .inputs()
        .iter()
        .map(|input| (input.id, input.name.clone()))
        .collect::<Vec<_>>();
    let input_ids = inputs.iter().map(|(input, _)| *input).collect::<Vec<_>>();
    let main_mix = canonical
        .main_mix()
        .ok_or_else(|| AppFailure("project is missing desired main mix routing".into()))?;
    let routing = project.runtime_routing();
    let realized_program = required_routing(routing.realized_program_id, "realized program")?;
    let realized_preview = required_routing(routing.realized_preview_id, "realized preview")?;
    // The stream inventory and desired running flags are seeded from the
    // project in vector order, so the restored show keeps the authored order.
    // Duplicates, oversized inventories, and blank names fail closed through
    // the switcher's own validation errors.
    let stream_inventory = canonical
        .stream_targets()
        .iter()
        .map(|target| {
            (
                SwitcherStreamTargetId::from_non_zero(target.id().get()),
                target.name().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let mut show = ShowState::new(
        canonical.name(),
        inputs,
        main_mix.desired_program,
        main_mix.desired_preview,
    )?
    .with_outputs(
        canonical
            .outputs()
            .iter()
            .map(|output| (output.id, output.name.clone()))
            .collect(),
    )?
    .with_stream_targets(canonical.stream_targets())?;
    restore_input_audio_strips(&mut show, canonical)?;

    restore_display_outputs(&mut show, canonical)?;
    let mut realized = SwitcherState::new(input_ids, realized_program, realized_preview)?
        .with_streams(stream_inventory)?;
    // The Fade-to-Black output selection is project configuration, seeded
    // into both switchers like the recording flags below.
    for &output in canonical.fade_to_black_excluded_outputs() {
        let _ = realized.set_fade_to_black_output(output, false);
    }
    for (&output, &duration) in canonical.fade_to_black_output_durations() {
        realized
            .set_fade_to_black_output_duration(output, Some(duration))
            .map_err(|error| AppFailure(format!("invalid Fade-to-Black duration: {error}")))?;
    }
    let _ = realized.set_fade_to_black_curve(canonical.fade_to_black_curve());
    for target in canonical
        .stream_targets()
        .iter()
        .filter(|target| target.running())
    {
        let target_id = SwitcherStreamTargetId::from_non_zero(target.id().get());
        show.set_stream_running(target_id, true)?;
        // Idle restore requires desired and realized to agree on stream state;
        // realization itself is daemon-side runtime work, not engine history.
        realized.set_stream_running(target_id, true)?;
    }
    // The recording flags are engine-owned desired state, so they are seeded
    // into BOTH switchers slot by slot: idle restore demands desired/realized
    // agreement, while realization of each desire is daemon-side recorder
    // work.
    for slot in ModelRecorderSlot::ALL {
        let active = canonical.recording_desired(slot);
        let recorder = switcher_recorder_slot(slot);
        show.desired_switcher_mut()
            .set_recording_desired(recorder, active)?;
        realized.set_recording_desired(recorder, active)?;
    }
    for config in canonical.stingers() {
        restore_stinger(&mut show, &mut realized, *config)?;
    }
    let manual = project.runtime_manual_transitions();
    if let Some(state) = manual.desired {
        show.restore_manual_transition(restored_t_bar(state)?)?;
    }
    if let Some(state) = manual.realized {
        realized.restore_t_bar(restored_t_bar(state)?)?;
    }
    let fade_to_black = project.runtime_fade_to_black();
    show.restore_fade_to_black(fade_to_black.desired.target_active);
    realized.restore_settled_fade_to_black(fade_to_black.realized.target_active);
    restore_overlays(&mut show, &mut realized, project.runtime_overlays())?;
    restore_edit_locks(&mut show, canonical)?;
    restore_titles(&mut show, canonical, &bundle_assets.titles)?;
    restore_scenes(&mut show, canonical)?;
    show.set_playlist(fm_engine::playlist_from_model(canonical.playlist())?)?;
    show.restore_grading(canonical.grading().clone(), bundle_assets.luts.clone())
        .map_err(|error| AppFailure(format!("project grading cannot be restored: {error}")))?;
    restore_presets_favorites_and_media(&mut show, canonical)?;
    // Each list input's active item is its media source (IN-006).
    show.restore_media_lists(canonical.media_lists().clone())?;
    // Checked against the restored titles, media inputs, and scripts (a cue
    // may run one, schema 55).
    show.set_scripts(canonical.scripts().to_vec())?;
    show.set_cue_lists(canonical.cue_lists().to_vec())?;
    // A run paused at a wait resumes on its due frame (schema 56).
    show.set_script_run(
        canonical.script_run().clone(),
        canonical.settings().frame_rate,
    )?;
    show.set_shortcuts(canonical.shortcuts().to_vec())?;
    let position = project.position();
    let mut engine = Engine::restore_persisted(
        show,
        realized,
        canonical.settings().frame_rate,
        clock_domain(),
        EngineRestoreState {
            state_epoch: StateEpoch::new(position.state_epoch),
            revision: Revision::new(position.revision),
            event_sequence: EventSequence::new(position.event_sequence),
            runtime_generation: RuntimeGeneration::new(position.runtime_generation),
            clock_time: ClockTime::from_nanos(position.clock_time_nanos),
            frame_cursor: position.frames_rendered.into(),
            receipts: project
                .idempotency_receipts()
                .iter()
                .map(runtime_receipt)
                .collect::<AppResult<Vec<_>>>()?,
        },
    )?;
    // Every shown title continues its timeline (GX-001, schema 68).
    engine.restore_title_timelines(canonical.title_timelines())?;
    Ok(engine)
}

/// Seeds every Scene input's scene (layers and animation) into the restored
/// show, so scene animation edits and on-air phases are engine state
/// (GX-011 stage 5), and every other project scene, so its layer grades can
/// be edited and every snapshot lays it out (SW-019, Protocol 2.35). Every
/// snapshot carries each shown scene's animation in a
/// `scene_animation_state` record, so a scene whose clip has no wire
/// encoding fails closed rather than locking out every session. Every
/// alias input's target is seeded too, so a live layer edit's cycle check
/// follows aliases (SW-006).
pub(crate) fn restore_scenes(show: &mut ShowState, project: &fm_model::Project) -> AppResult<()> {
    let dimensions = project.settings().video.dimensions;
    for scene in project.scenes() {
        show.set_scene(scene.clone(), dimensions.width(), dimensions.height());
    }
    for input in project.inputs() {
        if let fm_model::InputKind::Alias { target, .. } = input.kind {
            show.set_input_alias(input.id, target)?;
        }
    }
    for input in project.inputs() {
        let fm_model::InputKind::Scene { scene_id, .. } = &input.kind else {
            continue;
        };
        let scene = project.scene(*scene_id).ok_or_else(|| {
            AppFailure(format!(
                "scene input {} names missing scene {scene_id}",
                input.id
            ))
        })?;
        let animation_bytes = fm_protocol::scene_clip_wire_bytes(&fm_control::scene_clip_to_wire(
            &scene.animation.clip,
        ));
        if animation_bytes > fm_protocol::MAX_SCENE_ANIMATION_BYTES {
            return Err(AppFailure(format!(
                "scene {scene_id} animation encodes to {animation_bytes} bytes, more than the \
                 {} a scene_animation_state record carries",
                fm_protocol::MAX_SCENE_ANIMATION_BYTES
            ))
            .into());
        }
        show.set_scene_input(
            input.id,
            scene.clone(),
            dimensions.width(),
            dimensions.height(),
        )?;
    }
    Ok(())
}

/// Persists every engine scene's layers (live scene layer edits, SW-006)
/// and animation back into the project. Layers go first: an animation track
/// may name a layer an edit added.
pub(crate) fn sync_project_scenes(
    project: &mut fm_model::Project,
    show: &ShowState,
) -> AppResult<()> {
    for scene in show.scenes().values() {
        let layers_unchanged = project
            .scene(scene.id())
            .is_some_and(|stored| stored.layers == scene.scene.layers);
        if !layers_unchanged {
            project
                .set_scene_layers(scene.id(), scene.scene.layers.clone())
                .map_err(|error| {
                    AppFailure(format!(
                        "scene {} layers could not be persisted: {error}",
                        scene.id()
                    ))
                })?;
        }
    }
    for scene in show.scenes().values() {
        let unchanged = project
            .scene(scene.id())
            .is_some_and(|stored| stored.animation == *scene.animation());
        if unchanged {
            continue;
        }
        project
            .set_scene_animation(scene.id(), scene.animation().clone())
            .map_err(|error| {
                AppFailure(format!(
                    "scene {} animation could not be persisted: {error}",
                    scene.id()
                ))
            })?;
    }
    Ok(())
}

/// Seeds the persisted edit padlock into the restored show, so a checkpoint
/// restore and a journal replay from that checkpoint start from the same lock
/// state. Validation already guarantees every locked input exists.
pub(crate) fn restore_edit_locks(
    show: &mut ShowState,
    project: &fm_model::Project,
) -> AppResult<()> {
    show.set_project_locked(project.project_locked());
    for &input in project.locked_inputs() {
        show.set_input_locked(input, true)?;
    }
    Ok(())
}

pub(crate) fn restore_stinger(
    show: &mut ShowState,
    realized: &mut SwitcherState,
    config: StingerConfig,
) -> AppResult<()> {
    let slot = StingerSlotId::new(config.slot.number()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a validated Stinger slot is in the switcher range",
        )
    })?;
    let descriptor = StingerDescriptor::new(
        config.media_input,
        config.preload,
        config.cut_point_frames,
        match config.audio_policy {
            ModelStingerAudioPolicy::Muted => StingerAudioPolicy::Muted,
            ModelStingerAudioPolicy::StingerOnly => StingerAudioPolicy::StingerOnly,
            ModelStingerAudioPolicy::MixWithProgram => StingerAudioPolicy::MixWithProgram,
        },
        match config.missing_media_fallback {
            StingerMissingMediaFallback::Cut => MissingMediaFallback::Cut,
            StingerMissingMediaFallback::Fade => MissingMediaFallback::Fade,
            StingerMissingMediaFallback::KeepProgram => MissingMediaFallback::KeepProgram,
        },
    );
    show.configure_stinger(slot, descriptor)?;
    realized.configure_stinger(slot, descriptor)?;

    if config.preload {
        // Native startup resolves every requested source before the first tick.
        // Until live source-health events are projected into the engine, a
        // successfully restored input is the deterministic ready state.
        let _ = show.preload_stinger(slot, true)?;
        let _ = realized.preload_stinger(slot, true)?;
    }
    Ok(())
}

/// The engine's plugin chain for a persisted one (AU-006, schema 91).
pub(crate) fn engine_plugin_chain(
    state: &fm_types::audio_plugins::PluginChainState,
) -> AppResult<fm_plugin_host_api::PluginChain> {
    let limits = fm_plugin_host_api::ProtocolLimits::default();
    let mut chain = fm_plugin_host_api::PluginChain::new();
    for slot in state.slots() {
        let plugin = fm_plugin_host_api::PluginId::new(slot.plugin());
        let mut engine_slot = fm_plugin_host_api::PluginSlot::new(plugin.clone());
        engine_slot.set_bypassed(slot.is_bypassed());
        engine_slot
            .set_latency(slot.latency_frames())
            .map_err(|error| AppFailure(format!("project plugin chain is invalid: {error}")))?;
        if !slot.state().is_empty() {
            let snapshot = fm_plugin_host_api::StateSnapshot::new(
                plugin,
                fm_plugin_host_api::StateVersion::new(0),
                slot.state().to_vec(),
                &limits,
            )
            .map_err(|error| AppFailure(format!("project plugin state is invalid: {error}")))?;
            engine_slot
                .store_snapshot(snapshot)
                .map_err(|error| AppFailure(format!("project plugin state is invalid: {error}")))?;
        }
        chain
            .push(engine_slot)
            .map_err(|error| AppFailure(format!("project plugin chain is invalid: {error}")))?;
    }
    Ok(chain)
}

/// The persisted form of the engine's plugin chain (AU-006, schema 91).
pub(crate) fn model_plugin_chain(
    chain: &fm_plugin_host_api::PluginChain,
) -> AppResult<fm_types::audio_plugins::PluginChainState> {
    let mut state = fm_types::audio_plugins::PluginChainState::new();
    for slot in chain.slots() {
        let mut model = fm_types::audio_plugins::PluginSlotState::new(slot.plugin().as_str());
        model.set_bypassed(slot.is_bypassed());
        model
            .set_latency(slot.latency_frames())
            .map_err(|error| AppFailure(format!("plugin chain is invalid: {error}")))?;
        if let Some(snapshot) = slot.snapshot() {
            model
                .set_state(snapshot.data().to_vec())
                .map_err(|error| AppFailure(format!("plugin chain is invalid: {error}")))?;
        }
        state
            .push(model)
            .map_err(|error| AppFailure(format!("plugin chain is invalid: {error}")))?;
    }
    Ok(state)
}

pub(crate) fn restore_input_audio_strips(
    show: &mut ShowState,
    project: &fm_model::Project,
) -> AppResult<()> {
    for strip in project.input_audio_strips() {
        show.set_input_audio_strip(
            strip.input,
            EngineInputAudioStripState {
                gain_millidb: strip.state.gain.get(),
                balance_basis_points: strip.state.balance.get(),
                pan_basis_points: strip.state.pan.get(),
                muted: strip.state.muted,
                soloed: strip.state.soloed,
                follow_video: strip.state.follow_video,
                delay_samples: strip.state.delay_samples.get(),
                sends: strip.state.sends,
            },
        )?;
        show.set_input_audio_label(strip.input, strip.label.clone())?;
        show.set_input_audio_group(strip.input, strip.group)?;
        show.set_input_audio_dsp(strip.input, strip.dsp)?;
        show.set_input_audio_plugins(strip.input, engine_plugin_chain(&strip.plugins)?);
    }
    show.set_master_audio_dsp(*project.master_audio_dsp());
    show.set_master_audio_plugins(engine_plugin_chain(project.master_audio_plugins())?);
    for (group, value) in project.audio_groups().iter() {
        show.set_audio_group(
            group,
            fm_engine::EngineAudioGroup {
                name: value.name.clone(),
                state: fm_engine::EngineAudioGroupState {
                    gain_millidb: value.gain.get(),
                    muted: value.muted,
                },
                soloed: value.soloed,
                parent: value.parent,
            },
        )?;
    }
    for (bus, value) in project.aux_buses().iter() {
        show.set_aux_bus(
            bus,
            fm_engine::EngineAuxBus {
                name: value.name.clone(),
                state: fm_engine::EngineAudioGroupState {
                    gain_millidb: value.gain.get(),
                    muted: value.muted,
                },
                dsp: value.dsp,
                device: value.device.clone(),
                omt: value.omt,
            },
        )?;
    }
    for (&output, &bus) in project.output_aux_buses() {
        show.set_output_aux_bus(output, Some(bus));
    }
    show.set_audio_solo_mode(project.audio_solo_mode());
    let monitor = project.audio_monitor_output();
    show.set_audio_monitor(fm_engine::EngineAudioMonitor {
        device: monitor.device.clone(),
        state: fm_engine::EngineAudioGroupState {
            gain_millidb: monitor.gain.get(),
            muted: monitor.muted,
        },
    })?;
    show.restore_fade_to_black_excluded_outputs(
        project.fade_to_black_excluded_outputs().iter().copied(),
    )?;
    show.restore_fade_to_black_timing(
        project
            .fade_to_black_output_durations()
            .iter()
            .map(|(output, duration)| (*output, *duration)),
        project.fade_to_black_curve(),
    )?;
    Ok(())
}

/// Seeds the show's display-output slots (OR-011) and `LiveLAN`
/// configuration (OR-017) from the project.
pub(crate) fn restore_display_outputs(
    show: &mut ShowState,
    project: &fm_model::Project,
) -> AppResult<()> {
    for (&slot, config) in project.display_outputs() {
        show.set_display_output(slot, Some(config.clone()))?;
    }
    show.set_live_lan(project.live_lan().clone())?;
    for setting in project.alert_rules() {
        show.set_alert_rule(*setting)?;
    }
    Ok(())
}

/// Folds the show's `LiveLAN` configuration (OR-017) and display-output
/// slots (OR-011) into the stored project.
/// Every slot is cleared first, so a display moving between slots is never
/// claimed twice.
pub(crate) fn sync_project_display_outputs(
    project: &mut fm_model::Project,
    show: &ShowState,
) -> AppResult<()> {
    if project.alert_rules() != show.alert_rules() {
        for rule in fm_types::alert_rules::HealthAlertRule::ALL {
            project
                .set_alert_rule(fm_types::alert_rules::effective_setting(
                    show.alert_rules(),
                    rule,
                ))
                .map_err(|error| AppFailure(format!("project cannot take alert rules: {error}")))?;
        }
    }
    if project.live_lan() != show.live_lan() {
        project
            .set_live_lan(show.live_lan().clone())
            .map_err(|error| AppFailure(format!("project cannot take LiveLAN: {error}")))?;
    }
    if project.replay() != show.replay() {
        project
            .set_replay(show.replay().clone())
            .map_err(|error| AppFailure(format!("project cannot take replay: {error}")))?;
    }
    if project.display_outputs() == show.display_outputs() {
        return Ok(());
    }
    for slot in DisplayOutputSlot::ALL {
        project.clear_display_output(slot);
    }
    for (&slot, config) in show.display_outputs() {
        project
            .set_display_output(slot, config.clone())
            .map_err(|error| {
                AppFailure(format!(
                    "project cannot take display output {slot}: {error}"
                ))
            })?;
    }
    Ok(())
}

/// Folds the show's strip labels, groups, and Fade-to-Black output
/// selection (AU-001, SW-004) into the stored project.
#[allow(clippy::too_many_lines)]
pub(crate) fn sync_project_audio_mixer(
    project: &mut fm_model::Project,
    show: &ShowState,
) -> AppResult<()> {
    for &input in show.inputs() {
        let label = show.input_audio_label(input);
        if project.input_audio_label(input) != Some(label) {
            project
                .set_input_audio_label(input, label.to_owned())
                .map_err(|error| {
                    AppFailure(format!(
                        "project cannot take input {input}'s strip label: {error}"
                    ))
                })?;
        }
        if !project.set_input_audio_group(input, show.input_audio_group(input)) {
            return Err(
                AppFailure(format!("project is missing audio strip for input {input}")).into(),
            );
        }
        project
            .set_input_audio_dsp(input, show.input_audio_dsp(input))
            .map_err(|error| {
                AppFailure(format!(
                    "project cannot take input {input}'s strip DSP: {error}"
                ))
            })?;
        project
            .set_input_audio_plugins(input, model_plugin_chain(show.input_audio_plugins(input))?)
            .map_err(|error| {
                AppFailure(format!(
                    "project cannot take input {input}'s plugin chain: {error}"
                ))
            })?;
    }
    project
        .set_master_audio_dsp(*show.master_audio_dsp())
        .map_err(|error| AppFailure(format!("project cannot take the Master DSP: {error}")))?;
    project
        .set_master_audio_plugins(model_plugin_chain(show.master_audio_plugins())?)
        .map_err(|error| {
            AppFailure(format!(
                "project cannot take the Master plugin chain: {error}"
            ))
        })?;
    // Parents are cleared first so an old parent never closes a transient
    // cycle with a new one.
    for nested in [false, true] {
        for (group, value) in fm_model::AudioGroupNumber::ALL
            .into_iter()
            .zip(show.audio_groups())
        {
            let gain =
                InputGainMilliDb::new(value.state.gain_millidb).unwrap_or(InputGainMilliDb::UNITY);
            project
                .set_audio_group(
                    group,
                    fm_model::AudioGroup {
                        name: value.name.clone(),
                        gain,
                        muted: value.state.muted,
                        soloed: value.soloed,
                        parent: value.parent.filter(|_| nested),
                    },
                )
                .map_err(|error| {
                    AppFailure(format!("project cannot take audio group {group}: {error}"))
                })?;
        }
    }
    for (bus, value) in fm_model::AuxBusNumber::ALL
        .into_iter()
        .zip(show.aux_buses())
    {
        project
            .set_aux_bus(
                bus,
                fm_model::AuxBus {
                    name: value.name.clone(),
                    gain: InputGainMilliDb::new(value.state.gain_millidb)
                        .unwrap_or(InputGainMilliDb::UNITY),
                    muted: value.state.muted,
                    dsp: value.dsp,
                    device: value.device.clone(),
                    omt: value.omt,
                },
            )
            .map_err(|error| AppFailure(format!("project cannot take aux {bus}: {error}")))?;
    }
    let outputs = project
        .outputs()
        .iter()
        .map(|output| output.id)
        .collect::<Vec<_>>();
    for output in outputs {
        project.set_output_aux_bus(output, show.output_aux_bus(output));
    }
    project.set_audio_solo_mode(show.audio_solo_mode());
    let monitor = show.audio_monitor();
    project
        .set_audio_monitor_output(fm_model::AudioMonitorOutput {
            device: monitor.device.clone(),
            gain: InputGainMilliDb::new(monitor.state.gain_millidb)
                .unwrap_or(InputGainMilliDb::UNITY),
            muted: monitor.state.muted,
        })
        .map_err(|error| AppFailure(format!("project cannot take the monitor output: {error}")))?;
    let excluded = show.desired_switcher().fade_to_black_excluded_outputs();
    let outputs = project
        .outputs()
        .iter()
        .map(|output| output.id)
        .collect::<Vec<_>>();
    for output in outputs {
        project.set_fade_to_black_output(output, !excluded.contains(&output));
        project.set_fade_to_black_output_duration(
            output,
            show.desired_switcher()
                .fade_to_black_output_duration(output),
        );
    }
    project.set_fade_to_black_curve(show.desired_switcher().fade_to_black_curve());
    Ok(())
}

pub(crate) fn model_audio_strip_state(state: EngineInputAudioStripState) -> InputAudioStripState {
    InputAudioStripState {
        gain: InputGainMilliDb::new(state.gain_millidb).unwrap_or(InputGainMilliDb::UNITY),
        balance: InputBalanceBasisPoints::new(state.balance_basis_points).unwrap_or_default(),
        pan: fm_model::InputPanBasisPoints::new(state.pan_basis_points).unwrap_or_default(),
        muted: state.muted,
        soloed: state.soloed,
        follow_video: state.follow_video,
        delay_samples: InputDelaySamples::new(state.delay_samples).unwrap_or_default(),
        sends: state.sends,
    }
}
#[cfg(test)]
mod audio_plugin_chain_tests {
    use super::{engine_plugin_chain, model_plugin_chain};
    use fm_types::audio_plugins::{PluginChainState, PluginSlotState};

    #[test]
    pub(crate) fn persisted_and_engine_plugin_chains_round_trip() {
        let mut trim = PluginSlotState::new("freemix.reference.trim");
        trim.set_bypassed(true);
        trim.set_latency(7).unwrap();
        trim.set_state([1_u8, 2, 3]).unwrap();
        let mut polarity = PluginSlotState::new("freemix.reference.polarity");
        polarity.set_state([9_u8]).unwrap();
        let persisted = PluginChainState::from_slots(vec![trim, polarity]).expect("a valid chain");
        let engine = engine_plugin_chain(&persisted).expect("valid persisted chain");
        assert_eq!(engine.len(), 2);
        assert!(engine.slots()[0].is_bypassed());
        assert_eq!(engine.slots()[0].latency_frames(), 7);
        assert_eq!(
            engine.slots()[0]
                .snapshot()
                .map(fm_plugin_host_api::StateSnapshot::data),
            Some([1_u8, 2, 3].as_slice())
        );
        assert_eq!(
            model_plugin_chain(&engine).expect("valid engine chain"),
            persisted
        );
        let empty = PluginChainState::new();
        assert!(engine_plugin_chain(&empty).unwrap().is_empty());
        assert_eq!(
            model_plugin_chain(&fm_plugin_host_api::PluginChain::new()).unwrap(),
            empty
        );
    }
}
