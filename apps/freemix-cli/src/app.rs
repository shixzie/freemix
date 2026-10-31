use std::{
    error::Error,
    fs::{self, File, OpenOptions},
    io,
    num::NonZeroU128,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use fm_clock::{ClockDomainId, ClockTime};
use fm_command::{
    AcceptedReceipt, CommandEnvelope, CommandId, CommandReceipt, EventSequence, IdempotencyKey,
    RejectedReceipt, Rejection, RejectionCode, Revision, RuntimeGeneration, StateEpoch,
};
use fm_engine::{
    Engine, EngineAcceptance, EngineAudioMixerEdit, EngineCommand, EngineFadeToBlackState,
    EngineInputAudioStripState, EngineManualTransitionKind, EngineManualTransitionPosition,
    EngineRestoreState, EngineStreamControl, ShowState,
};
use fm_model::{
    AudioBus, CropRect, Input, InputAudioStripState, InputBalanceBasisPoints, InputDelaySamples,
    InputGainMilliDb, InputKind, Layer, LayerEffect, LayerGeometry, MainMix, Project,
    ProjectSettings, RecorderSlot as ModelRecorderSlot, RectMask, Rgba8 as ModelRgba8, Rotation,
    SceneLayerError, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, SourceRef,
    SrtPassphrase, StartupPolicy, StingerAudioPolicy as ModelStingerAudioPolicy, StingerConfig,
    StingerMissingMediaFallback, StingerSlotNumber, StreamEndpoint, StreamKey, StreamProtocol,
    StreamTarget, StreamTargetId,
};
use fm_persistence::validate_asset_uri;
use fm_persistence::{
    ChangeKind, ContentInventory, FadeToBlackState as PersistedFadeToBlackState,
    IdempotencyReceipt, ImportPlan, ManualTransitionKind as PersistedManualTransitionKind,
    ManualTransitionState as PersistedManualTransitionState, MergeStrategy, ProjectPosition,
    ProjectStore, ReceiptOutcome, RuntimeFadeToBlack, RuntimeManualTransitions,
    RuntimeOverlayBorder, RuntimeOverlayChannel, RuntimeOverlayPosition, RuntimeOverlayTransition,
    RuntimeOverlays, RuntimeRouting, SettingsSnapshot, StoredProject,
};
use fm_sim::{Rgba8, SimulatedPipeline, SimulatedSource, SourcePattern};
use fm_switcher::{
    MissingMediaFallback, OverlayBorderPreset, OverlayChannelId, OverlayChannelState,
    OverlayPositionPreset, OverlayTransitionKind, StingerAudioPolicy, StingerDescriptor,
    StingerSlotId, SwitcherState, TBarPosition, TBarState, TransitionKind,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
    capture::CaptureInputConfig,
};
use fm_video::{ImageFrame, write_ppm};

use crate::{
    args::{
        AudioMixerArgs, Command, ManualTransitionKind, OverlayBorder as CliOverlayBorder,
        OverlayPosition as CliOverlayPosition, OverlayTransition as CliOverlayTransition,
        PluginChainTarget, StingerAudioPolicy as CliStingerAudioPolicy,
        StingerFallback as CliStingerFallback, StreamSpec, TBarAction,
    },
    project_bundle, remote, scene_animation, title_animation,
};

pub(crate) type AppResult<T> = Result<T, Box<dyn Error>>;
static IMPLICIT_KEY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// Most idempotency receipts a local checkpoint retains.
///
/// Every local command stores a receipt, so an unbounded history would
/// eventually push `project.json` past the manifest size limit and make the
/// bundle unsaveable. Only the most recent receipts are kept; a key older than
/// this window is treated as new if it is ever reused.
const MAX_PERSISTED_RECEIPTS: usize = 1_024;
static PROJECT_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static PPM_OUTPUT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) struct ProjectEngine {
    project: Project,
    pub(crate) engine: Engine,
}

#[allow(clippy::too_many_lines)]
pub fn run(command: Command) -> AppResult<()> {
    match command {
        Command::New { path, name } => {
            if path.try_exists()? {
                return Err(AppFailure(format!(
                    "destination bundle already exists: {}",
                    path.display()
                ))
                .into());
            }
            let project = default_project(name)?;
            save_engine(&path, &project)?;
            print_status(&project);
        }
        Command::ProjectRename { path, name } => rename_project(&path, name)?,
        Command::InputAdd { path, input, name } => {
            add_input(&path, input_id(input)?, name)?;
        }
        Command::SimulatedSolidInputAdd {
            path,
            input,
            name,
            red,
            green,
            blue,
            alpha,
        } => add_input_with_kind(
            &path,
            input_id(input)?,
            name,
            InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(red, green, blue, alpha)),
                SimulatedAudio::Silence,
            )),
        )?,
        Command::MediaInputAdd {
            path,
            input,
            name,
            asset_uri,
        } => {
            validate_asset_uri(&asset_uri)?;
            add_input_with_kind(
                &path,
                input_id(input)?,
                name,
                InputKind::Media {
                    asset_uri,
                    playback: fm_model::MediaPlaybackSettings::default(),
                },
            )?;
        }
        Command::MediaInputRelink {
            path,
            input,
            asset_uri,
        } => {
            validate_asset_uri(&asset_uri)?;
            relink_media_input(&path, input_id(input)?, asset_uri)?;
        }
        Command::AudioBusAdd { path, bus, name } => {
            add_audio_bus(&path, bus_id(bus)?, name)?;
        }
        Command::AudioBusRename { path, bus, name } => {
            rename_audio_bus(&path, bus_id(bus)?, name)?;
        }
        Command::AudioBusRemove { path, bus } => {
            remove_audio_bus(&path, bus_id(bus)?)?;
        }
        Command::AudioBusSend {
            path,
            source,
            destination,
            add,
        } => update_audio_bus_send(&path, bus_id(source)?, bus_id(destination)?, add)?,
        Command::OutputAdd {
            path,
            output,
            scene,
            bus,
            name,
        } => add_output(
            &path,
            output_id(output)?,
            scene_id(scene)?,
            bus_id(bus)?,
            name,
        )?,
        Command::OutputRoute {
            path,
            output,
            scene,
            bus,
        } => {
            set_output_route(&path, output_id(output)?, scene_id(scene)?, bus_id(bus)?)?;
        }
        Command::OutputRename { path, output, name } => {
            rename_output(&path, output_id(output)?, name)?;
        }
        Command::OutputStartup {
            path,
            output,
            startup,
        } => set_output_startup(&path, output_id(output)?, startup)?,
        Command::OutputRemove { path, output } => {
            remove_output(&path, output_id(output)?)?;
        }
        Command::StreamAdd { path, spec } => add_stream_target(&path, &spec)?,
        Command::StreamUpdate { path, spec } => update_stream_target(&path, &spec)?,
        Command::StreamRemove { path, stream } => {
            remove_stream_target(&path, stream_target_id(stream)?)?;
        }
        Command::StreamStart { path, stream } => {
            set_stream_running(
                &path,
                switcher_stream_target(stream_target_id(stream)?),
                true,
            )?;
        }
        Command::StreamStop { path, stream } => {
            set_stream_running(
                &path,
                switcher_stream_target(stream_target_id(stream)?),
                false,
            )?;
        }
        Command::StreamStartAll { path } => set_all_streams_running(&path, true)?,
        Command::StreamStopAll { path } => set_all_streams_running(&path, false)?,
        Command::RecordStart { path, recorder } => set_recording_desired(&path, recorder, true)?,
        Command::RecordStop { path, recorder } => set_recording_desired(&path, recorder, false)?,
        Command::Streams { path } => {
            let stored = inspect_stored_project(&path)?;
            print_stream_targets(stored.project());
        }
        Command::SceneInputAdd {
            path,
            input,
            scene,
            name,
        } => {
            add_scene_input(&path, input_id(input)?, scene_id(scene)?, name)?;
        }
        Command::SceneInputDuplicate {
            path,
            source_scene,
            new_scene,
            new_input,
            scene_name,
            input_name,
        } => duplicate_scene_input(
            &path,
            scene_id(source_scene)?,
            scene_id(new_scene)?,
            input_id(new_input)?,
            scene_name,
            input_name,
        )?,
        Command::SceneInputAudioSource {
            path,
            scene_input,
            audio_source,
        } => {
            set_scene_input_audio_source(
                &path,
                input_id(scene_input)?,
                Some(input_id(audio_source)?),
            )?;
        }
        Command::SceneInputAudioSourceClear { path, scene_input } => {
            set_scene_input_audio_source(&path, input_id(scene_input)?, None)?;
        }
        Command::SceneInputRemove { path, scene_input } => {
            remove_scene_input(&path, input_id(scene_input)?)?;
        }
        Command::SceneBackground {
            path,
            scene,
            red,
            green,
            blue,
            alpha,
        } => {
            set_scene_background(
                &path,
                scene_id(scene)?,
                ModelRgba8::new(red, green, blue, alpha),
            )?;
        }
        Command::SceneRemove { path, scene } => remove_scene(&path, scene_id(scene)?)?,
        Command::SceneRename { path, scene, name } => rename_scene(&path, scene_id(scene)?, name)?,
        Command::SceneLayerAdd {
            path,
            scene,
            source,
            z_order,
            name,
        } => add_scene_layer(
            &path,
            scene_id(scene)?,
            SourceRef::Input(input_id(source)?),
            z_order,
            name,
        )?,
        Command::SceneLayerAddScene {
            path,
            scene,
            source_scene,
            z_order,
            name,
        } => add_scene_layer(
            &path,
            scene_id(scene)?,
            SourceRef::Scene(scene_id(source_scene)?),
            z_order,
            name,
        )?,
        Command::SceneLayerRemove { path, scene, index } => {
            remove_scene_layer(&path, scene_id(scene)?, index)?;
        }
        Command::SceneLayerMove {
            path,
            scene,
            source_index,
            destination_index,
        } => move_scene_layer(&path, scene_id(scene)?, source_index, destination_index)?,
        Command::SceneLayerDuplicate {
            path,
            scene,
            index,
            name,
        } => duplicate_scene_layer(&path, scene_id(scene)?, index, name)?,
        Command::SceneLayerCopy {
            path,
            source_scene,
            source_index,
            target_scene,
            name,
        } => copy_scene_layer(
            &path,
            scene_id(source_scene)?,
            source_index,
            scene_id(target_scene)?,
            name,
        )?,
        Command::SceneLayerRename {
            path,
            scene,
            index,
            name,
        } => rename_scene_layer(&path, scene_id(scene)?, index, name)?,
        Command::SceneLayerSourceInput {
            path,
            scene,
            index,
            input,
        } => set_scene_layer_source(
            &path,
            scene_id(scene)?,
            index,
            SourceRef::Input(input_id(input)?),
        )?,
        Command::SceneLayerSourceScene {
            path,
            scene,
            index,
            source_scene,
        } => set_scene_layer_source(
            &path,
            scene_id(scene)?,
            index,
            SourceRef::Scene(scene_id(source_scene)?),
        )?,
        Command::SceneLayerZOrder {
            path,
            scene,
            index,
            z_order,
        } => set_scene_layer_z_order(&path, scene_id(scene)?, index, z_order)?,
        Command::SceneLayerAppearance {
            path,
            scene,
            index,
            enabled,
            opacity,
        } => set_scene_layer_appearance(&path, scene_id(scene)?, index, enabled, opacity)?,
        Command::SceneLayerGeometry {
            path,
            scene,
            index,
            x,
            y,
            width,
            height,
            rotation_millidegrees,
            anchor,
        } => set_scene_layer_geometry(
            &path,
            scene_id(scene)?,
            index,
            LayerGeometry::new(x, y, width, height, Rotation::Deg0)
                .with_rotation_millidegrees(rotation_millidegrees)
                .with_anchor(anchor.map_or(fm_model::LayerAnchor::CENTER, |(x, y)| {
                    fm_model::LayerAnchor::new(x, y)
                })),
        )?,
        Command::SceneLayerCrop {
            path,
            scene,
            index,
            x,
            y,
            width,
            height,
        } => set_scene_layer_crop(
            &path,
            scene_id(scene)?,
            index,
            Some(CropRect::new(x, y, width, height)),
        )?,
        Command::SceneLayerCropClear { path, scene, index } => {
            set_scene_layer_crop(&path, scene_id(scene)?, index, None)?;
        }
        Command::SceneLayerMask {
            path,
            scene,
            index,
            x,
            y,
            width,
            height,
            inverted,
        } => set_scene_layer_mask(
            &path,
            scene_id(scene)?,
            index,
            Some(RectMask::new(x, y, width, height).inverted(inverted)),
        )?,
        Command::SceneLayerMaskClear { path, scene, index } => {
            set_scene_layer_mask(&path, scene_id(scene)?, index, None)?;
        }
        Command::SceneLayerEffectAdd {
            path,
            scene,
            index,
            name,
            parameters,
        } => {
            add_scene_layer_effect(
                &path,
                scene_id(scene)?,
                index,
                LayerEffect::new(name, parameters),
            )?;
        }
        Command::SceneLayerEffectSet {
            path,
            scene,
            index,
            effect_index,
            name,
            parameters,
        } => {
            set_scene_layer_effect(
                &path,
                scene_id(scene)?,
                index,
                effect_index,
                LayerEffect::new(name, parameters),
            )?;
        }
        Command::SceneLayerEffectRemove {
            path,
            scene,
            index,
            effect_index,
        } => {
            remove_scene_layer_effect(&path, scene_id(scene)?, index, effect_index)?;
        }
        Command::SceneLayerEffectClear { path, scene, index } => {
            set_scene_layer_effects(&path, scene_id(scene)?, index, Vec::new())?;
        }
        Command::SceneLayerKey {
            path,
            scene,
            index,
            layer_key,
        } => {
            let scene = scene_id(scene)?;
            update_project(&path, |project| {
                project
                    .set_scene_layer_key(scene, index, layer_key)
                    .map_err(Into::into)
            })?;
        }
        Command::RemoteSceneLayerKey {
            address,
            scene,
            layer,
            layer_key,
            key,
            expected_revision,
        } => remote::execute(
            address,
            remote_scene_layer_edit(
                scene,
                layer,
                fm_protocol::SceneLayerOperation::SetKey(layer_key.map(fm_control::wire_key)),
            )?,
            key,
            expected_revision,
        )?,
        Command::SceneLayerAutoKey {
            path,
            scene,
            index,
            picture,
            point,
        } => {
            let scene = scene_id(scene)?;
            update_project(&path, |project| {
                let picture = match &picture {
                    Some(file) => crate::auto_key::read_ppm(file)?,
                    None => crate::auto_key::layer_input_picture(project, scene, index)?,
                };
                let auto = picture.auto_key(crate::auto_key::sample(point))?;
                let previous = project
                    .scenes()
                    .iter()
                    .find(|candidate| candidate.id == scene)
                    .and_then(|found| found.layers.get(index))
                    .and_then(|layer| layer.key)
                    .map(fm_control::wire_key);
                println!("{}", crate::auto_key::describe(&auto));
                project
                    .set_scene_layer_key(
                        scene,
                        index,
                        Some(fm_control::domain_key(auto.layer_key(previous))),
                    )
                    .map_err(Into::into)
            })?;
        }
        Command::RemoteReplayMultiview {
            address,
            input,
            output,
            width,
            height,
        } => remote::replay_multiview(address, input.to_domain(), &output, width, height)?,
        Command::RemoteAudioPluginChain {
            address,
            input,
            slots,
        } => remote::audio_plugin_chain(address, input, slots)?,
        Command::RemoteSceneLayerAutoKey {
            address,
            scene,
            layer,
            picture,
            point,
            refinement,
            key,
            expected_revision,
        } => {
            // The daemon's layer is read only for what was not given: the
            // input's picture, and the key whose matte refinement is kept.
            let source = if picture.is_none() || refinement.is_none() {
                let layer = core::num::NonZeroU32::new(layer)
                    .ok_or_else(|| AppFailure("layer ID must be nonzero".into()))?;
                Some(remote::scene_layer_key_source(
                    address,
                    scene_id(scene)?,
                    fm_protocol::WireSceneLayerId::new(layer),
                    picture.is_none(),
                )?)
            } else {
                None
            };
            let picture = match (
                picture,
                source.as_ref().and_then(|source| source.picture.as_ref()),
            ) {
                (Some(file), _) => crate::auto_key::read_ppm(&file)?,
                (None, Some(image)) => crate::auto_key::Picture {
                    width: u32::from(image.width),
                    height: u32::from(image.height),
                    rgba: image.rgba.to_vec(),
                },
                (None, None) => {
                    return Err(AppFailure("the daemon sent no input picture".into()).into());
                }
            };
            let auto = picture.auto_key(crate::auto_key::sample(point))?;
            println!("{}", crate::auto_key::describe(&auto));
            let previous = match refinement {
                Some(refinement) => Some(fm_control::wire_key(fm_model::LayerKey::Chroma(
                    fm_model::ChromaLayerKey {
                        refinement,
                        ..fm_model::ChromaLayerKey::GREEN
                    },
                ))),
                None => source.and_then(|source| source.key),
            };
            remote::execute(
                address,
                remote_scene_layer_edit(
                    scene,
                    layer,
                    fm_protocol::SceneLayerOperation::SetKey(Some(auto.layer_key(previous))),
                )?,
                key,
                expected_revision,
            )?;
        }
        Command::RemoteSceneLayerEffects {
            address,
            scene,
            layer,
            effects,
            key,
            expected_revision,
        } => remote::execute(
            address,
            remote_scene_layer_edit(
                scene,
                layer,
                fm_protocol::SceneLayerOperation::SetEffects(fm_control::wire_effects(&effects)),
            )?,
            key,
            expected_revision,
        )?,
        Command::InputRemove { path, input } => remove_input(&path, input_id(input)?)?,
        Command::InputDuplicate {
            path,
            source,
            input,
            name,
        } => duplicate_input(&path, input_id(source)?, input_id(input)?, name)?,
        Command::InputReplaceSimulated { path, input } => {
            replace_input_simulated(&path, input_id(input)?)?;
        }
        Command::InputReplaceSolid {
            path,
            input,
            red,
            green,
            blue,
            alpha,
        } => replace_input_solid(
            &path,
            input_id(input)?,
            SolidColor::new(red, green, blue, alpha),
        )?,
        Command::InputReplaceMedia {
            path,
            input,
            asset_uri,
        } => {
            validate_asset_uri(&asset_uri)?;
            replace_input_media(&path, input_id(input)?, asset_uri)?;
        }
        Command::InputReplaceScene { path, input, scene } => {
            replace_input_scene(&path, input_id(input)?, scene_id(scene)?)?;
        }
        Command::Status { path } => print_status(&inspect_engine(&path)?),
        Command::JournalRecover { path } => recover_journal(&path)?,
        Command::Inputs { path } => {
            let stored = inspect_stored_project(&path)?;
            print_inputs(stored.project());
        }
        Command::TransitionKinds => print_transition_kinds(),
        Command::TransitionFavorites { path } => {
            let stored = inspect_stored_project(&path)?;
            print_transition_favorites(stored.project());
        }
        Command::Transition {
            path,
            kind,
            frames,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::Transition {
                kind: fm_control::domain_transition_kind(kind),
                duration_frames: frames,
            },
            frames,
            key,
            expected_revision,
        )?,
        Command::InputMediaList { path } => {
            let project = inspect_engine(&path)?;
            for status in project.engine.media_statuses() {
                println!(
                    "{}",
                    remote::format_media_status(&fm_control::wire_media_status(&status))
                );
            }
            // A local project probes no files: every length is unknown.
            let lengths = fm_control::MediaListLengths::new();
            for status in project.engine.media_list_statuses() {
                println!(
                    "{}",
                    remote::format_media_list(&fm_control::wire_media_list_status(
                        &status, &lengths
                    ))
                );
            }
        }
        Command::InputMediaListNavigate {
            target,
            input,
            navigation,
            key,
            expected_revision,
        } => {
            let payload = fm_protocol::CommandPayload::MediaList {
                input: fm_protocol::WireInputId::from_domain(input_id(input)?),
                operation: fm_protocol::MediaListOperation::Navigate(navigation),
            };
            match target {
                crate::args::MediaTarget::Local(path) => mutate(
                    &path,
                    fm_control::media_list_engine_command(&payload),
                    1,
                    key,
                    expected_revision,
                )?,
                crate::args::MediaTarget::Remote(address) => {
                    remote::execute(address, payload, key, expected_revision)?;
                }
            }
        }
        Command::InputMediaListOptions {
            path,
            input,
            auto_next,
            auto_first,
            loop_list,
            shuffle_seed,
            key,
            expected_revision,
        } => {
            let project = inspect_engine(&path)?;
            let input = input_id(input)?;
            let list = project
                .engine
                .show()
                .media_list(input)
                .cloned()
                .ok_or_else(|| AppFailure(format!("input {input} is not a media list")))?;
            let mut options = list.options;
            options.auto_next = auto_next;
            options.auto_first = auto_first;
            options.loop_list = loop_list;
            options.shuffle_seed = shuffle_seed;
            mutate(
                &path,
                EngineCommand::MediaList(fm_engine::EngineMediaListCommand {
                    input,
                    operation: fm_engine::EngineMediaListOperation::Set {
                        items: list.items,
                        options,
                        active: Some(list.active),
                    },
                }),
                1,
                key,
                expected_revision,
            )?;
        }
        Command::InputMediaListItem {
            path,
            input,
            operation,
            key,
            expected_revision,
        } => {
            let project = inspect_engine(&path)?;
            let input = input_id(input)?;
            let mut list = project
                .engine
                .show()
                .media_list(input)
                .cloned()
                .ok_or_else(|| AppFailure(format!("input {input} is not a media list")))?;
            let active = list.active;
            let mut keep_active = true;
            match operation {
                crate::args::MediaListItemEdit::Add { asset_uri, title } => {
                    let id = list
                        .items
                        .iter()
                        .map(|item| item.id)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1);
                    list.items.push(fm_model::MediaListItem {
                        id,
                        asset_uri,
                        title,
                        mark_in: 0,
                        mark_out: None,
                    });
                }
                crate::args::MediaListItemEdit::Remove { item } => {
                    if !list.items.iter().any(|candidate| candidate.id == item) {
                        return Err(
                            AppFailure(format!("media list {input} has no item {item}")).into()
                        );
                    }
                    list.items.retain(|candidate| candidate.id != item);
                    if list.items.is_empty() {
                        return Err(
                            AppFailure("a media list keeps at least one item".into()).into()
                        );
                    }
                    if item == active {
                        keep_active = false;
                    }
                }
                crate::args::MediaListItemEdit::SetTitle { item, title } => {
                    let target = list
                        .items
                        .iter_mut()
                        .find(|candidate| candidate.id == item)
                        .ok_or_else(|| {
                            AppFailure(format!("media list {input} has no item {item}"))
                        })?;
                    target.title = title;
                }
                crate::args::MediaListItemEdit::SetMarks {
                    item,
                    mark_in,
                    mark_out,
                } => {
                    let target = list
                        .items
                        .iter_mut()
                        .find(|candidate| candidate.id == item)
                        .ok_or_else(|| {
                            AppFailure(format!("media list {input} has no item {item}"))
                        })?;
                    target.mark_in = mark_in;
                    target.mark_out = mark_out;
                }
            }
            mutate(
                &path,
                EngineCommand::MediaList(fm_engine::EngineMediaListCommand {
                    input,
                    operation: fm_engine::EngineMediaListOperation::Set {
                        items: list.items,
                        options: list.options,
                        active: keep_active.then_some(active),
                    },
                }),
                1,
                key,
                expected_revision,
            )?;
        }
        Command::InputMediaListImport {
            path,
            input,
            playlist,
        } => crate::media_list_import::import(&path, input, &playlist)?,
        Command::InputLifecycle {
            target,
            operation,
            key,
            expected_revision,
        } => match target {
            crate::args::MediaTarget::Local(path) => {
                if matches!(
                    operation,
                    fm_protocol::InputLifecycleOperation::Duplicate { .. }
                        | fm_protocol::InputLifecycleOperation::ReplaceSource { .. }
                        | fm_protocol::InputLifecycleOperation::Import { .. }
                        | fm_protocol::InputLifecycleOperation::RemoveInputs { .. }
                ) {
                    return Err(AppFailure(format!(
                        "{} changes the input catalogue; use input-duplicate, input-replace-*, or input-import-batch locally",
                        operation.payload()
                    ))
                    .into());
                }
                mutate(
                    &path,
                    EngineCommand::InputLifecycle(fm_control::engine_input_lifecycle_edit(
                        &operation,
                    )),
                    1,
                    key,
                    expected_revision,
                )?;
            }
            crate::args::MediaTarget::Remote(address) => remote::execute(
                address,
                fm_protocol::CommandPayload::InputLifecycle(operation),
                key,
                expected_revision,
            )?,
        },
        Command::InputImportBatch {
            path,
            first_input,
            files,
        } => project_bundle::import_batch(&path, first_input, &files)?,
        Command::InputMedia {
            target,
            input,
            operation,
            key,
            expected_revision,
        } => match target {
            crate::args::MediaTarget::Local(path) => {
                if operation.is_transport() {
                    return Err(AppFailure(format!(
                        "media {} drives a playing transport, which only a running freemixd has; use remote-input-media-{}",
                        operation.token(),
                        crate::args::cli_token(operation.token())
                    ))
                    .into());
                }
                mutate(
                    &path,
                    EngineCommand::Media(fm_engine::EngineMediaCommand {
                        input: input_id(input)?,
                        operation: fm_control::domain_media_operation(operation),
                    }),
                    1,
                    key,
                    expected_revision,
                )?;
            }
            crate::args::MediaTarget::Remote(address) => remote::execute(
                address,
                fm_protocol::CommandPayload::Media {
                    input: fm_protocol::WireInputId::from_domain(input_id(input)?),
                    operation,
                },
                key,
                expected_revision,
            )?,
        },
        Command::InputPage {
            path,
            input,
            navigation,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::PageNavigate {
                input: input_id(input)?,
                navigation,
            },
            1,
            key,
            expected_revision,
        )?,
        Command::InputPages { path } => {
            let project = inspect_engine(&path)?;
            for (input, document) in project.project.documents() {
                let status = project.engine.paged_status(input);
                let page = status.map_or(document.page, |status| status.page);
                let count = status
                    .and_then(|status| status.count)
                    .map_or_else(|| "unknown".to_owned(), |count| count.to_string());
                println!(
                    "input-page: input={input} page={} count={count} asset={}",
                    page.saturating_add(1),
                    document.asset_uri
                );
            }
        }
        Command::TransitionFavoriteSet {
            path,
            slot,
            effect,
            frames,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::SetTransitionFavorite {
                slot,
                favorite: model_favorite(effect, frames)?,
            },
            1,
            key,
            expected_revision,
        )?,
        Command::TransitionRenderFrame {
            path,
            kind,
            numerator,
            denominator,
            output,
            width,
            height,
        } => render_transition_frame(
            &path,
            kind,
            (numerator, denominator),
            &output,
            (width, height),
        )?,
        Command::RemoteTransition {
            address,
            kind,
            frames,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::Transition {
                kind,
                duration_frames: frames,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteTransitionFavoriteSet {
            address,
            slot,
            effect,
            frames,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::SetTransitionFavorite {
                slot,
                favorite: fm_protocol::TransitionFavoriteStatus {
                    effect,
                    duration_frames: frames,
                },
            },
            key,
            expected_revision,
        )?,
        Command::Outputs { path } => {
            let stored = inspect_stored_project(&path)?;
            print_outputs(stored.project());
        }
        Command::AudioBuses { path } => {
            let stored = inspect_stored_project(&path)?;
            print_audio_buses(stored.project());
        }
        Command::Scenes { path } => {
            let stored = inspect_stored_project(&path)?;
            print_scenes(stored.project());
        }
        Command::AsRun { path } => print_as_run(&path)?,
        Command::AssetAudit { path } => audit_assets(&path)?,
        Command::SettingsExport { path, output } => settings_export(&path, &output)?,
        Command::SettingsImport {
            path,
            input,
            apply,
        } => settings_import(&path, &input, apply)?,
        Command::TitleDesignCheck { path } => title_design_check(&path)?,
        Command::TitleApplyRow {
            path,
            input,
            row,
            keep_live,
        } => crate::title_row::apply_row(&path, input, &row, keep_live)?,
        Command::TitleDesignImport {
            path,
            input,
            name,
            design,
            fonts,
        } => project_bundle::title_design_import(&path, input, name, &design, &fonts)?,
        Command::AudioStrip {
            path,
            input,
            gain_millidb,
            balance_basis_points,
            pan_basis_points,
            muted,
            soloed,
            follow_video,
            delay_samples,
            sends,
        } => mutate(
            &path,
            EngineCommand::SetInputAudioStrip {
                input: input_id(input)?,
                state: EngineInputAudioStripState {
                    sends,
                    ..engine_audio_strip_state(
                        gain_millidb,
                        balance_basis_points,
                        pan_basis_points,
                        muted,
                        soloed,
                        follow_video,
                        delay_samples,
                    )?
                },
            },
            1,
            None,
            None,
        )?,
        Command::AudioPluginChain {
            path,
            target,
            slots,
        } => {
            let chain = local_plugin_chain(&slots)?;
            let target = match target {
                PluginChainTarget::Master => fm_types::strip_dsp::StripDspTarget::Master,
                PluginChainTarget::Input(input) => {
                    fm_types::strip_dsp::StripDspTarget::Input(input_id(input)?)
                }
            };
            mutate(
                &path,
                EngineCommand::AudioMixer(EngineAudioMixerEdit::SetPluginChain {
                    target,
                    chain: Box::new(chain),
                }),
                1,
                None,
                None,
            )?;
        }
        Command::Rename {
            path,
            input,
            name,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::RenameInput {
                input: input_id(input)?,
                name,
            },
            1,
            key,
            expected_revision,
        )?,
        Command::Reorder {
            path,
            inputs,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::ReorderInputs {
                inputs: inputs
                    .into_iter()
                    .map(input_id)
                    .collect::<AppResult<Vec<_>>>()?,
            },
            1,
            key,
            expected_revision,
        )?,
        Command::Preview {
            path,
            input,
            key,
            expected_revision,
        } => {
            let input = input_id(input)?;
            mutate(
                &path,
                EngineCommand::SelectPreview(input),
                1,
                key,
                expected_revision,
            )?;
        }
        Command::Cut {
            path,
            key,
            expected_revision,
        } => mutate(&path, EngineCommand::Cut, 1, key, expected_revision)?,
        Command::Fade {
            path,
            frames,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::Fade {
                duration_frames: frames,
            },
            frames,
            key,
            expected_revision,
        )?,
        Command::AlphaFade {
            path,
            frames,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::AlphaFade {
                duration_frames: frames,
            },
            frames,
            key,
            expected_revision,
        )?,
        Command::Slide {
            path,
            frames,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::Slide {
                duration_frames: frames,
            },
            frames,
            key,
            expected_revision,
        )?,
        Command::Zoom {
            path,
            frames,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::Zoom {
                duration_frames: frames,
            },
            frames,
            key,
            expected_revision,
        )?,
        Command::Stinger {
            path,
            slot,
            frames,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::Stinger {
                slot: StingerSlotId::new(slot).expect("CLI parser validates Stinger slots"),
                duration_frames: frames,
            },
            frames,
            key,
            expected_revision,
        )?,
        Command::StingerConfigure {
            path,
            slot,
            media_input,
            preload,
            cut_point_frames,
            audio_policy,
            fallback,
        } => {
            configure_stinger(
                &path,
                StingerConfig::new(
                    StingerSlotNumber::new(slot).expect("CLI parser validates Stinger slots"),
                    input_id(media_input)?,
                    preload,
                    cut_point_frames,
                    model_stinger_audio_policy(audio_policy),
                    model_stinger_fallback(fallback),
                ),
            )?;
        }
        Command::StingerRemove { path, slot } => {
            remove_stinger(
                &path,
                StingerSlotNumber::new(slot).expect("CLI parser validates Stinger slots"),
            )?;
        }
        Command::OverlayTake {
            path,
            channel,
            source,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::TakeOverlay {
                channel: OverlayChannelId::new(channel).expect("CLI parser validates overlays"),
                source: input_id(source)?,
            },
            1,
            key,
            expected_revision,
        )?,
        Command::OverlayUpdate {
            path,
            channel,
            source,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::UpdateOverlay {
                channel: OverlayChannelId::new(channel).expect("CLI parser validates overlays"),
                source: input_id(source)?,
            },
            1,
            key,
            expected_revision,
        )?,
        Command::OverlayOff {
            path,
            channel,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::OverlayOff {
                channel: OverlayChannelId::new(channel).expect("CLI parser validates overlays"),
            },
            1,
            key,
            expected_revision,
        )?,
        Command::OverlayOutput {
            path,
            channel,
            output,
            included,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::SetOverlayOutputInclusion {
                channel: OverlayChannelId::new(channel).expect("CLI parser validates overlays"),
                output: output_id(output)?,
                included,
            },
            1,
            key,
            expected_revision,
        )?,
        Command::OverlayTransition {
            path,
            channel,
            transition,
            frames,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::ConfigureOverlayTransition {
                channel: OverlayChannelId::new(channel).expect("CLI parser validates overlays"),
                transition: switcher_overlay_transition(transition),
                duration_frames: frames,
            },
            1,
            key,
            expected_revision,
        )?,
        Command::OverlayAppearance {
            path,
            channel,
            position,
            border,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::ConfigureOverlayAppearance {
                channel: OverlayChannelId::new(channel).expect("CLI parser validates overlays"),
                position: switcher_overlay_position(position),
                border: switcher_overlay_border(border),
            },
            1,
            key,
            expected_revision,
        )?,
        Command::OverlayQueue {
            path,
            channel,
            source,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::QueueOverlay {
                channel: OverlayChannelId::new(channel).expect("CLI parser validates overlays"),
                source: input_id(source)?,
            },
            1,
            key,
            expected_revision,
        )?,
        Command::OverlayNext {
            path,
            channel,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::TakeNextOverlay {
                channel: OverlayChannelId::new(channel).expect("CLI parser validates overlays"),
            },
            1,
            key,
            expected_revision,
        )?,
        Command::Wipe {
            path,
            frames,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::Wipe {
                duration_frames: frames,
            },
            frames,
            key,
            expected_revision,
        )?,
        Command::TBar {
            path,
            action,
            key,
            expected_revision,
        } => mutate(
            &path,
            engine_t_bar_command(action),
            1,
            key,
            expected_revision,
        )?,
        Command::AudioMixer {
            path,
            edit,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::AudioMixer(engine_audio_mixer_edit(edit)?),
            1,
            key,
            expected_revision,
        )?,
        Command::RemoteAudioMixer {
            address,
            edit,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::AudioMixer(wire_audio_mixer_operation(edit)?),
            key,
            expected_revision,
        )?,
        Command::FadeToBlackOutput {
            path,
            output,
            affected,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::SetFadeToBlackOutput {
                output: output_id(output)?,
                affected,
            },
            1,
            key,
            expected_revision,
        )?,
        Command::RemoteFadeToBlackOutput {
            address,
            output,
            affected,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::SetFadeToBlackOutput {
                output: fm_protocol::WireOutputId::from_domain(output_id(output)?),
                affected,
            },
            key,
            expected_revision,
        )?,
        Command::FadeToBlackOutputDuration {
            path,
            output,
            duration_frames,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::SetFadeToBlackOutputDuration {
                output: output_id(output)?,
                duration_frames,
            },
            1,
            key,
            expected_revision,
        )?,
        Command::RemoteFadeToBlackOutputDuration {
            address,
            output,
            duration_frames,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::SetFadeToBlackOutputDuration {
                output: fm_protocol::WireOutputId::from_domain(output_id(output)?),
                duration_frames,
            },
            key,
            expected_revision,
        )?,
        Command::FadeToBlackCurve {
            path,
            curve,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::SetFadeToBlackCurve { curve },
            1,
            key,
            expected_revision,
        )?,
        Command::RemoteFadeToBlackCurve {
            address,
            curve,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::SetFadeToBlackCurve { curve },
            key,
            expected_revision,
        )?,
        Command::FadeToBlack {
            path,
            target,
            frames,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::FadeToBlack {
                active: target.active(),
                duration_frames: frames,
            },
            frames,
            key,
            expected_revision,
        )?,
        Command::RemoteStatus { address } => remote::status(address)?,
        Command::RemoteDiagnostics { address } => remote::diagnostics(address)?,
        Command::RemoteAlert {
            address,
            rule,
            action,
        } => remote::alert(address, rule, action)?,
        Command::RemoteAlertRules { address } => remote::alert_rules(address)?,
        Command::RemoteAlertRule {
            address,
            rule,
            edit,
            key,
            expected_revision,
        } => remote::set_alert_rule(address, rule, &edit, key, expected_revision)?,
        Command::DisplayOutputSet {
            path,
            slot,
            config,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::SetDisplayOutput {
                slot,
                config: Some(config),
            },
            1,
            key,
            expected_revision,
        )?,
        Command::DisplayOutputClear {
            path,
            slot,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::SetDisplayOutput { slot, config: None },
            1,
            key,
            expected_revision,
        )?,
        Command::DisplayOutputList { path } => {
            let project = inspect_engine(&path)?;
            // A local project has no presenter: every slot waits for one.
            remote::print_display_outputs(project.engine.show().display_outputs().iter().map(
                |(slot, config)| {
                    (
                        *slot,
                        config,
                        fm_protocol::WireDisplayOutputState::Waiting(
                            fm_protocol::WaitReason::NoPresenter,
                        ),
                    )
                },
            ));
        }
        Command::RemoteDisplayOutputSet {
            address,
            slot,
            config,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::SetDisplayOutput { slot, config },
            key,
            expected_revision,
        )?,
        Command::RemoteDisplayOutputClear {
            address,
            slot,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::ClearDisplayOutput { slot },
            key,
            expected_revision,
        )?,
        Command::RemoteDisplayOutputList { address } => remote::display_outputs(address)?,
        Command::Replay { path } => {
            let project = inspect_engine(&path)?;
            let settings = project.engine.show().replay();
            remote::print_replay_settings(settings);
            // A local project holds the persisted settings; the recording,
            // marks, events, and players are engine runtime, so a fresh
            // inspection starts with none.
            let status = project.engine.replay_status();
            let feed = match project.engine.replay_feed() {
                fm_engine::EngineReplayFeed::Synthetic => fm_protocol::ReplayFeed::Synthetic,
                fm_engine::EngineReplayFeed::External => fm_protocol::ReplayFeed::Recorded,
            };
            let frame =
                |frame: Option<u64>| frame.map_or_else(|| "none".to_owned(), |f| f.to_string());
            println!(
                "Replay(feed={}, root={:?}, live_frame={}, mark_in={}, mark_out={})",
                feed.token(),
                settings.storage_root.as_deref().map_or_else(
                    || project.engine.replay_root().to_string_lossy().into_owned(),
                    str::to_owned,
                ),
                frame(status.live_frame),
                frame(status.mark_in),
                frame(status.mark_out),
            );
        }
        Command::ReplaySet {
            path,
            edit,
            key,
            expected_revision,
        } => {
            let project = inspect_engine(&path)?;
            let settings = remote::edited_replay(project.project.replay(), &edit)?;
            // Check the sources against the project before mutating: an
            // unknown or Replay input is a local argument error, and the
            // bundle stays untouched.
            settings
                .check(|input| project.project.replay_source_check(input))
                .map_err(crate::args::ArgsError::InvalidReplay)?;
            mutate(
                &path,
                EngineCommand::Replay(fm_engine::EngineReplayOperation::Configure(Box::new(
                    settings,
                ))),
                1,
                key,
                expected_revision,
            )?;
        }
        Command::RemoteReplay { address } => remote::replay(address)?,
        Command::RemoteReplaySet {
            address,
            edit,
            key,
            expected_revision,
        } => remote::set_replay(address, &edit, key, expected_revision)?,
        Command::RemoteReplayDo {
            address,
            operation,
            key,
        } => remote::replay_do(address, operation, key)?,
        Command::Reel { path } => {
            let project = inspect_engine(&path)?;
            let summaries = project.engine.reel_status().reels;
            let reels = project
                .project
                .reels()
                .iter()
                .map(|spec| {
                    let summary = summaries.iter().find(|summary| summary.id == spec.id);
                    fm_protocol::ReelStatus {
                        id: spec.id,
                        spec: spec.clone(),
                        total_frames: summary.and_then(|summary| summary.total_frames),
                        error: summary.and_then(|summary| summary.error.clone()),
                    }
                })
                .collect::<Vec<_>>();
            // Export jobs are engine runtime; a fresh local inspection
            // starts with none.
            remote::print_reels(&reels, &[]);
        }
        Command::ReelSet {
            path,
            spec,
            key,
            expected_revision,
        } => {
            let project = inspect_engine(&path)?;
            // Check the reel against the project before mutating: a bad
            // angle or clip list is a local argument error, and the bundle
            // stays untouched.
            spec.check(|input| {
                project.project.replay_source_check(input)
                    == fm_types::replay::ReplaySourceCheck::Recordable
            })
            .map_err(crate::args::ArgsError::InvalidReel)?;
            // A new reel beyond the project's eight is refused the same way.
            let replaces = project
                .project
                .reels()
                .iter()
                .any(|reel| reel.id == spec.id);
            if !replaces && project.project.reels().len() >= fm_protocol::MAX_REELS {
                return Err(crate::args::ArgsError::InvalidReel(
                    fm_protocol::ReelSpecError::TooManyReels,
                )
                .into());
            }
            mutate(
                &path,
                EngineCommand::Reel(fm_engine::EngineReelOperation::Set(spec)),
                1,
                key,
                expected_revision,
            )?;
        }
        Command::ReelDelete {
            path,
            id,
            key,
            expected_revision,
        } => mutate(
            &path,
            EngineCommand::Reel(fm_engine::EngineReelOperation::Delete { reel: id }),
            1,
            key,
            expected_revision,
        )?,
        Command::RemoteReel { address } => remote::reel(address)?,
        Command::RemoteReelSet {
            address,
            spec,
            key,
            expected_revision,
        } => remote::set_reel(address, spec, key, expected_revision)?,
        Command::RemoteReelDelete {
            address,
            id,
            key,
            expected_revision,
        } => remote::delete_reel(address, id, key, expected_revision)?,
        Command::RemoteReelExport {
            address,
            job,
            reel,
            destination,
            key,
            expected_revision,
        } => remote::export_reel(address, job, reel, destination, key, expected_revision)?,
        Command::RemoteReelCancel {
            address,
            job,
            key,
            expected_revision,
        } => remote::reel_control(
            address,
            fm_protocol::ReelOperation::CancelExport { job },
            key,
            expected_revision,
        )?,
        Command::RemoteReelForget {
            address,
            job,
            key,
            expected_revision,
        } => remote::reel_control(
            address,
            fm_protocol::ReelOperation::ForgetExport { job },
            key,
            expected_revision,
        )?,
        Command::LiveLan { path } => {
            let project = inspect_engine(&path)?;
            let config = project.engine.show().live_lan();
            // A local project has no media plane: an enabled stream waits.
            remote::print_live_lan(
                &fm_protocol::LiveLanEntry::from_config(config),
                &fm_protocol::LiveLanStatus::default().for_config(config.enabled),
            );
        }
        Command::LiveLanSet {
            path,
            edit,
            key,
            expected_revision,
        } => {
            let current = inspect_engine(&path)?.engine.show().live_lan().clone();
            let pin = match edit.pin {
                fm_protocol::SecretEdit::Keep => current.pin,
                fm_protocol::SecretEdit::Clear => None,
                fm_protocol::SecretEdit::Set(pin) => Some(
                    fm_types::live_lan::LiveLanPin::parse(&pin)
                        .map_err(|_| crate::args::ArgsError::InvalidLiveLanPin)?,
                ),
            };
            mutate(
                &path,
                EngineCommand::SetLiveLan(Box::new(fm_types::live_lan::LiveLanConfig {
                    enabled: edit.enabled.unwrap_or(current.enabled),
                    source: edit.source.unwrap_or(current.source),
                    max_viewers: edit.max_viewers.unwrap_or(current.max_viewers),
                    pin,
                })),
                1,
                key,
                expected_revision,
            )?;
        }
        Command::RemoteLiveLan { address } => remote::live_lan(address)?,
        Command::RemoteLiveLanSet {
            address,
            edit,
            key,
            expected_revision,
        } => remote::set_live_lan(address, &edit, key, expected_revision)?,
        Command::NetworkStreamInputAdd {
            path,
            input,
            name,
            config,
        } => {
            // Name the input already listening on the port (IN-016/IN-017),
            // as the engine does, rather than a bare validation count.
            if let Some(endpoint) = config.listen_endpoint()
                && let Some((holder, _)) = inspect_engine(&path)?
                    .engine
                    .show()
                    .network_streams()
                    .find(|(_, other)| other.listen_endpoint() == Some(endpoint))
            {
                return Err(
                    AppFailure(format!("input {holder} already listens on {endpoint}")).into(),
                );
            }
            add_input_with_kind(
                &path,
                input_id(input)?,
                name,
                InputKind::NetworkStream(*config),
            )?;
        }
        Command::RemoteNetworkStreamInputAdd {
            address,
            input,
            name,
            config,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::AddInput {
                input: fm_protocol::WireInputId::from_domain(input_id(input)?),
                name,
                source: fm_protocol::LiveInputSource::NetworkStream(config),
            },
            key,
            expected_revision,
        )?,
        Command::NetworkStreamSet {
            path,
            input,
            options,
            key,
            expected_revision,
        } => {
            let input = input_id(input)?;
            let current = inspect_engine(&path)?
                .engine
                .show()
                .network_stream(input)
                .cloned()
                .ok_or_else(|| AppFailure(format!("input {input} is not a network stream")))?;
            let config = options
                .configure(&current)
                .map_err(|error| AppFailure(format!("invalid network stream: {error}")))?;
            mutate(
                &path,
                EngineCommand::SetNetworkStream {
                    input,
                    config: Box::new(config),
                },
                1,
                key,
                expected_revision,
            )?;
        }
        Command::RemoteNetworkStreamSet {
            address,
            input,
            options,
            key,
            expected_revision,
        } => {
            remote::set_network_stream(
                address,
                input_id(input)?,
                &options,
                key,
                expected_revision,
            )?;
        }
        Command::NetworkStreamStatus { path } => {
            let project = inspect_engine(&path)?;
            // Nothing receives a project's streams: every one is idle.
            remote::print_network_streams(project.engine.show().network_streams().map(
                |(input, config)| fm_protocol::NetworkStreamEntry {
                    input: fm_protocol::WireInputId::from_domain(input),
                    config: config.redacted(),
                    status: fm_protocol::NetworkStreamStatus::default(),
                },
            ));
        }
        Command::RemoteNetworkStreamStatus { address } => remote::network_streams(address)?,
        Command::CaptureInputAdd {
            path,
            input,
            name,
            config,
        } => {
            let CaptureInputConfig {
                stable_key,
                settings,
            } = *config;
            add_input_with_kind(
                &path,
                input_id(input)?,
                name,
                InputKind::Device {
                    stable_key,
                    capture: settings,
                },
            )?;
        }
        Command::RemoteCaptureInputAdd {
            address,
            input,
            name,
            config,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::AddInput {
                input: fm_protocol::WireInputId::from_domain(input_id(input)?),
                name,
                source: fm_protocol::LiveInputSource::Capture(config),
            },
            key,
            expected_revision,
        )?,
        Command::CaptureSet {
            path,
            input,
            options,
            key,
            expected_revision,
        } => {
            let input = input_id(input)?;
            let current = inspect_engine(&path)?
                .engine
                .show()
                .capture_settings(input)
                .cloned()
                .ok_or_else(|| AppFailure(format!("input {input} is not a capture input")))?;
            mutate(
                &path,
                EngineCommand::SetCaptureSettings {
                    input,
                    settings: Box::new(options.apply(current)),
                },
                1,
                key,
                expected_revision,
            )?;
        }
        Command::RemoteCaptureSet {
            address,
            input,
            options,
            key,
            expected_revision,
        } => {
            remote::set_capture_settings(
                address,
                input_id(input)?,
                &options,
                key,
                expected_revision,
            )?;
        }
        Command::CaptureStatus { path } => {
            let project = inspect_engine(&path)?;
            // Nothing acquires a project's devices: every input is idle.
            remote::print_capture_inputs(project.engine.show().capture_inputs().map(
                |(input, stable_key, settings)| fm_protocol::CaptureInputEntry {
                    input: fm_protocol::WireInputId::from_domain(input),
                    config: CaptureInputConfig {
                        stable_key: stable_key.to_owned(),
                        settings: settings.clone(),
                    },
                    status: fm_protocol::CaptureStatus::default(),
                },
            ));
        }
        Command::RemoteCaptureStatus { address } => remote::capture_inputs(address)?,
        Command::RemoteCaptureDevices { address } => remote::capture_devices(address)?,
        Command::ScreenCaptureInputAdd {
            path,
            input,
            name,
            config,
        } => {
            add_input_with_kind(
                &path,
                input_id(input)?,
                name,
                InputKind::ScreenCapture(*config),
            )?;
        }
        Command::RemoteScreenCaptureInputAdd {
            address,
            input,
            name,
            config,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::AddInput {
                input: fm_protocol::WireInputId::from_domain(input_id(input)?),
                name,
                source: fm_protocol::LiveInputSource::ScreenCapture(config),
            },
            key,
            expected_revision,
        )?,
        Command::ScreenCaptureStatus { path } => {
            let project = inspect_engine(&path)?;
            // Nothing captures a project's screens: every input is idle.
            remote::print_screen_capture_inputs(project.engine.show().screen_capture_inputs().map(
                |(input, config)| fm_protocol::ScreenCaptureEntry {
                    input: fm_protocol::WireInputId::from_domain(input),
                    config: config.clone(),
                    status: fm_protocol::ScreenCaptureStatus::default(),
                },
            ));
        }
        Command::RemoteScreenCaptureStatus { address } => remote::screen_capture_inputs(address)?,
        Command::RemoteScreenCaptureTargets { address } => {
            remote::screen_capture_targets(address)?;
        }
        Command::RemoteScreenCapturePick { address, input } => {
            remote::screen_capture_pick(address, input_id(input)?)?;
        }
        Command::SystemAudioInputAdd {
            path,
            input,
            name,
            config,
        } => {
            add_input_with_kind(
                &path,
                input_id(input)?,
                name,
                InputKind::SystemAudio(*config),
            )?;
        }
        Command::RemoteSystemAudioInputAdd {
            address,
            input,
            name,
            config,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::AddInput {
                input: fm_protocol::WireInputId::from_domain(input_id(input)?),
                name,
                source: fm_protocol::LiveInputSource::SystemAudio(config),
            },
            key,
            expected_revision,
        )?,
        Command::SystemAudioStatus { path } => {
            let project = inspect_engine(&path)?;
            // Nothing captures a project's devices: every input is idle.
            remote::print_system_audio_inputs(project.engine.show().system_audio_inputs().map(
                |(input, config)| fm_protocol::SystemAudioEntry {
                    input: fm_protocol::WireInputId::from_domain(input),
                    config: config.clone(),
                    status: fm_protocol::SystemAudioStatus::default(),
                },
            ));
        }
        Command::ImageInputAdd {
            path,
            input,
            name,
            config,
        } => project_bundle::add_image_input(&path, input, name, *config)?,
        Command::SlideshowInputAdd {
            path,
            input,
            name,
            config,
        } => project_bundle::add_slideshow_input(&path, input, name, *config)?,
        Command::RemoteImageInputAdd {
            address,
            input,
            name,
            config,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::AddInput {
                input: fm_protocol::WireInputId::from_domain(input_id(input)?),
                name,
                source: fm_protocol::LiveInputSource::Image(config),
            },
            key,
            expected_revision,
        )?,
        Command::RemoteSlideshowInputAdd {
            address,
            input,
            name,
            config,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::AddInput {
                input: fm_protocol::WireInputId::from_domain(input_id(input)?),
                name,
                source: fm_protocol::LiveInputSource::Slideshow(config),
            },
            key,
            expected_revision,
        )?,
        Command::RemotePageNavigate {
            address,
            input,
            navigation,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::PageNavigate {
                input: fm_protocol::WireInputId::from_domain(input_id(input)?),
                navigation,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteSystemAudioStatus { address } => remote::system_audio_inputs(address)?,
        Command::RemoteSystemAudioDevices { address } => remote::system_audio_devices(address)?,
        Command::RemoteAudioOutputDevices { address } => remote::audio_output_devices(address)?,
        Command::RemoteDisplayInventory { address } => remote::display_inventory(address)?,
        Command::RemoteAudioStrip {
            address,
            input,
            gain_millidb,
            balance_basis_points,
            pan_basis_points,
            muted,
            soloed,
            follow_video,
            delay_samples,
            sends,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::SetInputAudioStrip {
                pan_basis_points: fm_model::InputPanBasisPoints::new(pan_basis_points)
                    .ok_or_else(|| AppFailure("pan must be in -10000..=10000 basis points".into()))?
                    .get(),
                input: fm_protocol::WireInputId::new(
                    NonZeroU128::new(input)
                        .ok_or_else(|| AppFailure("input ID must be nonzero".into()))?,
                ),
                gain_millidb: InputGainMilliDb::new(gain_millidb)
                    .ok_or_else(|| AppFailure("gain must be in -96000..=24000 millidB".into()))?
                    .get(),
                balance_basis_points: InputBalanceBasisPoints::new(balance_basis_points)
                    .ok_or_else(|| {
                        AppFailure("balance must be in -10000..=10000 basis points".into())
                    })?
                    .get(),
                muted,
                soloed,
                follow_video,
                delay_samples: InputDelaySamples::new(delay_samples)
                    .ok_or_else(|| AppFailure("delay samples must be in 0..=48000".into()))?
                    .get(),
                sends,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteRename {
            address,
            input,
            name,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::RenameInput {
                input: fm_protocol::WireInputId::new(
                    NonZeroU128::new(input)
                        .ok_or_else(|| AppFailure("input ID must be nonzero".into()))?,
                ),
                name,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteInputAdd {
            address,
            input,
            name,
            source,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::AddInput {
                input: fm_protocol::WireInputId::new(
                    NonZeroU128::new(input)
                        .ok_or_else(|| AppFailure("input ID must be nonzero".into()))?,
                ),
                name,
                source,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteInputRemove {
            address,
            input,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::RemoveInput {
                input: fm_protocol::WireInputId::new(
                    NonZeroU128::new(input)
                        .ok_or_else(|| AppFailure("input ID must be nonzero".into()))?,
                ),
            },
            key,
            expected_revision,
        )?,
        Command::RemoteReorder {
            address,
            inputs,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::ReorderInputs {
                inputs: inputs
                    .into_iter()
                    .map(|input| {
                        NonZeroU128::new(input)
                            .ok_or_else(|| AppFailure("input ID must be nonzero".into()))
                            .map(fm_protocol::WireInputId::new)
                            .map_err(|error| -> Box<dyn Error> { Box::new(error) })
                    })
                    .collect::<AppResult<Vec<_>>>()?,
            },
            key,
            expected_revision,
        )?,
        Command::RemotePreview {
            address,
            input,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::SelectPreview {
                input: fm_protocol::WireInputId::new(
                    NonZeroU128::new(input)
                        .ok_or_else(|| AppFailure("input ID must be nonzero".into()))?,
                ),
            },
            key,
            expected_revision,
        )?,
        Command::RemoteCut {
            address,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::Cut,
            key,
            expected_revision,
        )?,
        Command::RemotePlaylist {
            address,
            payload,
            key,
            expected_revision,
        }
        | Command::RemoteCue {
            address,
            payload,
            key,
            expected_revision,
        } => remote::execute(address, payload, key, expected_revision)?,
        Command::Cue {
            path,
            payload,
            key,
            expected_revision,
        } => cue(&path, &payload, key, expected_revision)?,
        Command::CueLists { path } => {
            let project = inspect_engine(&path)?;
            remote::print_cue_lists(
                project
                    .engine
                    .show()
                    .cue_lists()
                    .iter()
                    .map(fm_control::wire_cue_list)
                    .collect::<Vec<_>>()
                    .iter(),
            );
        }
        Command::RemoteCueLists { address } => remote::cue_lists(address)?,
        Command::Scripts { path } => {
            let project = inspect_engine(&path)?;
            let scripts = project
                .engine
                .show()
                .scripts()
                .iter()
                .map(fm_control::wire_script)
                .collect::<Vec<_>>();
            remote::print_scripts(scripts.iter().map(|script| {
                (
                    script,
                    fm_control::script_run_status(&project.engine, script.id),
                )
            }));
        }
        Command::RemoteScripts { address } => remote::scripts(address)?,
        Command::Shortcuts { path, command } => crate::shortcut_sets::local(&path, command)?,
        Command::RemoteShortcuts { address, command } => remote::shortcuts(address, command)?,
        Command::Script {
            path,
            command,
            key,
            expected_revision,
        } => script(&path, command, key, expected_revision)?,
        Command::RemoteScript {
            address,
            command,
            key,
            expected_revision,
        } => remote::script(
            address,
            script_payload(command, |name| {
                remote::script_id(address, name)?.ok_or_else(|| unknown_script(name).into())
            })?,
            key,
            expected_revision,
        )?,
        Command::RemoteUndo {
            address,
            redo,
            expected_edit,
            key,
            expected_revision,
        } => remote::execute(
            address,
            if redo {
                fm_protocol::CommandPayload::Redo { expected_edit }
            } else {
                fm_protocol::CommandPayload::Undo { expected_edit }
            },
            key,
            expected_revision,
        )?,
        Command::RemoteLock {
            address,
            locked,
            input,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::SetEditLock {
                scope: match input {
                    None => fm_protocol::EditLockScope::Project,
                    Some(input) => {
                        fm_protocol::EditLockScope::Input(fm_protocol::WireInputId::new(
                            NonZeroU128::new(input)
                                .ok_or_else(|| AppFailure("input ID must be nonzero".into()))?,
                        ))
                    }
                },
                locked,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteFade {
            address,
            frames,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::Fade {
                duration_frames: frames,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteAlphaFade {
            address,
            frames,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::AlphaFade {
                duration_frames: frames,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteSlide {
            address,
            frames,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::Slide {
                duration_frames: frames,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteZoom {
            address,
            frames,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::Zoom {
                duration_frames: frames,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteStinger {
            address,
            slot,
            frames,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::Stinger {
                slot: fm_protocol::WireStingerSlotId::new(slot)
                    .expect("CLI parser validates Stinger slots"),
                duration_frames: frames,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteStingerConfigure {
            address,
            slot,
            media_input,
            preload,
            cut_point_frames,
            audio_policy,
            fallback,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::ConfigureStinger {
                slot: fm_protocol::WireStingerSlotId::new(slot)
                    .expect("CLI parser validates Stinger slots"),
                media_input: fm_protocol::WireInputId::new(
                    NonZeroU128::new(media_input)
                        .ok_or_else(|| AppFailure("media input ID must be nonzero".into()))?,
                ),
                preload,
                cut_point_frames,
                audio_policy: protocol_stinger_audio_policy(audio_policy),
                missing_media_fallback: protocol_stinger_fallback(fallback),
            },
            key,
            expected_revision,
        )?,
        Command::RemoteStingerRemove {
            address,
            slot,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::RemoveStinger {
                slot: fm_protocol::WireStingerSlotId::new(slot)
                    .expect("CLI parser validates Stinger slots"),
            },
            key,
            expected_revision,
        )?,
        Command::RemoteOverlayTake {
            address,
            channel,
            source,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::TakeOverlay {
                channel: fm_protocol::WireOverlayChannelId::new(channel)
                    .expect("CLI parser validates overlays"),
                source: fm_protocol::WireInputId::new(
                    NonZeroU128::new(source)
                        .ok_or_else(|| AppFailure("input ID must be nonzero".into()))?,
                ),
            },
            key,
            expected_revision,
        )?,
        Command::RemoteOverlayUpdate {
            address,
            channel,
            source,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::UpdateOverlay {
                channel: fm_protocol::WireOverlayChannelId::new(channel)
                    .expect("CLI parser validates overlays"),
                source: fm_protocol::WireInputId::new(
                    NonZeroU128::new(source)
                        .ok_or_else(|| AppFailure("input ID must be nonzero".into()))?,
                ),
            },
            key,
            expected_revision,
        )?,
        Command::RemoteOverlayOff {
            address,
            channel,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::OverlayOff {
                channel: fm_protocol::WireOverlayChannelId::new(channel)
                    .expect("CLI parser validates overlays"),
            },
            key,
            expected_revision,
        )?,
        Command::RemoteOverlayOutput {
            address,
            channel,
            output,
            included,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::SetOverlayOutputInclusion {
                channel: fm_protocol::WireOverlayChannelId::new(channel)
                    .expect("CLI parser validates overlays"),
                output: fm_protocol::WireOutputId::new(
                    NonZeroU128::new(output)
                        .ok_or_else(|| AppFailure("output ID must be nonzero".into()))?,
                ),
                included,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteOverlayTransition {
            address,
            channel,
            transition,
            frames,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::ConfigureOverlayTransition {
                channel: fm_protocol::WireOverlayChannelId::new(channel)
                    .expect("CLI parser validates overlays"),
                transition: protocol_overlay_transition(transition),
                duration_frames: frames,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteOverlayAppearance {
            address,
            channel,
            position,
            border,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::ConfigureOverlayAppearance {
                channel: fm_protocol::WireOverlayChannelId::new(channel)
                    .expect("CLI parser validates overlays"),
                position: protocol_overlay_position(position),
                border: protocol_overlay_border(border),
            },
            key,
            expected_revision,
        )?,
        Command::RemoteOverlayQueue {
            address,
            channel,
            source,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::QueueOverlay {
                channel: fm_protocol::WireOverlayChannelId::new(channel)
                    .expect("CLI parser validates overlays"),
                source: fm_protocol::WireInputId::new(
                    NonZeroU128::new(source)
                        .ok_or_else(|| AppFailure("source input ID must be nonzero".into()))?,
                ),
            },
            key,
            expected_revision,
        )?,
        Command::RemoteOverlayNext {
            address,
            channel,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::TakeNextOverlay {
                channel: fm_protocol::WireOverlayChannelId::new(channel)
                    .expect("CLI parser validates overlays"),
            },
            key,
            expected_revision,
        )?,
        Command::RemoteWipe {
            address,
            frames,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::Wipe {
                duration_frames: frames,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteTBar {
            address,
            action,
            key,
            expected_revision,
        } => remote::execute(
            address,
            protocol_t_bar_payload(action),
            key,
            expected_revision,
        )?,
        Command::RemoteFadeToBlack {
            address,
            target,
            frames,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::FadeToBlack {
                active: target.active(),
                duration_frames: frames,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteStreamStart {
            address,
            stream,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::StreamStart {
                target: wire_stream_target(stream)?,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteStreamStop {
            address,
            stream,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::StreamStop {
                target: wire_stream_target(stream)?,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteStreamRetry {
            address,
            stream,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::StreamRetry {
                target: wire_stream_target(stream)?,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteStreamStartAll {
            address,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::StreamStartAll,
            key,
            expected_revision,
        )?,
        Command::RemoteStreamStopAll {
            address,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::StreamStopAll,
            key,
            expected_revision,
        )?,
        Command::RemoteStreamSet {
            address,
            stream,
            options,
            key,
            expected_revision,
        } => remote::set_stream_target(
            address,
            wire_stream_target(stream)?,
            &options,
            key,
            expected_revision,
        )?,
        Command::RemoteStreamRemove {
            address,
            stream,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::RemoveStreamTarget {
                target: wire_stream_target(stream)?,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteStreams { address } => remote::stream_targets(address)?,
        Command::RemoteRecordStart {
            address,
            recorder,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::RecordStart {
                recorder: wire_recorder(recorder)?,
            },
            key,
            expected_revision,
        )?,
        Command::RemoteRecordStop {
            address,
            recorder,
            key,
            expected_revision,
        } => remote::execute(
            address,
            fm_protocol::CommandPayload::RecordStop {
                recorder: wire_recorder(recorder)?,
            },
            key,
            expected_revision,
        )?,
        Command::Render {
            path,
            output,
            width,
            height,
        } => render(&path, &output, width, height)?,
        Command::Demo { path, output } => demo(&path, output.as_deref())?,
        Command::ProjectExportBundle {
            source,
            destination,
        } => project_bundle::export_bundle(&source, &destination)?,
        Command::ProjectFromTemplate {
            template,
            destination,
            name,
        } => project_bundle::from_template(&template, &destination, &name)?,
        Command::ProjectAppend { target, source } => project_bundle::append(&target, &source)?,
        Command::ProjectImportMedia {
            path,
            file,
            input,
            name,
        } => project_bundle::import_media(&path, &file, input, name)?,
        Command::ProjectRelink {
            path,
            search,
            apply,
        } => project_bundle::relink(&path, search, apply)?,
        Command::TitleInputAdd {
            path,
            input,
            name,
            template,
            fonts,
        } => project_bundle::title_input_add(&path, input, name, &template, &fonts)?,
        Command::TitleAnimationPreset {
            path,
            template_uri,
            element,
            phase,
            preset,
            frames,
        } => title_animation::preset(&path, &template_uri, element, phase, &preset, frames)?,
        Command::TitleAnimationStretch {
            path,
            template_uri,
            phase,
            frames,
        } => title_animation::stretch(&path, &template_uri, phase, frames)?,
        Command::TitleRenderFrame {
            path,
            input,
            phase,
            frame,
            at,
            output,
        } => title_animation::render_frame(&path, input, phase, frame, at.as_deref(), &output)?,
        Command::SceneAnimationPreset {
            path,
            scene,
            layer,
            phase,
            preset,
            frames,
        } => scene_animation::preset(&path, scene, layer, phase, &preset, frames)?,
        Command::SceneAnimationStretch {
            path,
            scene,
            phase,
            frames,
        } => scene_animation::stretch(&path, scene, phase, frames)?,
        Command::SceneRenderFrame {
            path,
            scene,
            phase,
            frame,
            output,
            width,
            height,
        } => scene_animation::render_frame(&path, scene, phase, frame, &output, width, height)?,
        Command::GradeSet {
            path,
            target,
            library,
            spec,
            save_as,
        } => crate::grading::set(&path, target, library, &spec, save_as)?,
        Command::GradeClear { path, target } => crate::grading::clear(&path, target)?,
        Command::GradeImportLut { path, file, name } => {
            crate::grading::import_lut(&path, &file, name)?;
        }
        Command::GradeList { path } => crate::grading::list(&path)?,
        Command::GradeExport {
            path,
            grade,
            file,
            replace_lut,
        } => {
            crate::presets::export_grade(&path, grade, &file, replace_lut)?;
        }
        Command::GradeImport { path, file, name } => {
            crate::presets::import_grade(&path, &file, name.as_deref())?;
        }
        Command::PresetExport { path, preset, file } => {
            crate::presets::export_animation(&path, preset, &file)?;
        }
        Command::PresetSave {
            path,
            template_uri,
            element,
            phase,
            name,
        } => crate::title_animation::save_preset(&path, &template_uri, element, phase, &name)?,
        Command::PresetImport { path, file, name } => {
            crate::presets::import_animation(&path, &file, name.as_deref())?;
        }
        Command::ProjectAutosave { path, keep } => project_bundle::autosave(&path, keep)?,
        Command::ProjectRecover { path, restore } => project_bundle::recover(&path, restore)?,
        Command::RecentProjects { file } => project_bundle::list_recent(&file)?,
        Command::RecentProjectsAdd {
            file,
            project,
            capacity,
        } => project_bundle::add_recent(&file, &project, capacity)?,
        Command::RecentProjectsRemove { file, project } => {
            project_bundle::remove_recent(&file, &project)?;
        }
        Command::RecentProjectsPrune { file } => project_bundle::prune_recent(&file)?,
        Command::Help => print_help(),
    }
    Ok(())
}

fn rename_project(path: &Path, name: String) -> AppResult<()> {
    update_project(path, |project| {
        project.rename_project(name).map_err(Into::into)
    })
}

fn default_project(name: String) -> AppResult<ProjectEngine> {
    let first = input_id(1)?;
    let second = input_id(2)?;
    let frame_rate = FrameRate::new(60_000, 1_001)?;
    let settings = ProjectSettings {
        frame_rate,
        video: VideoFormat {
            dimensions: VideoDimensions::new(1_920, 1_080)
                .ok_or_else(|| AppFailure("default video dimensions are invalid".into()))?,
            frame_rate,
            pixel_format: PixelFormat::Nv12,
            scan: ScanMode::Progressive,
            color: ColorMetadata::default(),
        },
        audio: AudioFormat {
            sample_rate: SampleRate::new(48_000)
                .ok_or_else(|| AppFailure("default audio sample rate is invalid".into()))?,
            sample_format: SampleFormat::F32,
            channels: ChannelLayout::stereo(),
        },
    };
    let mut project = Project::new(generate_project_id()?, name, settings)
        .with_main_mix(MainMix::new(first, second));
    project.add_input(Input {
        id: first,
        name: "Input 1".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(73, 151, 199, u8::MAX)),
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: second,
        name: "Input 2".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(146, 46, 142, u8::MAX)),
            SimulatedAudio::Sine {
                frequency_hz: 1_000,
            },
        )),
        required_capabilities: Vec::new(),
    });
    project
        .validate()
        .map_err(|errors| AppFailure(format!("default project is invalid: {errors:?}")))?;
    engine_from_project(project)
}

fn engine_from_project(project: Project) -> AppResult<ProjectEngine> {
    let main_mix = required_main_mix(&project)?;
    let inputs = project
        .inputs()
        .iter()
        .map(|input| (input.id, input.name.clone()))
        .collect();
    let mut show = ShowState::new(
        project.name(),
        inputs,
        main_mix.desired_program,
        main_mix.desired_preview,
    )?
    .with_outputs(
        project
            .outputs()
            .iter()
            .map(|output| (output.id, output.name.clone()))
            .collect(),
    )?
    .with_stream_targets(project.stream_targets())?;
    restore_input_audio_strips(&mut show, &project)?;
    restore_display_outputs(&mut show, &project)?;
    let engine = Engine::new(show, project.settings().frame_rate, clock_domain());
    Ok(ProjectEngine { project, engine })
}

pub(crate) fn mutate(
    path: &Path,
    command: EngineCommand,
    ticks: u32,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> AppResult<()> {
    let mut project = load_engine(path)?;
    let ticks = match &command {
        EngineCommand::FadeToBlack { active, .. }
            if project.engine.realized_fade_to_black().active == *active =>
        {
            1
        }
        EngineCommand::TakeOverlay { channel, .. }
        | EngineCommand::TakeNextOverlay { channel }
        | EngineCommand::OverlayOff { channel } => {
            let overlay = project.engine.show().desired_switcher().overlay(*channel);
            match overlay.transition() {
                OverlayTransitionKind::Cut => 1,
                _ => overlay.duration_frames(),
            }
        }
        _ => ticks,
    };
    let result = execute(&mut project.engine, command, ticks, key, expected_revision)?;
    if result.replayed {
        if let Some(rejection) = result.rejection {
            return Err(rejection.into());
        }
        print_status(&project);
        return Ok(());
    }
    save_engine(path, &project)?;
    if let Some(rejection) = result.rejection {
        return Err(rejection.into());
    }
    print_status(&project);
    Ok(())
}

/// Applies a cue list edit or cue control to a local project (RC-017).
///
/// The lists and their run state (standby, played cues) are saved with the
/// project, so successive local GOs walk the list. After a GO the simulated
/// cue clock runs the cue to its end: each wait is ticked
/// through and its advance submitted on its due frame, exactly as the daemon
/// clock does, and the last transition is ticked until it settles.
fn cue(
    path: &Path,
    payload: &fm_protocol::CommandPayload,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> AppResult<()> {
    let mut project = load_engine(path)?;
    // A shortcut fire (RC-004) travels as a cue command does.
    let command = if matches!(payload, fm_protocol::CommandPayload::FireShortcut { .. }) {
        fm_control::shortcut_engine_command(payload)
    } else {
        fm_control::cue_engine_command(payload)
    };
    let mut result = execute(&mut project.engine, command, 1, key, expected_revision)?;
    if result.replayed {
        if let Some(rejection) = result.rejection {
            return Err(rejection.into());
        }
        print_status(&project);
        return Ok(());
    }
    if result.rejection.is_none() {
        let advanced = run_cue_clock(&mut project.engine)?;
        result.cue_scripts.extend(advanced);
    }
    save_engine(path, &project)?;
    if let Some(rejection) = result.rejection {
        return Err(rejection.into());
    }
    // Each script step the GO and its waits ran (Protocol 2.61).
    for ran in result.cue_scripts {
        ran.print();
    }
    print_status(&project);
    Ok(())
}

fn unknown_script(name: &str) -> AppFailure {
    AppFailure(format!("no script is named {name:?}"))
}

/// The wire command of a script command line, reading a new source from
/// its file (or standard input for `-`) and resolving a script named by
/// name through `resolve`.
pub(crate) fn script_payload(
    command: crate::args::ScriptCommand,
    resolve: impl FnOnce(&str) -> AppResult<u64>,
) -> AppResult<fm_protocol::CommandPayload> {
    use crate::args::{ScriptCommand, ScriptRef};
    use fm_protocol::{CommandPayload, ScriptSpec};

    Ok(match command {
        ScriptCommand::Set {
            script,
            name,
            source,
        } => {
            let source = if source.as_os_str() == "-" {
                io::read_to_string(io::stdin())?
            } else {
                fs::read_to_string(&source)?
            };
            CommandPayload::SetScript {
                script,
                spec: Box::new(ScriptSpec { name, source }),
                position: None,
            }
        }
        ScriptCommand::Remove { script } => CommandPayload::RemoveScript { script },
        ScriptCommand::Run {
            script: ScriptRef::Id(script),
        } => CommandPayload::RunScript { script },
        ScriptCommand::Run {
            script: ScriptRef::Name(name),
        } => CommandPayload::RunScript {
            script: resolve(&name)?,
        },
        ScriptCommand::Stop { runs } => CommandPayload::StopScript { runs },
    })
}

/// Edits, runs, or stops one script of a local project (RC-011). A run
/// performs its actions on one frame, all or nothing, then plays any
/// transition it started to its end; an accepted run prints
/// `ScriptRan(...)` and its `ScriptRun(...)` state. A script that waits
/// (Protocol 2.62) runs to its end on the simulated clock, as a GO's cue
/// waits do: each part is resumed on its due frame and prints its own
/// `ScriptRan(...)`, and a part that fails leaves the run faulted with its
/// typed reason, which `script-stop` clears.
fn script(
    path: &Path,
    command: crate::args::ScriptCommand,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> AppResult<()> {
    let mut project = load_engine(path)?;
    let payload = script_payload(command, |name| {
        project
            .engine
            .show()
            .scripts()
            .iter()
            .find(|script| script.name == name)
            .map(|script| script.id)
            .ok_or_else(|| unknown_script(name).into())
    })?;
    let command = fm_control::script_engine_command(&payload);
    let result = execute(&mut project.engine, command, 1, key, expected_revision)?;
    if result.replayed {
        if let Some(rejection) = result.rejection {
            return Err(rejection.into());
        }
        print_status(&project);
        return Ok(());
    }
    let mut parts = Vec::new();
    if result.rejection.is_none() {
        parts = run_cue_clock(&mut project.engine)?;
    }
    save_engine(path, &project)?;
    if let Some(rejection) = result.rejection {
        return Err(rejection.into());
    }
    if let fm_protocol::CommandPayload::RunScript { script } = &payload {
        if let Some(ran) = result.script_ran {
            remote::print_script_ran(ran.0, ran.1, ran.2, ran.3);
        }
        // The parts after each wait, resumed on their due frames.
        for ran in parts {
            ran.print();
        }
        remote::print_script_run(
            *script,
            &fm_control::script_run_status(&project.engine, *script),
        );
    }
    print_status(&project);
    Ok(())
}

/// Runs every waiting cue to its end on the simulated clock, returning the
/// script steps the advances ran.
///
/// The clock is bounded defensively, whatever the restored project holds:
/// every wait is at most the longest cue wait, delay, script wait, or armed
/// GO delay (Protocol 2.64) at the engine rate, each advance runs at least
/// one wait of a list (each list holds at most
/// [`fm_model::MAX_CUE_LIST_ACTIONS`]), a list's armed GO (at most one per
/// list), or a script part (at most [`fm_model::MAX_SCRIPT_WAITS`] per
/// run, at most [`fm_model::MAX_SCRIPT_RUNS`] runs, Protocol 2.69), and
/// nothing a cue or a script step runs starts another waiting run. A clock beyond that bound is refused instead of ticking for ever.
fn run_cue_clock(engine: &mut Engine) -> AppResult<Vec<CueScriptRan>> {
    let rate = engine.frame_rate();
    let longest_wait = u64::from(
        fm_model::MAX_CUE_WAIT_FRAMES
            .max(fm_model::delay_frames(fm_model::MAX_CUE_DELAY_MS, rate))
            .max(fm_model::delay_frames(fm_model::MAX_CUE_ARM_MS, rate))
            .max(fm_model::script_wait_frames(rate)),
    );
    let max_advances =
        u64::try_from(fm_model::MAX_CUE_LISTS * (fm_model::MAX_CUE_LIST_ACTIONS + 1))
            .unwrap_or(u64::MAX)
            .saturating_add(
                u64::from(fm_model::MAX_SCRIPT_WAITS) * fm_model::MAX_SCRIPT_RUNS as u64,
            );
    let mut advances = 0_u64;
    let mut cue_scripts = Vec::new();
    while let Some((list, due)) = engine.cue_advance_frame() {
        advances += 1;
        let cursor = engine.frame_cursor().get();
        if advances > max_advances || due.saturating_sub(cursor) > longest_wait {
            return Err(AppFailure(format!(
                "the cue clock does not settle: cue list {list} is due on frame {due} \
                 (frame {cursor}, advance {advances})"
            ))
            .into());
        }
        while engine.frame_cursor().get() < due {
            engine.tick()?;
        }
        let advance = execute(
            engine,
            EngineCommand::CueAdvance { list, frame: due },
            1,
            None,
            None,
        )?;
        if let Some(rejection) = advance.rejection {
            return Err(rejection.into());
        }
        cue_scripts.extend(advance.cue_scripts);
        // List 0 resumes the waiting script run (Protocol 2.62).
        if let Some((script, frame, actions, instructions)) = advance.script_ran {
            cue_scripts.push(CueScriptRan {
                frame,
                run: fm_engine::CueScriptRun {
                    script,
                    actions,
                    instructions,
                },
            });
        }
    }
    for _ in 0..=fm_model::MAX_CUE_TRANSITION_FRAMES {
        if engine.realized_switcher() == engine.show().desired_switcher() {
            break;
        }
        engine.tick()?;
    }
    Ok(cue_scripts)
}

fn execute(
    engine: &mut Engine,
    command: EngineCommand,
    ticks: u32,
    key: Option<String>,
    expected_revision: Option<u64>,
) -> AppResult<ExecuteResult> {
    let key = match key {
        Some(key) => key,
        None => generate_implicit_key()?,
    };
    let mut envelope = CommandEnvelope::new(format!("command-{key}"), key, command);
    if let Some(revision) = expected_revision {
        envelope = envelope.expecting(Revision::new(revision));
    }
    let outcome = engine.execute(envelope, 0)?;
    let script_ran = outcome
        .events
        .iter()
        .find_map(|event| match &event.payload {
            fm_engine::EngineEvent::ScriptRan {
                script,
                frame,
                actions,
                instructions,
                ..
            } => Some((*script, *frame, *actions, *instructions)),
            _ => None,
        });
    let cue_scripts = outcome
        .events
        .iter()
        .flat_map(|event| match &event.payload {
            fm_engine::EngineEvent::CueListChanged { frame, scripts, .. } => scripts
                .iter()
                .map(|run| CueScriptRan {
                    frame: *frame,
                    run: *run,
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    let rejection = outcome.receipt.rejected().map(|rejected| {
        AppFailure(crate::remote::refusal_text(
            rejected.rejection.code.as_str(),
            &rejected.rejection.message,
            rejected.rejection.reason.as_ref(),
        ))
    });
    if rejection.is_none() && !outcome.replayed {
        for _ in 0..ticks {
            engine.tick()?;
        }
    }
    Ok(ExecuteResult {
        replayed: outcome.replayed,
        rejection,
        script_ran,
        cue_scripts,
    })
}

/// Generates a unique key for a command run without `--key`.
///
/// The key only has to be unique, so it is a fixed-width timestamp, process id
/// and in-process sequence; it deliberately carries no command payload, which
/// would make every persisted receipt as large as the command's debug form.
fn generate_implicit_key() -> AppResult<String> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let sequence = IMPLICIT_KEY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(format!(
        "cli:{timestamp:032x}:{:08x}:{sequence:016x}",
        std::process::id()
    ))
}

/// Selects the newest [`MAX_PERSISTED_RECEIPTS`] receipts, in key order.
///
/// Recency is the revision a receipt was recorded at; a rejection recorded at
/// revision `r` happened after the command that produced `r`, so it ranks
/// above that accepted receipt.
fn retained_receipts(
    receipts: &[(IdempotencyKey, CommandReceipt<EngineAcceptance>)],
) -> Vec<&(IdempotencyKey, CommandReceipt<EngineAcceptance>)> {
    let mut retained = receipts.iter().collect::<Vec<_>>();
    if retained.len() <= MAX_PERSISTED_RECEIPTS {
        return retained;
    }
    retained.sort_by_key(|(key, receipt)| {
        let recency = match receipt {
            CommandReceipt::Accepted { acceptance, .. } => (acceptance.revision.get(), false),
            CommandReceipt::Rejected { rejection, .. } => (rejection.current_revision.get(), true),
        };
        (recency, key.as_str())
    });
    let mut retained = retained.split_off(retained.len() - MAX_PERSISTED_RECEIPTS);
    retained.sort_by_key(|(key, _)| key.as_str());
    retained
}

fn persisted_receipt(
    (key, receipt): &(IdempotencyKey, CommandReceipt<EngineAcceptance>),
) -> AppResult<IdempotencyReceipt> {
    Ok(match receipt {
        CommandReceipt::Accepted {
            command_id,
            acceptance,
        } => IdempotencyReceipt::accepted(
            key.as_str(),
            command_id.as_str(),
            acceptance.revision.get(),
            acceptance.result.target_frame.get(),
        ),
        CommandReceipt::Rejected {
            command_id,
            rejection,
        } => IdempotencyReceipt::rejected(
            key.as_str(),
            command_id.as_str(),
            rejection.current_revision.get(),
            persisted_rejection_code(rejection.rejection.code)?,
            &rejection.rejection.message,
            rejection.rejection.retryable,
        ),
    })
}

fn runtime_receipt(
    receipt: &IdempotencyReceipt,
) -> AppResult<(IdempotencyKey, CommandReceipt<EngineAcceptance>)> {
    let command_id = CommandId::new(receipt.command_id());
    let runtime = match receipt.outcome() {
        ReceiptOutcome::Accepted {
            revision,
            target_frame,
        } => CommandReceipt::Accepted {
            command_id,
            acceptance: AcceptedReceipt {
                revision: Revision::new(*revision),
                result: EngineAcceptance {
                    target_frame: (*target_frame).into(),
                },
            },
        },
        ReceiptOutcome::Rejected {
            current_revision,
            code,
            message,
            retryable,
        } => CommandReceipt::Rejected {
            command_id,
            rejection: RejectedReceipt {
                rejection: {
                    let code = runtime_rejection_code(code)?;
                    // Persisted receipts keep code and message; the reasons
                    // a code alone implies are restored (Protocol 2.45).
                    let rejection = Rejection::new(code, message).retryable(*retryable);
                    match fm_command::RefusalReason::implied_by(code, *current_revision) {
                        Some(reason) => rejection.with_reason(reason),
                        None => rejection,
                    }
                },
                current_revision: Revision::new(*current_revision),
            },
        },
    };
    Ok((IdempotencyKey::new(receipt.key()), runtime))
}

fn persisted_rejection_code(code: RejectionCode) -> AppResult<&'static str> {
    let stable = match code {
        RejectionCode::PermissionDenied => "permission_denied",
        RejectionCode::DeadlineExceeded => "deadline_exceeded",
        RejectionCode::RevisionConflict => "revision_conflict",
        RejectionCode::InvalidCommand => "invalid_command",
        RejectionCode::NotFound => "not_found",
        RejectionCode::Conflict => "conflict",
        RejectionCode::Unavailable => "unavailable",
        RejectionCode::ResourceExhausted => "resource_exhausted",
        RejectionCode::Internal => "internal",
        _ => {
            return Err(AppFailure(format!(
                "cannot persist unknown rejection code `{}`",
                code.as_str()
            ))
            .into());
        }
    };
    Ok(stable)
}

fn runtime_rejection_code(code: &str) -> AppResult<RejectionCode> {
    match code {
        "permission_denied" => Ok(RejectionCode::PermissionDenied),
        "deadline_exceeded" => Ok(RejectionCode::DeadlineExceeded),
        "revision_conflict" => Ok(RejectionCode::RevisionConflict),
        "invalid_command" => Ok(RejectionCode::InvalidCommand),
        "not_found" => Ok(RejectionCode::NotFound),
        "conflict" => Ok(RejectionCode::Conflict),
        "unavailable" => Ok(RejectionCode::Unavailable),
        "resource_exhausted" => Ok(RejectionCode::ResourceExhausted),
        "internal" => Ok(RejectionCode::Internal),
        _ => Err(AppFailure(format!("project contains unknown rejection code `{code}`")).into()),
    }
}

fn save_engine(path: &Path, project_engine: &ProjectEngine) -> AppResult<()> {
    let snapshot = project_engine.engine.snapshot()?;
    let desired = snapshot.show().desired_switcher();
    let realized = snapshot.realized_switcher();
    let mut project = project_engine.project.clone();
    project.set_main_mix(MainMix::new(desired.program(), desired.preview()));
    project.reorder_inputs(snapshot.show().inputs())?;
    sync_input_names(&mut project, snapshot.show())?;
    sync_input_audio_strips(&mut project, snapshot.show())?;
    sync_audio_mixer(&mut project, snapshot.show())?;
    sync_display_outputs(&mut project, snapshot.show())?;
    sync_stream_running(&mut project, snapshot.show())?;
    sync_recording_desired(&mut project, snapshot.show());
    sync_input_lifecycle(&mut project, snapshot.show());
    sync_network_streams(&mut project, snapshot.show())?;
    sync_capture_inputs(&mut project, snapshot.show())?;
    project.set_transition_favorites(*snapshot.show().transition_favorites());
    project.set_cue_lists(snapshot.show().cue_lists().to_vec());
    project.set_scripts(snapshot.show().scripts().to_vec());
    project.set_script_run(snapshot.show().script_run().clone());
    project.set_shortcuts(snapshot.show().shortcuts().to_vec());
    project.set_media_lists(snapshot.show().media_lists().clone());
    project.set_title_timelines(snapshot.title_timelines().clone());
    // The project's highlight reels (OR-013).
    project.restore_reels(snapshot.show().reels().to_vec());
    sync_title_fields(&mut project, snapshot.show())?;
    // A document's page lives beside its asset (IN-014).
    for (&input, &page) in snapshot.show().paged_inputs() {
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
    for (&input, settings) in snapshot.show().media_inputs() {
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
    let stored = StoredProject::from_project_with_complete_runtime_state(
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
        ProjectPosition {
            revision: snapshot.revision().get(),
            state_epoch: snapshot.state_epoch().get(),
            event_sequence: snapshot.event_sequence().get(),
            frames_rendered: snapshot.frames_rendered(),
            runtime_generation: snapshot.runtime_generation().get(),
            clock_time_nanos: snapshot.clock_time().as_nanos(),
        },
        retained_receipts(snapshot.receipts())
            .into_iter()
            .map(persisted_receipt)
            .collect::<AppResult<Vec<_>>>()?,
    )?;
    ProjectStore::new(path)?.save(&stored)?;
    Ok(())
}

/// Gives every title input of `project` its stored template and field
/// values in `show`, so cue title actions (RC-017) name a title the engine
/// knows, as on the daemon. A title whose template does not load (a damaged
/// bundle) is left out, so the commands that repair the project still run;
/// a cue list naming it is then refused when the lists are restored.
fn restore_titles(path: &Path, show: &mut ShowState, project: &Project) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    for input in project.inputs() {
        let InputKind::Title(title) = &input.kind else {
            continue;
        };
        let Ok(stored) = store.load_title_template(&title.template_uri) else {
            continue;
        };
        let Ok(fields) = fm_titles::TitleFieldSet::restore_revisioned(
            &stored.template,
            title.field_revision,
            title.fields.iter().map(|entry| {
                (
                    fm_titles::FieldId::new(entry.id.get()),
                    fm_title_render::field_value(&entry.value),
                    entry.revision,
                )
            }),
        ) else {
            continue;
        };
        show.set_title(
            input.id,
            fm_engine::EngineTitle::new(stored.template.name.clone(), fields)
                .with_template(title.template_uri.clone(), stored.template),
        )?;
    }
    Ok(())
}

/// Copies every title's field values and revisions the engine changed (a
/// cue's title text) into the project.
fn sync_title_fields(project: &mut Project, show: &ShowState) -> AppResult<()> {
    for (&input, title) in show.titles() {
        let fields = title
            .fields
            .values()
            .iter()
            .map(|(field, value)| fm_model::TitleFieldEntry {
                id: fm_model::TitleFieldId::new(field.get()),
                value: fm_title_render::model_field_value(value),
                revision: title.fields.field_revision(*field).unwrap_or(0),
            })
            .collect();
        project
            .set_title_fields(input, title.fields.revision(), fields)
            .map_err(|error| {
                AppFailure(format!(
                    "title input {input} fields could not be stored: {error:?}"
                ))
            })?;
    }
    Ok(())
}

/// Copies each network stream input's configuration the engine edited into
/// the project (IN-015).
fn sync_network_streams(project: &mut Project, show: &ShowState) -> AppResult<()> {
    for (input, config) in show.network_streams() {
        let Some(current) = project
            .inputs()
            .iter()
            .find(|candidate| candidate.id == input)
        else {
            continue;
        };
        if matches!(&current.kind, InputKind::NetworkStream(stored) if stored == config) {
            continue;
        }
        let capabilities = current.required_capabilities.clone();
        project
            .replace_input_source(
                input,
                InputKind::NetworkStream(config.clone()),
                capabilities,
            )
            .map_err(|error| {
                AppFailure(format!(
                    "input {input}'s network stream could not be stored: {error}"
                ))
            })?;
    }
    Ok(())
}

/// Copies each capture input's settings the engine edited into the project
/// (IN-001).
fn sync_capture_inputs(project: &mut Project, show: &ShowState) -> AppResult<()> {
    for (input, _, settings) in show.capture_inputs() {
        if project
            .capture_input(input)
            .is_none_or(|(_, stored)| stored == settings)
        {
            continue;
        }
        project
            .set_capture_settings(input, settings.clone())
            .map_err(|error| {
                AppFailure(format!(
                    "input {input}'s capture settings could not be stored: {error}"
                ))
            })?;
    }
    Ok(())
}

/// Copies the engine's category list and every input's lifecycle into the
/// project (IN-026).
fn sync_input_lifecycle(project: &mut Project, show: &ShowState) {
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
}

fn sync_input_names(project: &mut Project, show: &ShowState) -> AppResult<()> {
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
    Ok(())
}

/// Copies the engine's desired stream running flags into the authored
/// destinations so the checkpoint persists them like every other desired
/// switcher field.
fn sync_stream_running(project: &mut Project, show: &ShowState) -> AppResult<()> {
    for target in project.stream_targets().to_vec() {
        let running = show
            .stream_running(switcher_stream_target(target.id()))
            .ok_or_else(|| {
                AppFailure(format!(
                    "project is missing engine stream target {}",
                    target.id()
                ))
            })?;
        if running != target.running() {
            project
                .replace_stream_target(target.set_running(running))
                .map_err(|error| AppFailure(error.to_string()))?;
        }
    }
    Ok(())
}

/// Copies each recorder slot's desired flag from the engine into the authored
/// project so the checkpoint persists it like every other desired switcher
/// field.
fn sync_recording_desired(project: &mut Project, show: &ShowState) {
    for slot in ModelRecorderSlot::ALL {
        let active = show
            .desired_switcher()
            .recording_desired(switcher_recorder_slot(slot));
        if active != project.recording_desired(slot) {
            project.set_recording_desired(slot, active);
        }
    }
}

fn configure_stinger(path: &Path, config: StingerConfig) -> AppResult<()> {
    update_project(path, |project| {
        project.set_stinger_checked(config).map_err(Into::into)
    })
}

fn add_input(path: &Path, input: InputId, name: String) -> AppResult<()> {
    add_input_with_kind(
        path,
        input,
        name,
        InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
    )
}

pub(crate) fn add_input_with_kind(
    path: &Path,
    input: InputId,
    name: String,
    kind: InputKind,
) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.add_input_checked(Input {
        id: input,
        name,
        kind,
        required_capabilities: Vec::new(),
    })?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn relink_media_input(path: &Path, input: InputId, asset_uri: String) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.relink_media_input(input, asset_uri)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    ProjectStore::new(path)?.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn add_scene_input(path: &Path, input: InputId, scene: SceneId, name: String) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.add_scene_input_checked(scene, name.clone(), input, name)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    ProjectStore::new(path)?.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn duplicate_scene_input(
    path: &Path,
    source_scene: SceneId,
    new_scene: SceneId,
    new_input: InputId,
    scene_name: String,
    input_name: String,
) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.duplicate_scene_input_checked(
        source_scene,
        new_scene,
        scene_name,
        new_input,
        input_name,
    )?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    ProjectStore::new(path)?.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn add_audio_bus(path: &Path, bus: BusId, name: String) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.add_audio_bus_checked(AudioBus {
        id: bus,
        name,
        sends: Vec::new(),
    })?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn rename_audio_bus(path: &Path, bus: BusId, name: String) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.rename_audio_bus(bus, name)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn update_audio_bus_send(
    path: &Path,
    source: BusId,
    destination: BusId,
    add: bool,
) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    if add {
        project.add_audio_bus_send(source, destination)?;
    } else {
        project.remove_audio_bus_send(source, destination)?;
    }
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    ProjectStore::new(path)?.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn add_output(
    path: &Path,
    output: OutputId,
    scene: SceneId,
    bus: BusId,
    name: String,
) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.add_output_checked(fm_model::Output {
        id: output,
        name,
        video_source: scene,
        audio_source: bus,
        startup: StartupPolicy::Stopped,
        required_capabilities: Vec::new(),
    })?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn set_output_route(path: &Path, output: OutputId, scene: SceneId, bus: BusId) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.set_output_route(output, scene, bus)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn set_output_startup(path: &Path, output: OutputId, startup: StartupPolicy) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.set_output_startup(output, startup)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn rename_output(path: &Path, output: OutputId, name: String) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.rename_output(output, name)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

/// Builds a validated destination from the authored arguments.
///
/// The protocol pairs the destination with its key shape: HLS writes to a
/// local directory and gets `None`, while RTMP/RTMPS/SRT require the authored
/// key. Every rejection here carries no URL or key text, so a mistyped key is
/// refused without echoing it back to the terminal or into a shell history
/// through an error message.
fn stream_target(spec: &StreamSpec) -> AppResult<StreamTarget> {
    let (protocol, endpoint) = StreamEndpoint::parse_url(&spec.url)
        .map_err(|error| AppFailure(format!("invalid stream url: {error}")))?;
    let backup_endpoint = spec
        .backup_url
        .as_deref()
        .map(|url| {
            let (backup_protocol, backup_endpoint) = StreamEndpoint::parse_url(url)
                .map_err(|error| AppFailure(format!("invalid backup stream url: {error}")))?;
            if backup_protocol == protocol {
                Ok(backup_endpoint)
            } else {
                Err(AppFailure(format!(
                    "backup stream url must use {protocol}://, matching the primary"
                )))
            }
        })
        .transpose()?;
    let key = match protocol {
        StreamProtocol::Hls | StreamProtocol::Omt => None,
        StreamProtocol::Rtmp | StreamProtocol::Rtmps | StreamProtocol::Srt => {
            let secret = spec.key.as_deref().ok_or_else(|| {
                AppFailure("stream destination protocol requires a stream key".into())
            })?;
            Some(
                StreamKey::parse(secret)
                    .map_err(|error| AppFailure(format!("invalid stream key: {error}")))?,
            )
        }
    };
    let target = StreamTarget::new(
        stream_target_id(spec.stream)?,
        spec.name.clone(),
        protocol,
        endpoint,
        key,
        output_id(spec.output)?,
    )
    .and_then(|target| target.with_backup_endpoint(backup_endpoint))
    .map_err(|error| AppFailure(error.to_string()))?;
    let target = target
        .with_video_bitrate(spec.video_bitrate_kbps)
        .map_err(|error| AppFailure(error.to_string()))?;
    let target = target
        .with_ladder(spec.ladder.clone())
        .map_err(|error| AppFailure(error.to_string()))?;
    let target = match spec.srt_latency_ms {
        Some(latency) => target
            .with_srt_latency_ms(latency)
            .map_err(|error| AppFailure(error.to_string()))?,
        None => target,
    };
    // The passphrase is a secret: the typed error names no text.
    let passphrase = spec
        .srt_passphrase
        .as_deref()
        .map(|secret| {
            SrtPassphrase::parse(secret)
                .map_err(|error| AppFailure(format!("invalid SRT passphrase: {error}")))
        })
        .transpose()?;
    let target = target
        .with_srt_passphrase(passphrase)
        .map_err(|error| AppFailure(error.to_string()))?;
    Ok(target
        .with_startup(spec.startup)
        .with_reconnect(spec.reconnect))
}

fn add_stream_target(path: &Path, spec: &StreamSpec) -> AppResult<()> {
    let target = stream_target(spec)?;
    update_project(path, |project| {
        project.add_stream_target_checked(target)?;
        Ok(())
    })
}

/// Replaces one destination's authored configuration.
///
/// The desired running flag is switcher state, not configuration, so it is
/// carried over from the existing destination: rotating a key or URL must not
/// silently stop a stream the operator started.
fn update_stream_target(path: &Path, spec: &StreamSpec) -> AppResult<()> {
    let target = stream_target(spec)?;
    update_project(path, |project| {
        let running = project
            .stream_targets()
            .iter()
            .find(|existing| existing.id() == target.id())
            .is_some_and(StreamTarget::running);
        project.replace_stream_target(target.set_running(running))?;
        Ok(())
    })
}

fn remove_stream_target(path: &Path, target: StreamTargetId) -> AppResult<()> {
    update_project(path, |project| {
        project.remove_stream_target(target)?;
        Ok(())
    })
}

/// Starts or stops one inventoried stream destination.
///
/// This is an instant switcher mutation: it restores the engine checkpoint,
/// applies [`EngineCommand::StreamStart`] or [`EngineCommand::StreamStop`]
/// through the same path as every other instant command, checkpoints, and only
/// then persists. An unknown target is refused before any engine work so a
/// mistyped id leaves the bundle byte-identical.
fn set_stream_running(
    path: &Path,
    target: fm_switcher::StreamTargetId,
    running: bool,
) -> AppResult<()> {
    let mut project_engine = load_engine(path)?;
    if !project_engine
        .engine
        .show()
        .streams()
        .iter()
        .any(|stream| stream.id() == target)
    {
        return Err(AppFailure(format!("unknown stream destination {target}")).into());
    }
    let command = if running {
        EngineCommand::StreamStart { target }
    } else {
        EngineCommand::StreamStop { target }
    };
    let result = execute(&mut project_engine.engine, command, 1, None, None)?;
    if let Some(rejection) = result.rejection {
        return Err(rejection.into());
    }
    if !result.replayed {
        save_engine(path, &project_engine)?;
    }
    print_status(&project_engine);
    Ok(())
}

/// Starts or stops every inventoried stream destination at once (OR-006).
///
/// Like [`set_stream_running`] this is one instant switcher mutation,
/// [`EngineStreamControl::StartAll`] or [`EngineStreamControl::StopAll`],
/// that toggles only the destinations' desired running flags: the recorder
/// slots are never touched. A show without destinations is refused before any
/// engine work so the bundle stays byte-identical.
fn set_all_streams_running(path: &Path, running: bool) -> AppResult<()> {
    let mut project_engine = load_engine(path)?;
    if project_engine.engine.show().streams().is_empty() {
        return Err(AppFailure("the show has no stream destinations".into()).into());
    }
    let command = EngineCommand::StreamControl(if running {
        EngineStreamControl::StartAll
    } else {
        EngineStreamControl::StopAll
    });
    let result = execute(&mut project_engine.engine, command, 1, None, None)?;
    if let Some(rejection) = result.rejection {
        return Err(rejection.into());
    }
    if !result.replayed {
        save_engine(path, &project_engine)?;
    }
    print_status(&project_engine);
    Ok(())
}

/// Starts or stops program recording for one recorder slot.
///
/// This is an instant switcher mutation: it restores the engine checkpoint,
/// applies [`EngineCommand::RecordStart`] or [`EngineCommand::RecordStop`] for
/// the addressed slot through the same path as every other instant command,
/// checkpoints, and only then persists. The command carries only the
/// addressed slot, so the other slot's desired flag is untouched and an
/// idempotent replay returns the same receipt without touching the bundle.
fn set_recording_desired(path: &Path, recorder: u8, active: bool) -> AppResult<()> {
    let recorder = switcher_recorder(recorder)?;
    let mut project_engine = load_engine(path)?;
    let command = if active {
        EngineCommand::RecordStart { recorder }
    } else {
        EngineCommand::RecordStop { recorder }
    };
    let result = execute(&mut project_engine.engine, command, 1, None, None)?;
    if let Some(rejection) = result.rejection {
        return Err(rejection.into());
    }
    if !result.replayed {
        save_engine(path, &project_engine)?;
    }
    print_status(&project_engine);
    Ok(())
}

fn set_scene_input_audio_source(
    path: &Path,
    scene_input: InputId,
    audio_source: Option<InputId>,
) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.set_scene_input_audio_source(scene_input, audio_source)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn remove_scene_input(path: &Path, input: InputId) -> AppResult<()> {
    update_project(path, |project| {
        let selected = project
            .inputs()
            .iter()
            .find(|candidate| candidate.id == input)
            .ok_or_else(|| AppFailure(format!("unknown input {input}")))?;
        let InputKind::Scene { scene_id, .. } = &selected.kind else {
            return Err(AppFailure(format!("input {input} is not a scene input")).into());
        };
        let scene = *scene_id;
        project.remove_input(input)?;
        project.remove_scene(scene)?;
        Ok(())
    })
}

fn add_scene_layer(
    path: &Path,
    scene: SceneId,
    source: SourceRef,
    z_order: i32,
    name: String,
) -> AppResult<()> {
    update_project(path, |project| {
        let dimensions = project.settings().video.dimensions;
        project
            .add_layer_to_scene(
                scene,
                Layer {
                    // Replaced by a free id when the scene already uses it.
                    id: fm_model::LayerId::new(1).expect("layer id one is nonzero"),
                    name,
                    source,
                    enabled: true,
                    geometry: LayerGeometry::new(
                        0,
                        0,
                        dimensions.width(),
                        dimensions.height(),
                        Rotation::Deg0,
                    ),
                    crop: None,
                    mask: None,
                    key: None,
                    effects: Vec::new(),
                    opacity: u8::MAX,
                    z_order,
                },
            )
            .map_err(Into::into)
    })
}

fn set_scene_background(path: &Path, scene: SceneId, background: ModelRgba8) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.set_scene_background(scene, background)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn remove_scene(path: &Path, scene: SceneId) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.remove_scene(scene)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn rename_scene(path: &Path, scene: SceneId, name: String) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.rename_scene(scene, name)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    ProjectStore::new(path)?.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn remove_scene_layer(path: &Path, scene: SceneId, index: usize) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.remove_layer_from_scene(scene, index)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn move_scene_layer(
    path: &Path,
    scene: SceneId,
    source_index: usize,
    destination_index: usize,
) -> AppResult<()> {
    update_project(path, |project| {
        project
            .move_scene_layer(scene, source_index, destination_index)
            .map_err(Into::into)
    })
}

fn duplicate_scene_layer(path: &Path, scene: SceneId, index: usize, name: String) -> AppResult<()> {
    update_project(path, |project| {
        project
            .duplicate_scene_layer(scene, index, name)
            .map_err(Into::into)
    })
}

fn copy_scene_layer(
    path: &Path,
    source_scene: SceneId,
    source_index: usize,
    target_scene: SceneId,
    name: String,
) -> AppResult<()> {
    update_project(path, |project| {
        project
            .copy_scene_layer(source_scene, source_index, target_scene, name)
            .map_err(Into::into)
    })
}

fn rename_scene_layer(path: &Path, scene: SceneId, index: usize, name: String) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.rename_scene_layer(scene, index, name)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn set_scene_layer_source(
    path: &Path,
    scene: SceneId,
    index: usize,
    source: SourceRef,
) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.set_scene_layer_source(scene, index, source)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn set_scene_layer_z_order(
    path: &Path,
    scene: SceneId,
    index: usize,
    z_order: i32,
) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.set_scene_layer_z_order(scene, index, z_order)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn set_scene_layer_appearance(
    path: &Path,
    scene: SceneId,
    index: usize,
    enabled: bool,
    opacity: u8,
) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.set_scene_layer_appearance(scene, index, enabled, opacity)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn set_scene_layer_geometry(
    path: &Path,
    scene: SceneId,
    index: usize,
    geometry: LayerGeometry,
) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.set_scene_layer_geometry(scene, index, geometry)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn set_scene_layer_crop(
    path: &Path,
    scene: SceneId,
    index: usize,
    crop: Option<CropRect>,
) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.set_scene_layer_crop(scene, index, crop)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn set_scene_layer_mask(
    path: &Path,
    scene: SceneId,
    index: usize,
    mask: Option<RectMask>,
) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.set_scene_layer_mask(scene, index, mask)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn set_scene_layer_effects(
    path: &Path,
    scene: SceneId,
    index: usize,
    effects: Vec<LayerEffect>,
) -> AppResult<()> {
    update_project(path, |project| {
        project
            .set_scene_layer_effects(scene, index, effects)
            .map_err(Into::into)
    })
}

fn add_scene_layer_effect(
    path: &Path,
    scene: SceneId,
    index: usize,
    effect: LayerEffect,
) -> AppResult<()> {
    update_project(path, |project| {
        project
            .add_scene_layer_effect(scene, index, effect)
            .map_err(Into::into)
    })
}

fn set_scene_layer_effect(
    path: &Path,
    scene: SceneId,
    index: usize,
    effect_index: usize,
    effect: LayerEffect,
) -> AppResult<()> {
    update_project(path, |project| {
        let mut effects = scene_layer_effects(project, scene, index)?;
        let length = effects.len();
        let target =
            effects
                .get_mut(effect_index)
                .ok_or(SceneLayerError::EffectIndexOutOfRange {
                    scene,
                    index,
                    effect_index,
                    length,
                })?;
        *target = effect;
        project
            .set_scene_layer_effects(scene, index, effects)
            .map_err(Into::into)
    })
}

fn remove_scene_layer_effect(
    path: &Path,
    scene: SceneId,
    index: usize,
    effect_index: usize,
) -> AppResult<()> {
    update_project(path, |project| {
        project
            .remove_scene_layer_effect(scene, index, effect_index)
            .map_err(Into::into)
    })
}

fn scene_layer_effects(
    project: &Project,
    scene: SceneId,
    index: usize,
) -> AppResult<Vec<LayerEffect>> {
    let scene_model = project
        .scenes()
        .iter()
        .find(|candidate| candidate.id == scene)
        .ok_or(SceneLayerError::UnknownScene(scene))?;
    let layer = scene_model
        .layers
        .get(index)
        .ok_or(SceneLayerError::LayerIndexOutOfRange {
            scene,
            index,
            length: scene_model.layers.len(),
        })?;
    Ok(layer.effects.clone())
}

fn remove_input(path: &Path, input: InputId) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    let runtime = stored.runtime_routing();
    let transitions = stored.runtime_manual_transitions();
    let overlays = stored.runtime_overlays();
    let referenced = [
        runtime.desired_program_id,
        runtime.realized_program_id,
        runtime.desired_preview_id,
        runtime.realized_preview_id,
    ]
    .into_iter()
    .flatten()
    .any(|candidate| candidate == input)
        || [transitions.desired, transitions.realized]
            .into_iter()
            .flatten()
            .any(|state| state.from_id == input || state.to_id == input)
        || overlays
            .desired
            .iter()
            .chain(overlays.realized.iter())
            .any(|channel| {
                channel.source == Some(input) || channel.queued_sources.contains(&input)
            });
    if referenced {
        return Err(AppFailure(format!("cannot remove input {input}: runtime reference")).into());
    }
    let store = ProjectStore::new(path)?;
    let mut project = stored.project().clone();
    project.remove_input(input)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        transitions,
        stored.runtime_fade_to_black(),
        overlays.clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn overlay_references_output(overlays: &RuntimeOverlays, output: OutputId) -> bool {
    overlays
        .desired
        .iter()
        .chain(overlays.realized.iter())
        .any(|channel| channel.included_outputs.contains(&output))
}

fn remove_output(path: &Path, output: OutputId) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    if overlay_references_output(stored.runtime_overlays(), output) {
        return Err(AppFailure(format!(
            "cannot remove output {output}: runtime overlay reference"
        ))
        .into());
    }
    let store = ProjectStore::new(path)?;
    let mut project = stored.project().clone();
    project.remove_output(output)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn remove_audio_bus(path: &Path, bus: BusId) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    let store = ProjectStore::new(path)?;
    let mut project = stored.project().clone();
    project.remove_audio_bus(bus)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn duplicate_input(path: &Path, source: InputId, input: InputId, name: String) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    let store = ProjectStore::new(path)?;
    let mut project = stored.project().clone();
    // The copy shares the source's kind, capabilities, strip, category and
    // colour label (IN-026).
    project
        .duplicate_input(source, input, name)
        .map_err(|error| AppFailure(format!("input {source} cannot be duplicated: {error}")))?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn replace_input_simulated(path: &Path, input: InputId) -> AppResult<()> {
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    project.replace_input_source(
        input,
        InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Bars,
            SimulatedAudio::Silence,
        )),
        Vec::new(),
    )?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    ProjectStore::new(path)?.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn replace_input_solid(path: &Path, input: InputId, color: SolidColor) -> AppResult<()> {
    update_project(path, |project| {
        project
            .replace_input_source(
                input,
                InputKind::Simulated(SimulatedInput::new(
                    SimulatedVideo::Solid(color),
                    SimulatedAudio::Silence,
                )),
                Vec::new(),
            )
            .map_err(Into::into)
    })
}

fn replace_input_media(path: &Path, input: InputId, asset_uri: String) -> AppResult<()> {
    update_project(path, |project| {
        project
            .replace_input_source(
                input,
                InputKind::Media {
                    asset_uri,
                    playback: fm_model::MediaPlaybackSettings::default(),
                },
                Vec::new(),
            )
            .map_err(Into::into)
    })
}

fn replace_input_scene(path: &Path, input: InputId, scene: SceneId) -> AppResult<()> {
    update_project(path, |project| {
        project
            .replace_input_source(
                input,
                InputKind::Scene {
                    scene_id: scene,
                    audio_source: None,
                },
                Vec::new(),
            )
            .map_err(Into::into)
    })
}

fn remove_stinger(path: &Path, slot: StingerSlotNumber) -> AppResult<()> {
    update_project(path, |project| {
        let _ = project.remove_stinger(slot);
        Ok(())
    })
}

pub(crate) fn update_project(
    path: &Path,
    update: impl FnOnce(&mut Project) -> AppResult<()>,
) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let stored = load_stored_project(path)?;
    let mut project = stored.project().clone();
    update(&mut project)?;
    let configured = StoredProject::from_project_with_complete_runtime_state(
        project,
        stored.runtime_routing(),
        stored.runtime_manual_transitions(),
        stored.runtime_fade_to_black(),
        stored.runtime_overlays().clone(),
        stored.position(),
        stored.idempotency_receipts().to_vec(),
    )?;
    store.save(&configured)?;
    print_status(&load_engine(path)?);
    Ok(())
}

fn load_engine(path: &Path) -> AppResult<ProjectEngine> {
    restore_project_engine(path, &load_stored_project(path)?)
}

/// Restores an engine for reporting only, without opening the journal.
pub(crate) fn inspect_engine(path: &Path) -> AppResult<ProjectEngine> {
    restore_project_engine(path, &inspect_stored_project(path)?)
}

/// The project-owned state the engine edits: the favourite transition
/// buttons, the padlocks, and the media inputs' playback settings. The persisted padlock is engine state, so local
/// engine commands honor it exactly as the daemon does.
fn restore_project_edit_state(show: &mut ShowState, project: &Project) -> AppResult<()> {
    show.restore_transition_favorites(*project.transition_favorites());
    show.set_project_locked(project.project_locked());
    for &input in project.locked_inputs() {
        show.set_input_locked(input, true)?;
    }
    for input in project.inputs() {
        if let InputKind::Media { playback, .. } = &input.kind {
            show.set_media_input(input.id, *playback)?;
        }
        if let Some(source) = fm_model::InputSourceSpec::from_kind(&input.kind) {
            show.set_input_source(input.id, source)?;
        }
        if let InputKind::Device { capture, .. } = &input.kind {
            show.set_capture_settings(input.id, capture.clone())?;
        }
        // A document opens on the page it was left on (IN-014).
        if let InputKind::Document(document) = &input.kind {
            show.set_paged_page(input.id, document.page)?;
        }
    }
    show.restore_input_lifecycle(
        project.input_categories().to_vec(),
        project.input_lifecycles(),
    )?;
    // What is recorded for replay (OR-012).
    show.set_replay(project.replay().clone())?;
    // The project's highlight reels (OR-013).
    show.restore_reels(project.reels().to_vec());
    // Scripts first: a cue may run one (schema 55).
    show.set_scripts(project.scripts().to_vec())?;
    show.set_cue_lists(project.cue_lists().to_vec())?;
    // A run paused at a wait, or a failed part's report (schema 56).
    show.set_script_run(project.script_run().clone(), project.settings().frame_rate)?;
    show.set_shortcuts(project.shortcuts().to_vec())?;
    // Each list input's active item is its media source (IN-006).
    show.restore_media_lists(project.media_lists().clone())?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn restore_project_engine(path: &Path, stored: &StoredProject) -> AppResult<ProjectEngine> {
    let project = stored.project().clone();
    let inputs = project
        .inputs()
        .iter()
        .map(|input| (input.id, input.name.clone()))
        .collect::<Vec<_>>();
    let input_ids = inputs.iter().map(|(input, _)| *input).collect::<Vec<_>>();
    let main_mix = required_main_mix(&project)?;
    let routing = stored.runtime_routing();
    let realized_program = required_routing(routing.realized_program_id, "realized program")?;
    let realized_preview = required_routing(routing.realized_preview_id, "realized preview")?;
    let mut show = ShowState::new(
        project.name(),
        inputs,
        main_mix.desired_program,
        main_mix.desired_preview,
    )?
    .with_outputs(
        project
            .outputs()
            .iter()
            .map(|output| (output.id, output.name.clone()))
            .collect(),
    )?
    .with_stream_targets(project.stream_targets())?;
    for target in project
        .stream_targets()
        .iter()
        .filter(|target| target.running())
    {
        show.set_stream_running(switcher_stream_target(target.id()), true)?;
    }
    restore_input_audio_strips(&mut show, &project)?;
    restore_display_outputs(&mut show, &project)?;
    let mut realized = SwitcherState::new(input_ids, realized_program, realized_preview)?
        .with_streams(project.stream_targets().iter().map(|target| {
            (
                switcher_stream_target(target.id()),
                target.name().to_owned(),
            )
        }))?;
    for target in project
        .stream_targets()
        .iter()
        .filter(|target| target.running())
    {
        realized.set_stream_running(switcher_stream_target(target.id()), true)?;
    }
    for slot in ModelRecorderSlot::ALL {
        if project.recording_desired(slot) {
            let recorder = switcher_recorder_slot(slot);
            let _ = show
                .desired_switcher_mut()
                .set_recording_desired(recorder, true);
            let _ = realized.set_recording_desired(recorder, true);
        }
    }
    for config in project.stingers() {
        restore_stinger(&mut show, &mut realized, *config)?;
    }
    let manual = stored.runtime_manual_transitions();
    if let Some(state) = manual.desired {
        show.restore_manual_transition(restored_t_bar(state)?)?;
    }
    if let Some(state) = manual.realized {
        realized.restore_t_bar(restored_t_bar(state)?)?;
    }
    for &output in project.fade_to_black_excluded_outputs() {
        let _ = realized.set_fade_to_black_output(output, false);
    }
    for (&output, &duration) in project.fade_to_black_output_durations() {
        realized
            .set_fade_to_black_output_duration(output, Some(duration))
            .map_err(|error| AppFailure(format!("invalid Fade-to-Black duration: {error}")))?;
    }
    let _ = realized.set_fade_to_black_curve(project.fade_to_black_curve());
    let fade_to_black = stored.runtime_fade_to_black();
    show.restore_fade_to_black(fade_to_black.desired.target_active);
    realized.restore_settled_fade_to_black(fade_to_black.realized.target_active);
    restore_overlays(&mut show, &mut realized, stored.runtime_overlays())?;
    restore_titles(path, &mut show, &project)?;
    restore_project_edit_state(&mut show, &project)?;
    let position = stored.position();
    let mut engine = Engine::restore_persisted(
        show,
        realized,
        project.settings().frame_rate,
        clock_domain(),
        EngineRestoreState {
            state_epoch: StateEpoch::new(position.state_epoch),
            revision: Revision::new(position.revision),
            event_sequence: EventSequence::new(position.event_sequence),
            runtime_generation: RuntimeGeneration::new(position.runtime_generation),
            clock_time: ClockTime::from_nanos(position.clock_time_nanos),
            frame_cursor: position.frames_rendered.into(),
            receipts: stored
                .idempotency_receipts()
                .iter()
                .map(runtime_receipt)
                .collect::<AppResult<Vec<_>>>()?,
        },
    )?;
    restore_document_page_counts(path, &project, &mut engine);
    // Every shown title continues its timeline (GX-001, schema 68).
    engine.restore_title_timelines(project.title_timelines())?;
    Ok(ProjectEngine { project, engine })
}

/// Reports every document input's page count to the engine (IN-014), read
/// from the bundle asset exactly as the daemon reads it: a PDF is parsed, a
/// presentation's package is read (no converter is needed for a count). A
/// document that cannot be read keeps no count, so a page step on it is
/// refused as unavailable.
fn restore_document_page_counts(path: &Path, project: &Project, engine: &mut Engine) {
    let Ok(store) = ProjectStore::new(path) else {
        return;
    };
    for (input, document) in project.documents() {
        let count = document_page_count(&store, &document.asset_uri);
        engine.set_paged_count(input, count);
    }
}

fn document_page_count(store: &ProjectStore, asset_uri: &str) -> Option<u32> {
    let format = fm_types::paged::DocumentFormat::from_file_name(asset_uri)?;
    let file = store.resolve_asset_uri(asset_uri).ok()?;
    if std::fs::metadata(&file).ok()?.len() > fm_presentations::MAX_DOCUMENT_BYTES {
        return None;
    }
    let bytes = std::fs::read(file).ok()?;
    fm_presentations::probe(format, &bytes)
        .ok()
        .map(|info| info.page_count)
}

/// The persisted form of the engine's plugin chain (AU-006, schema 91).
fn model_plugin_chain(
    chain: &fm_plugin_host_api::PluginChain,
) -> AppResult<fm_types::audio_plugins::PluginChainState> {
    let mut state = fm_types::audio_plugins::PluginChainState::new();
    for slot in chain.slots() {
        let mut model = fm_types::audio_plugins::PluginSlotState::new(slot.plugin().as_str());
        model.set_bypassed(slot.is_bypassed());
        model
            .set_latency(slot.latency_frames())
            .map_err(|error| AppFailure(format!("plugin chain: {error}")))?;
        if let Some(snapshot) = slot.snapshot() {
            model
                .set_state(snapshot.data().to_vec())
                .map_err(|error| AppFailure(format!("plugin chain: {error}")))?;
        }
        for (name, value) in slot.parameters() {
            model
                .set_parameter(name, value)
                .map_err(|error| AppFailure(format!("plugin chain: {error}")))?;
        }
        state
            .push(model)
            .map_err(|error| AppFailure(format!("plugin chain: {error}")))?;
    }
    Ok(state)
}

/// The engine plugin chain for a persisted one (AU-006, schema 91).
fn engine_plugin_chain(
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
            .map_err(|error| {
                AppFailure(format!("plugin slot {} is invalid: {error}", slot.plugin()))
            })?;
        if !slot.state().is_empty() {
            let snapshot = fm_plugin_host_api::StateSnapshot::new(
                plugin,
                fm_plugin_host_api::StateVersion::new(0),
                slot.state().to_vec(),
                &limits,
            )
            .map_err(|error| {
                AppFailure(format!("plugin slot {} is invalid: {error}", slot.plugin()))
            })?;
            engine_slot.store_snapshot(snapshot).map_err(|error| {
                AppFailure(format!("plugin slot {} is invalid: {error}", slot.plugin()))
            })?;
        }
        for (name, value) in slot.parameters() {
            engine_slot.set_parameter(name, value).map_err(|error| {
                AppFailure(format!("plugin slot {} is invalid: {error}", slot.plugin()))
            })?;
        }
        chain.push(engine_slot).map_err(|error| {
            AppFailure(format!("plugin slot {} is invalid: {error}", slot.plugin()))
        })?;
    }
    Ok(chain)
}

/// The engine plugin chain for typed CLI slots (AU-006).
fn local_plugin_chain(
    slots: &[fm_protocol::PluginChainSlot],
) -> AppResult<fm_plugin_host_api::PluginChain> {
    let mut state = fm_types::audio_plugins::PluginChainState::new();
    for slot in slots {
        let mut model = fm_types::audio_plugins::PluginSlotState::new(slot.plugin.as_str());
        model.set_bypassed(slot.bypassed);
        model.set_latency(slot.latency_frames).map_err(|error| {
            AppFailure(format!("plugin slot {} is invalid: {error}", slot.plugin))
        })?;
        model.set_state(slot.state.clone()).map_err(|error| {
            AppFailure(format!("plugin slot {} is invalid: {error}", slot.plugin))
        })?;
        for parameter in &slot.parameters {
            model
                .set_parameter(&parameter.name, parameter.value())
                .map_err(|error| {
                    AppFailure(format!("plugin slot {} is invalid: {error}", slot.plugin))
                })?;
        }
        state.push(model).map_err(|error| {
            AppFailure(format!("plugin slot {} is invalid: {error}", slot.plugin))
        })?;
    }
    engine_plugin_chain(&state)
}

fn engine_audio_strip_state(
    gain_millidb: i32,
    balance_basis_points: i32,
    pan_basis_points: i32,
    muted: bool,
    soloed: bool,
    follow_video: bool,
    delay_samples: u32,
) -> AppResult<EngineInputAudioStripState> {
    Ok(EngineInputAudioStripState {
        pan_basis_points: fm_model::InputPanBasisPoints::new(pan_basis_points)
            .ok_or_else(|| AppFailure("pan must be in -10000..=10000 basis points".into()))?
            .get(),
        gain_millidb: InputGainMilliDb::new(gain_millidb)
            .ok_or_else(|| AppFailure("gain must be in -96000..=24000 millidB".into()))?
            .get(),
        balance_basis_points: InputBalanceBasisPoints::new(balance_basis_points)
            .ok_or_else(|| AppFailure("balance must be in -10000..=10000 basis points".into()))?
            .get(),
        muted,
        soloed,
        follow_video,
        delay_samples: InputDelaySamples::new(delay_samples)
            .ok_or_else(|| AppFailure("delay samples must be in 0..=48000".into()))?
            .get(),
        sends: fm_types::audio_bus::AudioSends::MASTER,
    })
}

/// The engine edit for a parsed mixer command.
#[allow(clippy::too_many_lines)] // one arm per mixer command
fn engine_audio_mixer_edit(edit: AudioMixerArgs) -> AppResult<fm_engine::EngineAudioMixerEdit> {
    let group = |number: u8| {
        fm_model::AudioGroupNumber::new(number)
            .ok_or_else(|| AppFailure("group must be 1..=4".into()))
    };
    Ok(match edit {
        AudioMixerArgs::Label { input, label } => fm_engine::EngineAudioMixerEdit::SetLabel {
            input: input_id(input)?,
            label,
        },
        AudioMixerArgs::Group {
            input,
            group: number,
        } => fm_engine::EngineAudioMixerEdit::SetGroup {
            input: input_id(input)?,
            group: number.map(group).transpose()?,
        },
        AudioMixerArgs::GroupName {
            group: number,
            name,
        } => fm_engine::EngineAudioMixerEdit::RenameGroup {
            group: group(number)?,
            name,
        },
        AudioMixerArgs::GroupLevel {
            group: number,
            gain_millidb,
            muted,
        } => fm_engine::EngineAudioMixerEdit::SetGroupLevel {
            group: group(number)?,
            state: fm_engine::EngineAudioGroupState {
                gain_millidb: InputGainMilliDb::new(gain_millidb)
                    .ok_or_else(|| AppFailure("gain must be in -96000..=24000 millidB".into()))?
                    .get(),
                muted,
            },
        },
        AudioMixerArgs::GroupSolo {
            group: number,
            soloed,
        } => fm_engine::EngineAudioMixerEdit::SetGroupSolo {
            group: group(number)?,
            soloed,
        },
        AudioMixerArgs::GroupParent {
            group: number,
            parent,
        } => fm_engine::EngineAudioMixerEdit::SetGroupParent {
            group: group(number)?,
            parent: parent.map(group).transpose()?,
        },
        AudioMixerArgs::SoloMode { mode } => fm_engine::EngineAudioMixerEdit::SetSoloMode { mode },
        AudioMixerArgs::MonitorDevice { device } => {
            if let Some(device) = &device {
                fm_model::validate_audio_monitor_device(device)
                    .map_err(|error| AppFailure(error.to_string()))?;
            }
            fm_engine::EngineAudioMixerEdit::SetMonitorDevice { device }
        }
        AudioMixerArgs::MonitorLevel {
            gain_millidb,
            muted,
        } => fm_engine::EngineAudioMixerEdit::SetMonitorLevel {
            state: fm_engine::EngineAudioGroupState {
                gain_millidb: InputGainMilliDb::new(gain_millidb)
                    .ok_or_else(|| AppFailure("gain must be in -96000..=24000 millidB".into()))?
                    .get(),
                muted,
            },
        },
        AudioMixerArgs::AuxDsp { bus, chain } => fm_engine::EngineAudioMixerEdit::SetAuxBusDsp {
            bus,
            chain: Box::new(chain),
        },
        AudioMixerArgs::StripDsp { input, chain } => fm_engine::EngineAudioMixerEdit::SetStripDsp {
            target: match input {
                Some(input) => fm_types::strip_dsp::StripDspTarget::Input(input_id(input)?),
                None => fm_types::strip_dsp::StripDspTarget::Master,
            },
            chain: Box::new(chain),
            gesture: None,
        },
        AudioMixerArgs::LoudnessReset { input } => fm_engine::EngineAudioMixerEdit::ResetLoudness {
            target: match input {
                Some(input) => fm_types::strip_dsp::StripDspTarget::Input(input_id(input)?),
                None => fm_types::strip_dsp::StripDspTarget::Master,
            },
        },
        AudioMixerArgs::AuxName { bus, name } => {
            fm_model::validate_audio_group_name(&name)
                .map_err(|error| AppFailure(error.to_string()))?;
            fm_engine::EngineAudioMixerEdit::RenameAuxBus { bus, name }
        }
        AudioMixerArgs::AuxLevel {
            bus,
            gain_millidb,
            muted,
        } => fm_engine::EngineAudioMixerEdit::SetAuxBusLevel {
            bus,
            state: fm_engine::EngineAudioGroupState {
                gain_millidb: InputGainMilliDb::new(gain_millidb)
                    .ok_or_else(|| AppFailure("gain must be in -96000..=24000 millidB".into()))?
                    .get(),
                muted,
            },
        },
        AudioMixerArgs::OutputAux { output, bus } => {
            fm_engine::EngineAudioMixerEdit::SetOutputAuxBus {
                output: fm_types::OutputId::new(
                    NonZeroU128::new(output)
                        .ok_or_else(|| AppFailure("output ID must be nonzero".into()))?,
                ),
                bus,
            }
        }
        AudioMixerArgs::OutputMixMinus { output, input } => {
            fm_engine::EngineAudioMixerEdit::SetOutputMixMinus {
                output: fm_types::OutputId::new(
                    NonZeroU128::new(output)
                        .ok_or_else(|| AppFailure("output ID must be nonzero".into()))?,
                ),
                input: input
                    .map(|input| {
                        NonZeroU128::new(input)
                            .map(fm_types::InputId::new)
                            .ok_or_else(|| AppFailure("input ID must be nonzero".into()))
                    })
                    .transpose()?,
            }
        }
        AudioMixerArgs::AuxDevice { bus, device } => {
            fm_engine::EngineAudioMixerEdit::SetAuxBusDevice { bus, device }
        }
        AudioMixerArgs::AuxOmt { bus, omt } => {
            fm_engine::EngineAudioMixerEdit::SetAuxBusOmt { bus, omt }
        }
    })
}

/// The wire operation for a parsed mixer command.
#[allow(clippy::too_many_lines)] // one arm per mixer command
fn wire_audio_mixer_operation(edit: AudioMixerArgs) -> AppResult<fm_protocol::AudioMixerOperation> {
    Ok(match engine_audio_mixer_edit(edit)? {
        fm_engine::EngineAudioMixerEdit::SetLabel { input, label } => {
            fm_protocol::AudioMixerOperation::SetLabel {
                input: fm_protocol::WireInputId::from_domain(input),
                label,
            }
        }
        fm_engine::EngineAudioMixerEdit::SetGroup { input: id, group } => {
            fm_protocol::AudioMixerOperation::SetGroup {
                input: fm_protocol::WireInputId::from_domain(id),
                group: group.map(fm_model::AudioGroupNumber::get),
            }
        }
        fm_engine::EngineAudioMixerEdit::RenameGroup { group, name } => {
            fm_protocol::AudioMixerOperation::RenameGroup {
                group: group.get(),
                name,
            }
        }
        fm_engine::EngineAudioMixerEdit::SetGroupLevel { group, state } => {
            fm_protocol::AudioMixerOperation::SetGroupLevel {
                group: group.get(),
                gain_millidb: state.gain_millidb,
                muted: state.muted,
            }
        }
        fm_engine::EngineAudioMixerEdit::SetGroupSolo { group, soloed } => {
            fm_protocol::AudioMixerOperation::SetGroupSolo {
                group: group.get(),
                soloed,
            }
        }
        fm_engine::EngineAudioMixerEdit::SetGroupParent { group, parent } => {
            fm_protocol::AudioMixerOperation::SetGroupParent {
                group: group.get(),
                parent: parent.map(fm_model::AudioGroupNumber::get),
            }
        }
        fm_engine::EngineAudioMixerEdit::SetSoloMode { mode } => {
            fm_protocol::AudioMixerOperation::SetSoloMode {
                mode: match mode {
                    fm_model::AudioSoloMode::InPlace => fm_protocol::AudioSoloMode::InPlace,
                    fm_model::AudioSoloMode::Pfl => fm_protocol::AudioSoloMode::Pfl,
                    fm_model::AudioSoloMode::Afl => fm_protocol::AudioSoloMode::Afl,
                },
            }
        }
        fm_engine::EngineAudioMixerEdit::SetMonitorDevice { device } => {
            fm_protocol::AudioMixerOperation::SetMonitorDevice { device }
        }
        fm_engine::EngineAudioMixerEdit::SetMonitorLevel { state } => {
            fm_protocol::AudioMixerOperation::SetMonitorLevel {
                gain_millidb: state.gain_millidb,
                muted: state.muted,
            }
        }
        fm_engine::EngineAudioMixerEdit::SetStripDsp { target, chain, .. } => {
            fm_protocol::AudioMixerOperation::SetStripDsp {
                target: match target {
                    fm_types::strip_dsp::StripDspTarget::Master => {
                        fm_protocol::AudioDspTarget::Master
                    }
                    fm_types::strip_dsp::StripDspTarget::Aux(bus) => {
                        fm_protocol::AudioDspTarget::Aux(bus)
                    }
                    fm_types::strip_dsp::StripDspTarget::Input(input) => {
                        fm_protocol::AudioDspTarget::Input(fm_protocol::WireInputId::from_domain(
                            input,
                        ))
                    }
                },
                chain,
                gesture: None,
            }
        }
        fm_engine::EngineAudioMixerEdit::RenameAuxBus { bus, name } => {
            fm_protocol::AudioMixerOperation::RenameAuxBus { bus, name }
        }
        fm_engine::EngineAudioMixerEdit::SetAuxBusLevel { bus, state } => {
            fm_protocol::AudioMixerOperation::SetAuxBusLevel {
                bus,
                gain_millidb: state.gain_millidb,
                muted: state.muted,
            }
        }
        fm_engine::EngineAudioMixerEdit::SetAuxBusDsp { bus, chain } => {
            fm_protocol::AudioMixerOperation::SetAuxBusDsp { bus, chain }
        }
        fm_engine::EngineAudioMixerEdit::SetAuxBusDevice { bus, device } => {
            fm_protocol::AudioMixerOperation::SetAuxBusDevice { bus, device }
        }
        fm_engine::EngineAudioMixerEdit::SetAuxBusOmt { bus, omt } => {
            fm_protocol::AudioMixerOperation::SetAuxBusOmt { bus, omt }
        }
        fm_engine::EngineAudioMixerEdit::SetOutputAuxBus { output, bus } => {
            fm_protocol::AudioMixerOperation::SetOutputBus {
                output: fm_protocol::WireOutputId::from_domain(output),
                bus,
            }
        }
        fm_engine::EngineAudioMixerEdit::SetOutputMixMinus { output, input } => {
            fm_protocol::AudioMixerOperation::SetOutputMixMinus {
                output: fm_protocol::WireOutputId::from_domain(output),
                input: input.map(fm_protocol::WireInputId::from_domain),
            }
        }
        fm_engine::EngineAudioMixerEdit::ResetLoudness { target } => {
            fm_protocol::AudioMixerOperation::ResetLoudness {
                target: match target {
                    fm_types::strip_dsp::StripDspTarget::Master => {
                        fm_protocol::AudioDspTarget::Master
                    }
                    fm_types::strip_dsp::StripDspTarget::Aux(bus) => {
                        fm_protocol::AudioDspTarget::Aux(bus)
                    }
                    fm_types::strip_dsp::StripDspTarget::Input(input) => {
                        fm_protocol::AudioDspTarget::Input(fm_protocol::WireInputId::from_domain(
                            input,
                        ))
                    }
                },
            }
        }
        fm_engine::EngineAudioMixerEdit::SetPluginChain { target, chain } => {
            fm_protocol::AudioMixerOperation::SetPluginChain {
                target: match target {
                    fm_types::strip_dsp::StripDspTarget::Master => {
                        fm_protocol::AudioDspTarget::Master
                    }
                    fm_types::strip_dsp::StripDspTarget::Aux(bus) => {
                        fm_protocol::AudioDspTarget::Aux(bus)
                    }
                    fm_types::strip_dsp::StripDspTarget::Input(input) => {
                        fm_protocol::AudioDspTarget::Input(fm_protocol::WireInputId::from_domain(
                            input,
                        ))
                    }
                },
                slots: chain
                    .slots()
                    .iter()
                    .map(|slot| fm_protocol::PluginChainSlot {
                        plugin: slot.plugin().as_str().to_owned(),
                        bypassed: slot.is_bypassed(),
                        latency_frames: slot.latency_frames(),
                        state: slot
                            .snapshot()
                            .map_or_else(Vec::new, |snapshot| snapshot.data().to_vec()),
                        parameters: slot
                            .parameters()
                            .map(|(name, value)| fm_protocol::PluginParameter::new(name, value))
                            .collect(),
                    })
                    .collect(),
            }
        }
    })
}

fn restore_input_audio_strips(show: &mut ShowState, project: &Project) -> AppResult<()> {
    for strip in project.input_audio_strips() {
        show.set_input_audio_strip(
            strip.input,
            EngineInputAudioStripState {
                pan_basis_points: strip.state.pan.get(),
                gain_millidb: strip.state.gain.get(),
                balance_basis_points: strip.state.balance.get(),
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
    for (&output, &input) in project.output_mix_minus() {
        show.set_output_mix_minus(output, Some(input));
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
fn restore_display_outputs(show: &mut ShowState, project: &Project) -> AppResult<()> {
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
/// slots (OR-011) into the project. Every
/// slot is cleared first, so a display moving between slots is never
/// claimed twice.
fn sync_display_outputs(project: &mut Project, show: &ShowState) -> AppResult<()> {
    if project.alert_rules() != show.alert_rules() {
        for rule in fm_types::alert_rules::HealthAlertRule::ALL {
            project
                .set_alert_rule(fm_types::alert_rules::effective_setting(
                    show.alert_rules(),
                    rule,
                ))
                .map_err(|error| format!("project cannot take alert rules: {error}"))?;
        }
    }
    if project.live_lan() != show.live_lan() {
        project
            .set_live_lan(show.live_lan().clone())
            .map_err(|error| format!("project cannot take LiveLAN: {error}"))?;
    }
    if project.replay() != show.replay() {
        project
            .set_replay(show.replay().clone())
            .map_err(|error| format!("project cannot take replay: {error}"))?;
    }
    if project.display_outputs() == show.display_outputs() {
        return Ok(());
    }
    for slot in fm_types::display::DisplayOutputSlot::ALL {
        project.clear_display_output(slot);
    }
    for (&slot, config) in show.display_outputs() {
        project
            .set_display_output(slot, config.clone())
            .map_err(|error| format!("project cannot take display output {slot}: {error}"))?;
    }
    Ok(())
}

/// Folds the show's strip labels, groups, and Fade-to-Black output
/// selection (AU-001, SW-004) into the project.
fn sync_audio_mixer(project: &mut Project, show: &ShowState) -> AppResult<()> {
    for &input in show.inputs() {
        let label = show.input_audio_label(input);
        if project.input_audio_label(input) != Some(label) {
            project
                .set_input_audio_label(input, label.to_owned())
                .map_err(|error| AppFailure(format!("input {input} strip label: {error}")))?;
        }
        if !project.set_input_audio_group(input, show.input_audio_group(input)) {
            return Err(AppFailure(format!("project is missing input {input}")).into());
        }
        project
            .set_input_audio_dsp(input, show.input_audio_dsp(input))
            .map_err(|error| AppFailure(format!("input {input} strip DSP: {error}")))?;
        project
            .set_input_audio_plugins(input, model_plugin_chain(show.input_audio_plugins(input))?)
            .map_err(|error| AppFailure(format!("input {input} plugin chain: {error}")))?;
    }
    project
        .set_master_audio_dsp(*show.master_audio_dsp())
        .map_err(|error| AppFailure(format!("Master DSP: {error}")))?;
    project
        .set_master_audio_plugins(model_plugin_chain(show.master_audio_plugins())?)
        .map_err(|error| AppFailure(format!("Master plugin chain: {error}")))?;
    // Parents are cleared first so an old parent never closes a transient
    // cycle with a new one.
    for nested in [false, true] {
        for (group, value) in fm_model::AudioGroupNumber::ALL
            .into_iter()
            .zip(show.audio_groups())
        {
            project
                .set_audio_group(
                    group,
                    fm_model::AudioGroup {
                        name: value.name.clone(),
                        gain: InputGainMilliDb::new(value.state.gain_millidb)
                            .expect("engine group gain is bounded by the model contract"),
                        muted: value.state.muted,
                        soloed: value.soloed,
                        parent: value.parent.filter(|_| nested),
                    },
                )
                .map_err(|error| AppFailure(format!("audio group {group}: {error}")))?;
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
                        .expect("engine aux bus gain is bounded by the model contract"),
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
        project.set_output_mix_minus(output, show.output_mix_minus_input(output));
    }
    project.set_audio_solo_mode(show.audio_solo_mode());
    let monitor = show.audio_monitor();
    project
        .set_audio_monitor_output(fm_model::AudioMonitorOutput {
            device: monitor.device.clone(),
            gain: InputGainMilliDb::new(monitor.state.gain_millidb)
                .expect("engine monitor gain is bounded by the model contract"),
            muted: monitor.state.muted,
        })
        .map_err(|error| AppFailure(format!("monitor output: {error}")))?;
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

fn sync_input_audio_strips(project: &mut Project, show: &ShowState) -> AppResult<()> {
    for (&input, &state) in show.input_audio_strips() {
        project.input_audio_strip(input).ok_or_else(|| {
            AppFailure(format!("project is missing audio strip for input {input}"))
        })?;
        let strip = InputAudioStripState {
            pan: fm_model::InputPanBasisPoints::new(state.pan_basis_points)
                .expect("engine input audio pan is bounded by the model contract"),
            gain: InputGainMilliDb::new(state.gain_millidb)
                .expect("engine input audio gain is bounded by the model contract"),
            balance: InputBalanceBasisPoints::new(state.balance_basis_points)
                .expect("engine input audio balance is bounded by the model contract"),
            muted: state.muted,
            soloed: state.soloed,
            follow_video: state.follow_video,
            delay_samples: InputDelaySamples::new(state.delay_samples)
                .expect("engine input audio delay is bounded by the model contract"),
            sends: state.sends,
        };
        if !project.set_input_audio_strip(input, strip) {
            return Err(AppFailure(format!("project is missing input {input}")).into());
        }
    }
    Ok(())
}

fn persisted_overlays(desired: &SwitcherState, realized: &SwitcherState) -> RuntimeOverlays {
    RuntimeOverlays {
        desired: std::array::from_fn(|index| persisted_overlay(&desired.overlays()[index])),
        realized: std::array::from_fn(|index| persisted_overlay(&realized.overlays()[index])),
    }
}

fn persisted_overlay(channel: &OverlayChannelState) -> RuntimeOverlayChannel {
    RuntimeOverlayChannel {
        source: channel.source(),
        active: channel.is_active(),
        transition: RuntimeOverlayTransition::from_token(
            fm_control::wire_overlay_transition(channel.transition()).token(),
        )
        .expect("every overlay transition has a persisted token"),
        duration_frames: channel.duration_frames(),
        position: persisted_overlay_position(channel.position()),
        border: persisted_overlay_border(channel.border()),
        queued_sources: channel.queued_sources().to_vec(),
        included_outputs: channel.included_outputs().to_vec(),
    }
}

fn persisted_overlay_position(position: OverlayPositionPreset) -> RuntimeOverlayPosition {
    match position {
        OverlayPositionPreset::FullFrame => RuntimeOverlayPosition::FullFrame,
        OverlayPositionPreset::TopLeft => RuntimeOverlayPosition::TopLeft,
        OverlayPositionPreset::TopRight => RuntimeOverlayPosition::TopRight,
        OverlayPositionPreset::BottomLeft => RuntimeOverlayPosition::BottomLeft,
        OverlayPositionPreset::BottomRight => RuntimeOverlayPosition::BottomRight,
    }
}

fn restored_overlay_position(position: RuntimeOverlayPosition) -> OverlayPositionPreset {
    match position {
        RuntimeOverlayPosition::FullFrame => OverlayPositionPreset::FullFrame,
        RuntimeOverlayPosition::TopLeft => OverlayPositionPreset::TopLeft,
        RuntimeOverlayPosition::TopRight => OverlayPositionPreset::TopRight,
        RuntimeOverlayPosition::BottomLeft => OverlayPositionPreset::BottomLeft,
        RuntimeOverlayPosition::BottomRight => OverlayPositionPreset::BottomRight,
    }
}

fn persisted_overlay_border(border: OverlayBorderPreset) -> RuntimeOverlayBorder {
    match border {
        OverlayBorderPreset::None => RuntimeOverlayBorder::None,
        OverlayBorderPreset::ThinWhite => RuntimeOverlayBorder::ThinWhite,
        OverlayBorderPreset::ThickWhite => RuntimeOverlayBorder::ThickWhite,
    }
}

fn restored_overlay_border(border: RuntimeOverlayBorder) -> OverlayBorderPreset {
    match border {
        RuntimeOverlayBorder::None => OverlayBorderPreset::None,
        RuntimeOverlayBorder::ThinWhite => OverlayBorderPreset::ThinWhite,
        RuntimeOverlayBorder::ThickWhite => OverlayBorderPreset::ThickWhite,
    }
}

fn restore_overlays(
    show: &mut ShowState,
    realized: &mut SwitcherState,
    overlays: &RuntimeOverlays,
) -> AppResult<()> {
    for (index, (desired, realized_state)) in
        overlays.desired.iter().zip(&overlays.realized).enumerate()
    {
        let channel = OverlayChannelId::from_index(index).expect("overlay index is bounded");
        restore_overlay(show.desired_switcher_mut(), channel, desired)?;
        restore_overlay(realized, channel, realized_state)?;
    }
    Ok(())
}

fn restore_overlay(
    switcher: &mut SwitcherState,
    channel: OverlayChannelId,
    state: &RuntimeOverlayChannel,
) -> AppResult<()> {
    switcher.configure_overlay_transition(
        channel,
        fm_control::domain_overlay_transition(
            fm_protocol::OverlayTransitionKind::from_token(state.transition.token())
                .expect("every persisted overlay transition has a wire token"),
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

fn restore_stinger(
    show: &mut ShowState,
    realized: &mut SwitcherState,
    config: StingerConfig,
) -> AppResult<()> {
    let slot = StingerSlotId::new(config.slot.number())
        .expect("validated model Stinger slots are in the switcher range");
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
        let _ = show.preload_stinger(slot, true)?;
        let _ = realized.preload_stinger(slot, true)?;
    }
    Ok(())
}

fn persisted_fade_to_black(state: fm_engine::EngineFadeToBlackState) -> PersistedFadeToBlackState {
    PersistedFadeToBlackState::new(
        state.active,
        u16::try_from(state.position.numerator())
            .expect("engine fade-to-black numerator uses the u16 contract"),
    )
}

fn persisted_t_bar(state: TBarState) -> PersistedManualTransitionState {
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
    PersistedManualTransitionState::new(
        kind,
        state.from(),
        state.to(),
        state.interval_start().basis_points(),
        state.position().basis_points(),
    )
    .expect("engine manual transition positions are bounded")
}

fn restored_t_bar(state: PersistedManualTransitionState) -> AppResult<TBarState> {
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

/// Loads a project that is about to be mutated.
///
/// This opens the journal database, which a running daemon owns exclusively:
/// the mutation is refused while the daemon holds it, and refused again if
/// batches the daemon recorded are still unapplied.
pub(crate) fn load_stored_project(path: &Path) -> AppResult<StoredProject> {
    let store = ProjectStore::new(path)?;
    let project = store.load()?;
    if store.journal_path().try_exists()? {
        let scan = store.scan_journal()?;
        if !scan.batches().is_empty() {
            return Err(AppFailure(
                "project has unapplied journal batches that freemix-cli cannot safely interpret"
                    .into(),
            )
            .into());
        }
    }
    Ok(project)
}

/// Reads a project without touching its journal.
///
/// The journal database is locked exclusively by whichever process has it
/// open, so inspecting a project must never open it: reporting a show's state
/// has to work while its daemon is running.
pub(crate) fn inspect_stored_project(path: &Path) -> AppResult<StoredProject> {
    Ok(ProjectStore::new(path)?.load()?)
}

fn audit_assets(path: &Path) -> AppResult<()> {
    let stored = inspect_stored_project(path)?;
    let store = ProjectStore::new(path)?;
    let issues = store.audit_assets(&stored);
    for issue in &issues {
        println!(
            "input={} reason={}",
            issue.input_id,
            project_bundle::audit_reason_name(issue.reason)
        );
    }
    if issues.is_empty() {
        println!("asset-audit: clean");
        Ok(())
    } else {
        Err(AppFailure(format!("asset audit found {} issue(s)", issues.len())).into())
    }
}

fn settings_export(path: &Path, output: &Path) -> AppResult<()> {
    let stored = inspect_stored_project(path)?;
    let snapshot = SettingsSnapshot::from_project(stored.project())
        .map_err(|error| AppFailure(format!("settings export: {error}")))?;
    std::fs::write(output, snapshot.to_export_json())
        .map_err(|error| AppFailure(format!("settings export: {error}")))?;
    println!("settings-export: {} entries", snapshot.len());
    Ok(())
}

fn settings_import(path: &Path, input: &Path, apply: bool) -> AppResult<()> {
    let stored = if apply {
        load_stored_project(path)?
    } else {
        inspect_stored_project(path)?
    };
    let current = SettingsSnapshot::from_project(stored.project())
        .map_err(|error| AppFailure(format!("settings import: {error}")))?;
    let inventory = ContentInventory::from_project(stored.project());
    let source = std::fs::read_to_string(input)
        .map_err(|error| AppFailure(format!("settings import: {error}")))?;
    let plan = ImportPlan::from_export_json(
        &current,
        &source,
        inventory,
        MergeStrategy::MergePreferIncoming,
    )
    .map_err(|error| AppFailure(format!("settings import: {error}")))?;
    let preview = plan.incoming_preview();
    for change in preview.changes() {
        let kind = match change.kind {
            ChangeKind::Added => "added",
            ChangeKind::Changed => "changed",
            ChangeKind::Removed => "removed",
            ChangeKind::Unchanged => continue,
        };
        println!("{kind} {}", change.key.as_str());
    }
    let outcome = plan
        .outcome()
        .map_err(|error| AppFailure(format!("settings import: {error}")))?;
    if apply {
        if let Some(change) = preview.changes().iter().find(|change| {
            change.kind != ChangeKind::Unchanged
                && !matches!(
                    change.key.section(),
                    fm_persistence::SettingsSection::Format
                        | fm_persistence::SettingsSection::RestartPolicy
                )
        }) {
            return Err(AppFailure(format!(
                "settings-import: cannot apply {} changes yet (only format and restart_policy); nothing was saved",
                change.key.section()
            ))
            .into());
        }
        let mut project = stored.project().clone();
        outcome
            .result
            .apply_format_and_restart(&mut project)
            .map_err(|error| AppFailure(format!("settings-import: {error}")))?;
        let configured = stored.with_project(project)?;
        ProjectStore::new(path)?.save(&configured)?;
    }
    println!(
        "settings-import: {} added, {} changed, {} removed; {}",
        preview.count(ChangeKind::Added),
        preview.count(ChangeKind::Changed),
        preview.count(ChangeKind::Removed),
        if apply { "applied" } else { "nothing applied" },
    );
    for key in &outcome.retained_secrets {
        println!("kept-secret {}", key.as_str());
    }
    for conflict in &outcome.conflicts {
        println!("conflict {conflict:?}");
    }
    Ok(())
}

fn title_design_check(path: &Path) -> AppResult<()> {
    let design = fm_persistence::read_title_design_file(path)
        .map_err(|error| AppFailure(format!("title design: {error}")))?;
    println!(
        "title-design: id={} name={} nodes={} roots={} fields={}",
        design.id(),
        design.name(),
        design.node_count(),
        design.roots().len(),
        design.fields().len(),
    );
    Ok(())
}

fn recover_journal(path: &Path) -> AppResult<()> {
    let store = ProjectStore::new(path)?;
    let scan = store.scan_journal()?;
    reject_unapplied_journal_batches(scan.batches().len())?;
    let recovered = store.recover_journal()?;
    reject_unapplied_journal_batches(recovered.batches().len())?;
    for observation in recovered.observations() {
        println!("journal observation: {observation}");
    }
    println!(
        "journal recovered: checkpoint_sequence={} checkpoint_revision={} unapplied_batches=0",
        recovered.checkpoint_sequence(),
        recovered.checkpoint_revision(),
    );
    Ok(())
}

fn reject_unapplied_journal_batches(count: usize) -> AppResult<()> {
    if count == 0 {
        return Ok(());
    }
    Err(AppFailure(format!(
        "journal recovery refused: {count} unapplied journal batch(es) remain; no batches were applied"
    ))
    .into())
}

pub(crate) fn simulated_pipeline(
    project: &Project,
    width: u32,
    height: u32,
) -> AppResult<SimulatedPipeline> {
    let mut pipeline = SimulatedPipeline::new(width, height)?;
    for input in project.inputs() {
        if matches!(input.kind, InputKind::Simulated(_)) {
            pipeline.register(SimulatedSource::new(input.id, source_pattern(input)?))?;
        }
    }
    Ok(pipeline)
}

fn render(path: &Path, output: &Path, width: u32, height: u32) -> AppResult<()> {
    let project = inspect_engine(path)?;
    let engine = &project.engine;
    let pipeline = simulated_pipeline(&project.project, width, height)?;
    let program = engine.realized_switcher().program_frame();
    if program.secondary.is_some()
        && [Some(program.primary), program.secondary]
            .into_iter()
            .flatten()
            .any(|input| is_scene_input(&project.project, input))
    {
        return Err(AppFailure(
            "render does not support transitions with Scene input endpoints".into(),
        )
        .into());
    }
    let scene = project
        .project
        .inputs()
        .iter()
        .find(|input| input.id == program.primary)
        .map(|input| &input.kind)
        .and_then(|kind| match kind {
            InputKind::Scene { scene_id, .. } => Some(*scene_id),
            _ => None,
        });
    let grades = crate::grading::compiled_grades(path, &project.project)?;
    let frame = if let Some(scene) = scene {
        // Scene geometry, crops and masks are authored in project pixels, so
        // the scene is composed at the project dimensions from sources
        // rendered at that size and only then scaled to the requested output.
        let dimensions = project.project.settings().video.dimensions;
        let scene_pipeline =
            simulated_pipeline(&project.project, dimensions.width(), dimensions.height())?;
        // The engine's on-air scene phases are runtime state a local command
        // does not replay: `render` draws the scene's static design and
        // `scene-render-frame` draws any point of its animation.
        crate::scene_render::render_scene(
            &project.project,
            scene,
            fm_model::fm_animation::ClipState::Static,
            &scene_pipeline,
            &grades,
            Some(program.primary),
            engine.frame_cursor().get(),
            width,
            height,
        )?
    } else {
        for input in [Some(program.primary), program.secondary]
            .into_iter()
            .flatten()
        {
            if let Some(input) = project
                .project
                .inputs()
                .iter()
                .find(|candidate| candidate.id == input)
            {
                source_pattern(input)?;
            }
        }
        // Each input is graded before the transition mixes it; the program
        // grade colours the mixed picture.
        let mixed =
            pipeline.render_mapped(engine.frame_cursor().get(), program, |input, frame| {
                match grades.input(input) {
                    Some(grade) => fm_compositor::apply_grade(&frame, grade),
                    None => Ok(frame),
                }
            })?;
        match grades.program() {
            Some(grade) => fm_compositor::apply_grade(&mixed, grade)?,
            None => mixed,
        }
    };
    write_ppm_atomic(output, |file| write_ppm(&frame, file))?;
    println!(
        "rendered {}x{} Program input {} to {}",
        width,
        height,
        engine.realized_switcher().program(),
        output.display()
    );
    Ok(())
}

fn is_scene_input(project: &Project, input: InputId) -> bool {
    project
        .inputs()
        .iter()
        .any(|candidate| candidate.id == input && matches!(candidate.kind, InputKind::Scene { .. }))
}

/// Writes a rendered title frame as a binary PPM, atomically.
pub(crate) fn write_title_ppm(output: &Path, frame: &ImageFrame) -> io::Result<()> {
    write_ppm_atomic(output, |file| write_ppm(frame, file))
}

fn write_ppm_atomic(
    output: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut name = std::ffi::OsString::from(".");
    name.push(output.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "PPM output has no file name")
    })?);
    name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        PPM_OUTPUT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = parent.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let mut cleanup = PpmTempGuard(Some(temp.clone()));
    write(&mut file)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, output)?;
    cleanup.disarm();
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

struct PpmTempGuard(Option<PathBuf>);

impl PpmTempGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for PpmTempGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn demo(path: &Path, output: Option<&Path>) -> AppResult<()> {
    // Like `new`, the demo only ever creates a fresh bundle; an existing one
    // (possibly owned by a running daemon through its journal) is never
    // overwritten.
    if path.try_exists()? {
        return Err(AppFailure(format!(
            "destination bundle already exists: {}",
            path.display()
        ))
        .into());
    }
    let mut project = default_project("FreeMix MVP Demo".into())?;
    println!("created deterministic two-input show");
    print_status(&project);

    reject_unexpected(execute(
        &mut project.engine,
        EngineCommand::Cut,
        1,
        Some("demo-cut".into()),
        Some(0),
    )?)?;
    println!("realized Cut on frame boundary");
    print_status(&project);

    reject_unexpected(execute(
        &mut project.engine,
        EngineCommand::Fade { duration_frames: 4 },
        4,
        Some("demo-fade".into()),
        Some(1),
    )?)?;
    println!("realized four-frame Fade");
    save_engine(path, &project)?;
    print_status(&project);

    let reloaded = load_engine(path)?;
    println!("reloaded persisted show");
    print_status(&reloaded);
    if let Some(output) = output {
        render(path, output, 640, 360)?;
    }
    Ok(())
}

fn reject_unexpected(result: ExecuteResult) -> AppResult<()> {
    match result.rejection {
        Some(rejection) => Err(rejection.into()),
        None => Ok(()),
    }
}

fn print_status(project: &ProjectEngine) {
    let engine = &project.engine;
    let desired = engine.show().desired_switcher();
    let realized = engine.realized_switcher();
    println!(
        "project_id={} show={:?} revision={} frame={} Program(desired={}, realized={}) Preview(desired={}, realized={}) TBar(desired={}, realized={}) FTB(desired={}, realized={}) Overlays(desired={}, realized={}) AudioStrips={} Stingers={} recording_desired={}",
        project.project.id(),
        engine.show().name(),
        engine.revision(),
        engine.frame_cursor(),
        desired.program(),
        realized.program(),
        desired.preview(),
        realized.preview(),
        format_t_bar(desired.t_bar()),
        format_t_bar(realized.t_bar()),
        format_fade_to_black(engine.desired_fade_to_black()),
        format_fade_to_black(engine.realized_fade_to_black()),
        format_overlays(desired.overlays()),
        format_overlays(realized.overlays()),
        format_audio_strips(&project.project),
        format_stingers(project.project.stingers()),
        format_recording_desired(desired),
    );
}

/// Maps one validated CLI recorder slot onto the switcher's slot domain.
///
/// Both domains cover exactly `1..=2`, so the argument layer's validated
/// value always converts.
fn switcher_recorder(recorder: u8) -> AppResult<fm_switcher::RecorderSlot> {
    fm_switcher::RecorderSlot::new(recorder)
        .ok_or_else(|| AppFailure(format!("recorder slot {recorder} is out of range")).into())
}

/// Maps one validated model recorder slot onto the switcher's slot domain.
fn switcher_recorder_slot(slot: ModelRecorderSlot) -> fm_switcher::RecorderSlot {
    fm_switcher::RecorderSlot::new(slot.get())
        .expect("model recorder slots are in the switcher range")
}

/// Converts one validated CLI recorder slot into its wire representation.
fn wire_recorder(recorder: u8) -> AppResult<fm_protocol::WireRecorderSlot> {
    fm_protocol::WireRecorderSlot::new(recorder)
        .ok_or_else(|| AppFailure(format!("recorder slot {recorder} is out of range")).into())
}

/// Renders both recorder slots' desired flags in slot order, for example
/// `[false,true]` for slot 1 stopped and slot 2 recording.
fn format_recording_desired(desired: &SwitcherState) -> String {
    let mut formatted = String::from("[");
    for (index, slot) in ModelRecorderSlot::ALL.into_iter().enumerate() {
        if index != 0 {
            formatted.push(',');
        }
        formatted.push_str(
            &desired
                .recording_desired(switcher_recorder_slot(slot))
                .to_string(),
        );
    }
    formatted.push(']');
    formatted
}

fn print_inputs(project: &Project) {
    for input in project.inputs() {
        let scene = match &input.kind {
            InputKind::Scene {
                scene_id,
                audio_source,
            } => format!(
                " scene_id={scene_id} audio_source={}",
                audio_source.map_or_else(|| "none".to_owned(), |input| input.to_string()),
            ),
            InputKind::Title(title) => format!(
                " template={} field_revision={} fields={}",
                title.template_uri,
                title.field_revision,
                title.fields.len(),
            ),
            _ => String::new(),
        };
        let strip = project.input_audio_strip(input.id).map_or_else(
            || "unavailable".to_owned(),
            |state| {
                format!(
                    "gain_mdb={},balance_bp={},delay_samples={},muted={},soloed={},follow_video={}",
                    state.gain.get(),
                    state.balance.get(),
                    state.delay_samples.get(),
                    state.muted,
                    state.soloed,
                    state.follow_video,
                )
            },
        );
        println!(
            "input id={} name={:?} kind={}{} strip={strip}",
            input.id,
            input.name,
            input_kind_name(&input.kind),
            scene,
        );
    }
}

fn print_outputs(project: &Project) {
    for output in project.outputs() {
        let scene = project
            .scenes()
            .iter()
            .find(|scene| scene.id == output.video_source)
            .expect("validated output scene reference");
        let bus = project
            .audio_buses()
            .iter()
            .find(|bus| bus.id == output.audio_source)
            .expect("validated output audio bus reference");
        let startup = match output.startup {
            StartupPolicy::Stopped => "stopped",
            StartupPolicy::ReconcileDesiredState => "reconcile-desired-state",
        };
        let capabilities = output
            .required_capabilities
            .iter()
            .map(|capability| format!("{capability:?}"))
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "output id={} name={:?} video_scene={} video_scene_name={:?} audio_bus={} audio_bus_name={:?} startup={startup} capabilities=[{capabilities}]",
            output.id, output.name, output.video_source, scene.name, output.audio_source, bus.name,
        );
    }
}

/// Lists configured streaming destinations with the stream key redacted.
///
/// The URLs printed here come from [`StreamTarget::redacted_url`], which
/// renders the key as `****`. An HLS destination has no key: its local
/// `hls:///absolute/dir` output path is operator-chosen storage rather than a
/// secret and is printed verbatim. Its authored ladder renders as
/// `ladder=640x360@800,1280x720@1500`, and a destination without one renders
/// `ladder=none`. No line in this program prints the key.
fn print_stream_targets(project: &Project) {
    for target in project.stream_targets() {
        let output = project
            .outputs()
            .iter()
            .find(|output| output.id == target.output())
            .expect("validated stream destination output reference");
        let startup = match target.startup() {
            StartupPolicy::Stopped => "stopped",
            StartupPolicy::ReconcileDesiredState => "reconcile-desired-state",
        };
        // An SRT destination also prints its latency and whether it is
        // encrypted, never the passphrase.
        let srt = target.srt_latency_ms().map_or_else(String::new, |latency| {
            format!(
                " srt_latency_ms={latency} passphrase={}",
                if target.srt_passphrase().is_some() {
                    "yes"
                } else {
                    "no"
                }
            )
        });
        println!(
            "stream id={} name={:?} protocol={} url={:?} backup_url={:?} output={} output_name={:?} startup={startup} video_bitrate_kbps={} ladder={} reconnect={}{srt} running={}",
            target.id(),
            target.name(),
            target.protocol(),
            target.redacted_url(),
            target
                .redacted_backup_url()
                .unwrap_or_else(|| "none".to_owned()),
            target.output(),
            output.name,
            target.video_bitrate_kbps(),
            target
                .ladder()
                .map_or_else(|| "none".to_owned(), ToString::to_string),
            format_reconnect(
                target.reconnect().initial_delay_ms(),
                target.reconnect().max_delay_ms(),
                target.reconnect().max_attempts(),
            ),
            target.running(),
        );
    }
}

/// Renders a reconnect policy in the `--reconnect` syntax it is authored in:
/// `<initial_ms>:<max_ms>:<attempts|none>`.
pub(crate) fn format_reconnect(
    initial_delay_ms: u32,
    max_delay_ms: u32,
    max_attempts: Option<u32>,
) -> String {
    format!(
        "{initial_delay_ms}:{max_delay_ms}:{}",
        max_attempts.map_or_else(|| "none".to_owned(), |attempts| attempts.to_string())
    )
}

fn print_audio_buses(project: &Project) {
    for bus in project.audio_buses() {
        let sends = bus
            .sends
            .iter()
            .map(|send| send.destination.to_string())
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "audio_bus id={} name={:?} sends=[{sends}]",
            bus.id, bus.name,
        );
    }
    for (bus, value) in project.aux_buses().iter() {
        let dsp = if value.dsp.is_default() {
            "flat".to_owned()
        } else {
            value.dsp.code()
        };
        println!(
            "audio_aux_bus bus={} name={:?} gain_millidb={} muted={} dsp={dsp} omt={}",
            bus.letter(),
            value.name,
            value.gain.get(),
            value.muted,
            value.omt
        );
    }
}

/// A standalone live scene layer edit (no gesture) of layer `layer` of
/// scene `scene` (IN-020).
fn remote_scene_layer_edit(
    scene: u128,
    layer: u32,
    operation: fm_protocol::SceneLayerOperation,
) -> AppResult<fm_protocol::CommandPayload> {
    let layer = core::num::NonZeroU32::new(layer)
        .ok_or_else(|| AppFailure("layer ID must be nonzero".into()))?;
    Ok(fm_protocol::CommandPayload::SceneLayer(
        fm_protocol::SceneLayerEdit {
            scene: fm_protocol::WireSceneId::from_domain(scene_id(scene)?),
            layer: fm_protocol::WireSceneLayerId::new(layer),
            operation,
            continues: false,
            gesture: 0,
        },
    ))
}

/// Renders one layer's optional key, or `none` (IN-020), then its matte
/// refinement when that is not the identity (SW-008).
fn format_layer_key(key: Option<fm_model::LayerKey>) -> String {
    let Some(key) = key else {
        return "none".to_owned();
    };
    let refinement = key.refinement();
    let kind = format_key_kind(key);
    if refinement.is_identity() {
        return kind;
    }
    let despill = match refinement.despill {
        fm_model::KeyDespill::Off => "off".to_owned(),
        fm_model::KeyDespill::Green { strength } => format!("green:{strength}"),
        fm_model::KeyDespill::Blue { strength } => format!("blue:{strength}"),
    };
    let light_wrap = if refinement.light_wrap == 0 {
        String::new()
    } else {
        format!(
            " light_wrap={}:{}",
            refinement.light_wrap, refinement.light_wrap_width
        )
    };
    let matte = refinement.garbage_matte.map_or_else(String::new, |matte| {
        let points = matte
            .points()
            .iter()
            .map(|point| format!("{}:{}", point.x, point.y))
            .collect::<Vec<_>>()
            .join(",");
        let side = if matte.invert() { "outside" } else { "inside" };
        let feather = if matte.feather() == 0 {
            String::new()
        } else {
            format!(" matte_feather={}", matte.feather())
        };
        format!(" matte={side}:{points}{feather}")
    });
    format!(
        "{kind}+refine(clip={},{} choke={} edge_softness={} despill={despill}{light_wrap}{matte})",
        refinement.clip_black, refinement.clip_white, refinement.choke, refinement.edge_softness
    )
}

/// Renders one key's kind and settings (IN-020, SW-008).
fn format_key_kind(key: fm_model::LayerKey) -> String {
    match key {
        fm_model::LayerKey::Chroma(chroma) => format!(
            "chroma(rgb={},{},{} similarity={} smoothness={} spill={})",
            chroma.red,
            chroma.green,
            chroma.blue,
            chroma.similarity,
            chroma.smoothness,
            chroma.spill
        ),
        fm_model::LayerKey::Luma(luma) => format!(
            "luma(low={} high={} softness={} invert={})",
            luma.low,
            luma.high,
            luma.softness,
            if luma.invert { "on" } else { "off" }
        ),
        fm_model::LayerKey::Fill(fill) => format!(
            "fill(key_input={} channel={} low={} high={} softness={} invert={})",
            fill.key_input,
            match fill.channel {
                fm_model::KeyChannel::Luma => "luma",
                fm_model::KeyChannel::Alpha => "alpha",
            },
            fill.low,
            fill.high,
            fill.softness,
            if fill.invert { "on" } else { "off" }
        ),
    }
}

fn print_scenes(project: &Project) {
    for scene in project.scenes() {
        let background = scene.background;
        println!(
            "scene id={} name={:?} background=rgba({},{},{},{}) layers={}",
            scene.id,
            scene.name,
            background.red,
            background.green,
            background.blue,
            background.alpha,
            scene.layers.len(),
        );
        for (index, layer) in scene.layers.iter().enumerate() {
            println!(
                "  layer index={index} id={} name={:?} mask={} key={} effects={}",
                layer.id,
                layer.name,
                format_layer_mask(layer.mask),
                format_layer_key(layer.key),
                format_layer_effects(&layer.effects),
            );
        }
    }
}

/// Renders one layer's optional rectangular mask, or `none`.
fn format_layer_mask(mask: Option<RectMask>) -> String {
    mask.map_or_else(
        || "none".to_owned(),
        |mask| {
            format!(
                "{}x{}@{},{}:{}",
                mask.width,
                mask.height,
                mask.x,
                mask.y,
                if mask.invert { "inverted" } else { "normal" },
            )
        },
    )
}

/// Prints the daemon's as-run log beside the project bundle (OR-015).
fn print_as_run(path: &Path) -> AppResult<()> {
    let log = path.join("as-run.log");
    let text = fs::read_to_string(&log)
        .map_err(|error| AppFailure(format!("cannot read {}: {error}", log.display())))?;
    print!("{text}");
    Ok(())
}

/// Renders one layer's ordered effects as `name(parameter,...)` entries joined
/// by `;`, or `none` when the layer carries no effects.
fn format_layer_effects(effects: &[LayerEffect]) -> String {
    if effects.is_empty() {
        return "none".to_owned();
    }
    effects
        .iter()
        .map(|effect| {
            let parameters = effect
                .parameters
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            format!("{}({parameters})", effect.name)
        })
        .collect::<Vec<_>>()
        .join(";")
}

const fn input_kind_name(kind: &InputKind) -> &'static str {
    match kind {
        InputKind::Color => "color",
        InputKind::Media { .. } => "media",
        InputKind::Device { .. } => "device",
        InputKind::NetworkStream(_) => "network-stream",
        InputKind::SystemAudio(_) => "system-audio",
        InputKind::Image(_) => "image",
        InputKind::Slideshow(_) => "slideshow",
        InputKind::ScreenCapture(_) => "screen-capture",
        InputKind::Scene { .. } => "scene",
        InputKind::Simulated(_) => "simulated",
        InputKind::Alias { .. } => "alias",
        InputKind::MixFeed(fm_model::MixFeed::Program) => "program-feed",
        InputKind::MixFeed(fm_model::MixFeed::Preview) => "preview-feed",
        InputKind::Title(_) => "title",
        InputKind::Document(_) => "document",
        InputKind::Replay => "replay",
    }
}

fn format_audio_strips(project: &Project) -> String {
    let strips = project
        .input_audio_strips()
        .iter()
        .map(|strip| {
            let name = project
                .inputs()
                .iter()
                .find(|input| input.id == strip.input)
                .map_or("", |input| input.name.as_str());
            format!(
                "{}:{name:?}:gain_mdb={}:balance_bp={}:delay_samples={}:{}:{}:{}",
                strip.input,
                strip.state.gain.get(),
                strip.state.balance.get(),
                strip.state.delay_samples.get(),
                if strip.state.muted { "muted" } else { "live" },
                if strip.state.soloed { "solo" } else { "mix" },
                if strip.state.follow_video {
                    "afv"
                } else {
                    "always"
                },
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{strips}]")
}

fn format_overlays(overlays: &[OverlayChannelState; 8]) -> String {
    let channels = overlays
        .iter()
        .enumerate()
        .map(|(index, overlay)| {
            let outputs = overlay
                .included_outputs()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("+");
            format!(
                "{}:{}:{}:opacity={}:{}@{}:{}:{}:queue=[{}]:outputs=[{}]",
                index + 1,
                overlay
                    .source()
                    .map_or_else(|| "none".to_owned(), |source| source.to_string()),
                if overlay.is_active() { "on" } else { "off" },
                overlay.opacity(),
                crate::args::cli_token(
                    fm_control::wire_overlay_transition(overlay.transition()).token()
                ),
                overlay.duration_frames(),
                overlay_position_name(overlay.position()),
                overlay_border_name(overlay.border()),
                overlay
                    .queued_sources()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("+"),
                outputs,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{channels}]")
}

fn format_stingers(stingers: &[StingerConfig]) -> String {
    let configured = stingers
        .iter()
        .map(|config| {
            format!(
                "{}:{}:{}:{}:{}:{}",
                config.slot.number(),
                config.media_input,
                if config.preload {
                    "preload"
                } else {
                    "deferred"
                },
                config.cut_point_frames,
                match config.audio_policy {
                    ModelStingerAudioPolicy::Muted => "muted",
                    ModelStingerAudioPolicy::StingerOnly => "stinger-only",
                    ModelStingerAudioPolicy::MixWithProgram => "mix-with-program",
                },
                match config.missing_media_fallback {
                    StingerMissingMediaFallback::Cut => "cut",
                    StingerMissingMediaFallback::Fade => "fade",
                    StingerMissingMediaFallback::KeepProgram => "keep-program",
                },
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{configured}]")
}

fn format_fade_to_black(state: EngineFadeToBlackState) -> String {
    format!(
        "{}@{}/{}",
        if state.active { "black" } else { "live" },
        state.position.numerator(),
        state.position.denominator(),
    )
}

fn format_t_bar(state: Option<TBarState>) -> String {
    state.map_or_else(
        || "inactive".to_owned(),
        |state| {
            format!(
                "{}:{}->{}@{}",
                fm_control::wire_manual_kind(EngineManualTransitionKind::from_t_bar(state)).token(),
                state.from(),
                state.to(),
                state.position().basis_points(),
            )
        },
    )
}

#[allow(clippy::too_many_lines)]
fn print_help() {
    println!(
        "\
FreeMix deterministic MVP

Usage:
  freemix-cli new <show.freemix> [--name <name>]
  freemix-cli input-add <show.freemix> <nonzero-input-id> <name>
  freemix-cli simulated-solid-input-add <show.freemix> <nonzero-input-id> <name> <red:0..=255> <green:0..=255> <blue:0..=255> <alpha:0..=255>
  freemix-cli media-input-add <show.freemix> <nonzero-input-id> <name> <asset://key>
  freemix-cli media-input-relink <show.freemix> <existing-media-input-id> <asset://key>
  freemix-cli title-input-add <show.freemix> --id <nonzero-input-id> --name <name> --template <source.fmtitle> [--font <Family=path.ttf>]...
      Imports a title template and adds a title input showing its default
      field values (field_revision 0). The source is a .fmtitle document in
      the stored format (schema_version equal to the current project schema,
      a \"template\" object, \"fonts\": [], and a \"clocks\" table defining
      exactly the production clocks its elements show: time_of_day in a
      POSIX TZ zone, countdown_to a UTC time, or since_take); every font
      family the template draws with is supplied once with --font and must
      parse as a font. The template is stored as
      assets/titles/<source-stem>.fmtitle and fonts are copied to
      assets/fonts/, reusing identical files. Image field defaults
      must already be bundle asset:// URIs that resolve.
  freemix-cli title-animation-preset <show.freemix> <asset://titles/...fmtitle> <element-id> --phase in|out --preset <id> --frames <n>
      Applies a built-in animation preset (fade, slide-from-left,
      slide-from-right, slide-from-top, slide-from-bottom, pop, wipe, drop) to
      one element over n frames of the template's animation timebase,
      growing the phase to n frames when shorter. In presets end at the
      static design and Out presets start there. The stored template is
      rewritten in place and its animation revision advances.
  freemix-cli title-animation-stretch <show.freemix> <asset://titles/...fmtitle> --phase in|out --frames <n>
      Sets a phase to n frames, scaling every key of the phase
      proportionally (frames rounded half up).
  freemix-cli title-render-frame <show.freemix> <title-input-id> --phase in|out --frame <f> [--at <YYYY-MM-DDTHH:MM:SSZ>] <out.ppm>
      Renders the title input's field values at frame f of the phase, at
      the template canvas size, with the bundle's fonts and decoded PNG or
      JPEG images, exactly as freemixd draws it. Production clocks read the
      UTC instant --at (the Unix epoch when absent) for time of day and
      countdowns, and the frames since the take (the title taken at the
      first In frame, Out following In with no hold) for since-take clocks.
  freemix-cli scene-animation-preset <show.freemix> <scene-id> <layer-id> --phase in|out --preset <id> --frames <n>
      Applies a built-in animation preset (the title presets) to one layer
      of a scene over n frames (30/1 timebase), growing the phase to n
      frames when shorter. Off-canvas distances come from the layer's
      footprint and the project canvas. Layers animate position, scale,
      rotation (any angle, clockwise about the layer anchor), crop, and
      opacity.
      The scene plays In when it reaches Program or Preview, holds, and
      plays Out while a transition takes it off Program (a cut plays no
      Out); nested and multiview scenes hold.
  freemix-cli scene-animation-stretch <show.freemix> <scene-id> --phase in|out --frames <n>
      Sets a scene animation phase to n frames, scaling every key of the
      phase proportionally (frames rounded half up).
  freemix-cli scene-render-frame <show.freemix> <scene-id> --phase in|hold|out [--frame <f>] <out.ppm> [--width <px>] [--height <px>]
      Renders the scene posed at frame f of the In or Out phase (or held
      at the In end, which takes no --frame) from the simulated sources,
      composed at the project canvas and scaled like render.
  freemix-cli grade-set <show.freemix> <input-id|layer <scene-id> <layer-id>|program> [--preset <id>] [--exposure <millistops>] [--temperature <t>] [--tint <t>] [--contrast <amount>] [--saturation <permille>] [--lut <lut-id> [--lut-strength <permille>]] [--save-as <name>]
  freemix-cli grade-set <show.freemix> <input-id|layer <scene-id> <layer-id>|program> --grade <grade-id>
      Grades an input (before its scene-layer transform) or the program
      output. Values apply over a built-in preset (neutral, warm, cool,
      punchy, faded, bw); --save-as keeps the grade in the project library
      and assigns it by reference, --grade assigns a library grade.
  freemix-cli grade-clear <show.freemix> <input-id|layer <scene-id> <layer-id>|program>
  freemix-cli grade-import-lut <show.freemix> <file.cube> [--name <name>]
  freemix-cli shortcuts <show.freemix>
  freemix-cli remote-shortcuts <127.0.0.1:port>
  freemix-cli shortcut-export <show.freemix> <file.fmshortcuts> [--name <set name>]
  freemix-cli remote-shortcut-export <127.0.0.1:port> <file.fmshortcuts> [--name <set name>]
  freemix-cli shortcut-import <show.freemix> <file.fmshortcuts>
  freemix-cli remote-shortcut-import <127.0.0.1:port> <file.fmshortcuts>
  freemix-cli shortcut-fire <show.freemix> <shortcut> [--selected <input>] [--value <0-1000>] [--key <key>] [--expect <revision>]
  freemix-cli remote-shortcut-fire <127.0.0.1:port> <shortcut> [--selected <input>] [--value <0-1000>] [--key <key>] [--expect <revision>]
      Fires project shortcut <shortcut> (its id, as `shortcuts` lists it)
      as the command it stands for. --selected names the input its actions
      on the selected input act on (RC-004); without it such a fire is
      refused.
      Lists, exports, or imports the project's shortcut set (RC-004). An
      import redefines shortcuts it names by an existing name and adds the
      rest; it imports nothing when a key or OSC name would clash.
  freemix-cli grade-export <show.freemix> <grade-id> <file.fmgrade> [--replace-lut]
  freemix-cli grade-import <show.freemix> <file.fmgrade> [--name <name>]
  freemix-cli preset-export <show.freemix> <preset-id> <file.fmpreset>
  freemix-cli preset-import <show.freemix> <file.fmpreset> [--name <name>]
  freemix-cli preset-save <show.freemix> <template-uri> <element-id> --phase <in|out> --name <name>
      Copies a .cube LUT (at most 16 MiB, 65^3) to assets/luts/<id>.cube.
  freemix-cli grade-list <show.freemix>
  freemix-cli audio-bus-add <show.freemix> <nonzero-bus-id> <name>
  freemix-cli audio-bus-rename <show.freemix> <existing-bus-id> <name>
  freemix-cli audio-bus-remove <show.freemix> <existing-bus-id>
  freemix-cli audio-bus-send-add <show.freemix> <source-bus-id> <destination-bus-id>
  freemix-cli audio-bus-send-remove <show.freemix> <source-bus-id> <destination-bus-id>
  freemix-cli output-add <show.freemix> <nonzero-output-id> <nonzero-scene-id> <nonzero-bus-id> <name>
  freemix-cli output-route <show.freemix> <existing-output-id> <existing-scene-id> <existing-bus-id>
  freemix-cli output-rename <show.freemix> <existing-output-id> <name>
  freemix-cli output-startup <show.freemix> <existing-output-id> <stopped|reconcile-desired-state>
  freemix-cli output-remove <show.freemix> <existing-output-id>
  freemix-cli stream-add <show.freemix> <nonzero-stream-id> <existing-output-id> <rtmp(s)://host/app|srt://host[:port]|hls:///abs/dir> [<stream-key>] <name> [--backup <url matching the primary scheme>] [--startup <stopped|reconcile-desired-state>] [--video-bitrate-kbps <1000..=100000>] [--rung <video_bitrate_kbps>:<width>x<height> (hls:// only, repeat 2..=8)] [--reconnect <initial_ms>:<max_ms>:<attempts|none>] [--srt-latency-ms <20..=8000> (srt:// only)] [--srt-passphrase <10..=79 chars> (srt:// only)]
  freemix-cli stream-update <show.freemix> <existing-stream-id> <existing-output-id> <rtmp(s)://host/app|srt://host[:port]|hls:///abs/dir> [<stream-key>] <name> [--backup <url matching the primary scheme>] [--startup <stopped|reconcile-desired-state>] [--video-bitrate-kbps <1000..=100000>] [--rung <video_bitrate_kbps>:<width>x<height> (hls:// only, repeat 2..=8)] [--reconnect <initial_ms>:<max_ms>:<attempts|none>] [--srt-latency-ms <20..=8000> (srt:// only)] [--srt-passphrase <10..=79 chars> (srt:// only)]
  freemix-cli stream-remove <show.freemix> <existing-stream-id>
  freemix-cli stream-start <show.freemix> <existing-stream-id>
  freemix-cli stream-stop <show.freemix> <existing-stream-id>
  freemix-cli stream-start-all <show.freemix>
  freemix-cli stream-stop-all <show.freemix>
      Starts or stops every destination at once (up to five, each otherwise
      independent); recording is never affected. --reconnect sets how a
      destination reconnects after a drop: the first retry waits initial_ms
      (100..=300000), each later one doubles up to max_ms, and after
      1..=10000 undelivered attempts it gives up (`none` retries forever);
      the default is 250:2000:40, printed by `streams` as reconnect=...
      An srt:// destination connects as a caller with --srt-latency-ms
      (default 120) and, with --srt-passphrase, AES encryption; `streams`
      prints srt_latency_ms=... passphrase=yes|no, never the passphrase.
      stream-update replaces the whole configuration, passphrase included.
  freemix-cli record-start <show.freemix> [--recorder <1|2>]
  freemix-cli record-stop <show.freemix> [--recorder <1|2>]
      The stream key is a positional argument and is therefore visible in this
      machine's process list. It is stored in plaintext in project.json, like
      every other authored field, so protect the bundle accordingly. It is
      never printed: `streams` and every error render it as ****. An
      hls:///abs/dir destination writes local segments and carries no key: it
      omits the <stream-key> positional and refuses --backup; its local output
      path is not a secret and is printed verbatim. The repeated
      --rung <video_bitrate_kbps>:<width>x<height> option authors that
      destination's multi-bitrate ladder: 2 to 8 rungs with strictly
      increasing bitrates and nondecreasing even dimensions, printed as
      `ladder=640x360@1000,1280x720@2500`; the top rung becomes the
      destination's video_bitrate_kbps. Every other destination refuses
      --rung.
  freemix-cli scene-input-add <show.freemix> <nonzero-input-id> <nonzero-scene-id> <name>
  freemix-cli scene-input-duplicate <show.freemix> <source-scene-id> <new-scene-id> <new-input-id> <scene-name> <input-name>
  freemix-cli scene-input-audio-source <show.freemix> <scene-input-id> <source-input-id>
  freemix-cli scene-input-audio-source-clear <show.freemix> <scene-input-id>
  freemix-cli scene-input-remove <show.freemix> <scene-input-id>
  freemix-cli scene-background <show.freemix> <scene-id> <premultiplied-red:0..=255> <premultiplied-green:0..=255> <premultiplied-blue:0..=255> <alpha:0..=255>
  freemix-cli scene-remove <show.freemix> <existing-scene-id>
  freemix-cli scene-rename <show.freemix> <existing-scene-id> <scene-name>
  freemix-cli scene-layer-add <show.freemix> <scene-id> <source-input-id> <z-order> <layer-name>
  freemix-cli scene-layer-add-scene <show.freemix> <target-scene-id> <source-scene-id> <z-order> <layer-name>
  freemix-cli scene-layer-remove <show.freemix> <scene-id> <zero-based-layer-index>
  freemix-cli scene-layer-move <show.freemix> <scene-id> <source-zero-based-layer-index> <destination-zero-based-layer-index>
  freemix-cli scene-layer-copy <show.freemix> <source-scene-id> <source-zero-based-layer-index> <target-scene-id> <new-name>
  freemix-cli scene-layer-rename <show.freemix> <scene-id> <zero-based-layer-index> <name>
  freemix-cli scene-layer-source-input <show.freemix> <scene-id> <zero-based-layer-index> <input-id>
  freemix-cli scene-layer-source-scene <show.freemix> <scene-id> <zero-based-layer-index> <source-scene-id>
  freemix-cli scene-layer-z-order <show.freemix> <scene-id> <zero-based-layer-index> <signed-z-order>
  freemix-cli scene-layer-appearance <show.freemix> <scene-id> <zero-based-layer-index> <enabled:on|off> <opacity:0..=255>
  freemix-cli scene-layer-geometry <show.freemix> <scene-id> <zero-based-layer-index> <x> <y> <width> <height> <rotation-degrees> [--anchor <x-basis-points> <y-basis-points>]
  freemix-cli scene-layer-crop <show.freemix> <scene-id> <zero-based-layer-index> <x> <y> <width> <height>
  freemix-cli scene-layer-crop-clear <show.freemix> <scene-id> <zero-based-layer-index>
  freemix-cli scene-layer-mask <show.freemix> <scene-id> <zero-based-layer-index> <x> <y> <width> <height> <normal|inverted>
  freemix-cli scene-layer-mask-clear <show.freemix> <scene-id> <zero-based-layer-index>
  freemix-cli scene-layer-effect-add <show.freemix> <scene-id> <zero-based-layer-index> <name> [<i32-parameter>...]
  freemix-cli scene-layer-effect-set <show.freemix> <scene-id> <zero-based-layer-index> <zero-based-effect-index> <name> [<i32-parameter>...]
  freemix-cli scene-layer-effect-remove <show.freemix> <scene-id> <zero-based-layer-index> <zero-based-effect-index>
  freemix-cli scene-layer-effect-clear <show.freemix> <scene-id> <zero-based-layer-index>
  freemix-cli scene-layer-key <show.freemix> <scene-id> <zero-based-layer-index> none|chroma <red> <green> <blue> <similarity> <smoothness> <spill>|luma <low> <high> <softness> <invert:on|off>|fill <key-input-id> luma|alpha <low> <high> <softness> <invert:on|off> [--clip <black> <white>] [--choke <-8..8>] [--edge-softness <0..8>] [--despill green|blue <strength>] [--light-wrap <amount> <1..8>] [--matte inside|outside <x:y,...> [--matte-feather <0..64>]]
  freemix-cli remote-scene-layer-key <127.0.0.1:port> <scene-id> <layer-id> <key spec as above> [--key <key>] [--expect <revision>]
  freemix-cli scene-layer-auto-key <show.freemix> <scene-id> <zero-based-layer-index> [--picture <file.ppm>] [--point <x> <y>]
  freemix-cli remote-scene-layer-auto-key <127.0.0.1:port> <scene-id> <layer-id> [--picture <file.ppm>] [--point <x> <y>] [--clip <black> <white>] [--choke <-8..8>] [--edge-softness <0..8>] [--light-wrap <amount> <1..8>] [--matte inside|outside <x:y,...> [--matte-feather <0..64>]] [--key <key>] [--expect <revision>]
  freemix-cli remote-scene-layer-effects <127.0.0.1:port> <scene-id> <layer-id> none|<name>[:<i32>,...]... [--key <key>] [--expect <revision>]
  freemix-cli project-rename <show.freemix> <name>
  freemix-cli input-remove <show.freemix> <input-id>
  freemix-cli input-duplicate <show.freemix> <source-input-id> <new-nonzero-input-id> <new-name>
  freemix-cli input-replace-simulated <show.freemix> <input-id>
  freemix-cli input-replace-solid <show.freemix> <input-id> <red:0..=255> <green:0..=255> <blue:0..=255> <alpha:0..=255>
  freemix-cli input-replace-media <show.freemix> <input-id> <asset://key>
  freemix-cli input-replace-scene <show.freemix> <input-id> <existing-scene-id>
  freemix-cli input-media-list <show.freemix>
  freemix-cli input-media-list-navigate <show.freemix> <input-id> <next|previous|first|last|goto <position>|item <id>> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-media-list-navigate <address> <input-id> <next|previous|first|last|goto <position>|item <id>> [--key <key>] [--expect <revision>]
  freemix-cli input-media-list-options <show.freemix> <input-id> <auto-next on|off> <auto-first on|off> <loop on|off> <shuffle seed|off> [--key <key>] [--expect <revision>]
  freemix-cli input-media-list-item <show.freemix> <input-id> add <asset://key> <title|-> | remove <item-id> | title <item-id> <title|-> | marks <item-id> <in> <out|end> [--key <key>] [--expect <revision>]
  freemix-cli input-media-list-import <show.freemix> <input-id> <playlist.m3u>
  freemix-cli input-pages <show.freemix>
  freemix-cli input-page <show.freemix> <input-id> <next|previous|first|last|goto <page>> [--key <key>] [--expect <revision>]
  freemix-cli input-categories <show.freemix> [<id>=<name>...] [--key <key>] [--expect <revision>]
  freemix-cli input-category <show.freemix> <input-id> <category-id|none> [--key <key>] [--expect <revision>]
  freemix-cli input-color <show.freemix> <input-id> <red|orange|yellow|green|teal|blue|purple|pink|none> [--key <key>] [--expect <revision>]
  freemix-cli input-pause <show.freemix> <input-id> <on|off> [--key <key>] [--expect <revision>]
  freemix-cli input-close <show.freemix> <input-id> <on|off> [--key <key>] [--expect <revision>]
  freemix-cli input-import-batch <show.freemix> <first-new-input-id> <media-file>...
  freemix-cli input-media-<trim|rate|end-action|end-trigger|audio-track|audio-policy> <show.freemix> <input-id> <operands> [--key <key>] [--expect <revision>]
      trim <in> <out|end>; rate <2500..=40000 basis points>; end-action <stop|hold|loop>; end-trigger <frames|off>; audio-track <stream|none>; audio-policy <mute|resample>
  freemix-cli status <show.freemix>
  freemix-cli journal-recover <show.freemix>
  freemix-cli inputs <show.freemix>
  freemix-cli outputs <show.freemix>
  freemix-cli streams <show.freemix>
  freemix-cli audio-buses <show.freemix>
  freemix-cli scenes <show.freemix>
  freemix-cli asset-audit <show.freemix>
  freemix-cli settings-export <show.freemix> <settings.json>
  freemix-cli settings-import <show.freemix> <settings.json> [--apply]
  freemix-cli title-design-check <design.fmdesign>
  freemix-cli title-apply-row <show.freemix> <input-id> <row.json> [--keep-live]
  freemix-cli title-design-import <show.freemix> <design.fmdesign> --id <nonzero-input-id> --name <name> [--font <Family=path.ttf>]...
  freemix-cli audio-strip <show.freemix> <input> <gain-millidb:-96000..=24000> <balance-bp:-10000..=10000> <muted:on|off> <soloed:on|off> <follow-video:on|off> <delay-samples:0..=48000> [--pan <bp:-10000..=10000>]
  freemix-cli audio-label <show.freemix> <input> <label (empty clears)> [--key <key>] [--expect <revision>]
  freemix-cli audio-group <show.freemix> <input> <1..4|none> [--key <key>] [--expect <revision>]
  freemix-cli audio-group-name <show.freemix> <group:1..4> <name> [--key <key>] [--expect <revision>]
  freemix-cli audio-group-level <show.freemix> <group:1..4> <gain-millidb> <muted:on|off> [--key <key>] [--expect <revision>]
  freemix-cli audio-group-solo <show.freemix> <group:1..4> <soloed:on|off> [--key <key>] [--expect <revision>]
  freemix-cli audio-group-parent <show.freemix> <group:1..4> <parent:1..4|none> [--key <key>] [--expect <revision>]
  freemix-cli audio-solo-mode <show.freemix> <sip|pfl|afl> [--key <key>] [--expect <revision>]
  freemix-cli audio-monitor-device <show.freemix> <alsa-device|none> [--key <key>] [--expect <revision>]
  freemix-cli audio-monitor-level <show.freemix> <gain-millidb> <muted:on|off> [--key <key>] [--expect <revision>]
  freemix-cli audio-strip-dsp <show.freemix> <input|master> <flat|voice|podcast|music|master|chain-code> [--key <key>] [--expect <revision>]
  freemix-cli audio-plugin-chain <show.freemix> <input|master> [<plugin>[:<bypass 0|1>][:<latency>[:<state-hex>]] ...]
  freemix-cli audio-loudness-reset <show.freemix> <input|master> [--key <key>] [--expect <revision>]
  freemix-cli rename <show.freemix> <input> <name> [--key <key>] [--expect <revision>]
  freemix-cli input-reorder <show.freemix> <input> [<input>...] [--key <key>] [--expect <revision>]
  freemix-cli preview <show.freemix> <input> [--key <key>] [--expect <revision>]
  freemix-cli cut <show.freemix> [--key <key>] [--expect <revision>]
  freemix-cli fade <show.freemix> <frames> [--key <key>] [--expect <revision>]
  freemix-cli alpha-fade <show.freemix> <frames> [--key <key>] [--expect <revision>]
  freemix-cli slide <show.freemix> <frames> [--key <key>] [--expect <revision>]
  freemix-cli zoom <show.freemix> <frames> [--key <key>] [--expect <revision>]
  freemix-cli transition-kinds
  freemix-cli transition <show.freemix> <kind> <frames> [--key <key>] [--expect <revision>]
  freemix-cli transition-render-frame <show.freemix> <kind> <numerator> <denominator> <program.ppm> [--width <pixels>] [--height <pixels>]
  freemix-cli transition-favorites <show.freemix>
  freemix-cli transition-favorite-set <show.freemix> <slot:1..=4> <cut|kind|stinger-N> <frames> [--key <key>] [--expect <revision>]
  freemix-cli stinger <show.freemix> <slot:1..=8> <frames> [--key <key>] [--expect <revision>]
  freemix-cli stinger-configure <show.freemix> <slot:1..=8> <media-input> <true|false> <cut-point-frames> <muted|stinger-only|mix-with-program> <cut|fade|keep-program>
  freemix-cli stinger-remove <show.freemix> <slot:1..=8>
  freemix-cli overlay-take <show.freemix> <channel:1..=8> <source-input> [--key <key>] [--expect <revision>]
  freemix-cli overlay-update <show.freemix> <channel:1..=8> <source-input> [--key <key>] [--expect <revision>]
  freemix-cli overlay-off <show.freemix> <channel:1..=8> [--key <key>] [--expect <revision>]
  freemix-cli overlay-output <show.freemix> <channel:1..=8> <output> <true|false> [--key <key>] [--expect <revision>]
  freemix-cli overlay-transition <show.freemix> <channel:1..=8> <cut|fade|kind> <frames> [--key <key>] [--expect <revision>]
  freemix-cli overlay-appearance <show.freemix> <channel:1..=8> <full-frame|top-left|top-right|bottom-left|bottom-right> <none|thin-white|thick-white> [--key <key>] [--expect <revision>]
  freemix-cli overlay-queue <show.freemix> <channel:1..=8> <source-input> [--key <key>] [--expect <revision>]
  freemix-cli overlay-next <show.freemix> <channel:1..=8> [--key <key>] [--expect <revision>]
  freemix-cli wipe <show.freemix> <frames> [--key <key>] [--expect <revision>]
  freemix-cli tbar-start <show.freemix> <mix kind|stinger-<slot> <clip-frames>> [--key <key>] [--expect <revision>]
  freemix-cli tbar-position <show.freemix> <basis-points:0..=10000> [--key <key>] [--expect <revision>]
  freemix-cli tbar-commit <show.freemix> [--key <key>] [--expect <revision>]
  freemix-cli tbar-cancel <show.freemix> [--key <key>] [--expect <revision>]
  freemix-cli ftb <show.freemix> <live|black> <frames> [--key <key>] [--expect <revision>]
  freemix-cli ftb-output <show.freemix> <output> <affected:on|off> [--key <key>] [--expect <revision>]
  freemix-cli ftb-output-duration <show.freemix> <output> <frames:1..=3600|none> [--key <key>] [--expect <revision>]
  freemix-cli ftb-curve <show.freemix> <linear|ease> [--key <key>] [--expect <revision>]
  freemix-cli display-output-set <show.freemix> <slot:1|2> <display=index:N|name:NAME|id:ID[,source=program|preview|output:ID][,mode=current|WxH[@HZ]][,scaling=fit|fill|stretch|1:1][,hdr=off|auto]> [--key <key>] [--expect <revision>]
  freemix-cli display-output-clear <show.freemix> <slot:1|2> [--key <key>] [--expect <revision>]
  freemix-cli display-output-list <show.freemix>
  freemix-cli replay <show.freemix>
  freemix-cli replay-set <show.freemix> [--sources <input,input|none>] [--retention <10..=3600>] [--root <absolute-dir>|--default-root] [--auto-return off|<0..=300>] [--key <key>] [--expect <revision>]
      Prints what the project records for instant replay (its settings, feed,
      and effective root), or edits it: the recorded inputs in angle order
      (at most four; none turns recording off), the retention window in
      seconds, an absolute storage root or the default root, and auto return
      (a Replay input that plays its event to the end on Program hands it
      back to the last live source with a mix of that many frames, 0 a cut;
      Protocol 2.116). Absent options keep their value.
  freemix-cli reel <show.freemix>
  freemix-cli reel-set <show.freemix> --id <1..=18446744073709551615> --name <name> --clips <event>/<angle>/<in>/<out>/<speed>[/<dissolve>][,...] [--music <absolute path> [--music-level <0..=100>] [--music-duck <0..=100>]] [--key <key>] [--expect <revision>]
  freemix-cli reel-delete <show.freemix> --id <1..=18446744073709551615> [--key <key>] [--expect <revision>]
      Prints a project's highlight reels (id, name, clip count, and the
      compiled total frames when the local engine can plan them), replaces or
      creates one, or deletes one (Protocol 2.107, OR-013). --clips lists 1 to
      64 comma-separated <event>/<angle>/<trim-in>/<trim-out>/<speed>[/<dissolve>]
      records: a nonzero event (1..=4294967295) and nonzero input angle, trims
      of 0..=216000 frames, a name of 1..=64 bytes, a speed of 25, 50, 75, or
      100 percent, and the frames the clip dissolves in from the clip before
      (0..=600, absent or 0 for a cut; the first clip always cuts; Protocol
      2.111). --music plays an audio file on the server (an absolute path of
      at most 512 bytes) under the clips at --music-level percent (default
      80), ducked to --music-duck percent of that (default 25) while a
      real-time clip's audio plays, and faded over the last second; without
      --music the reel has no bed (Protocol 2.113). A project holds at most 8
      reels. The compiled plan
      needs recorded replay events, which a local project does not hold, so
      reel prints the stored clips and the plan error.
  freemix-cli live-lan <show.freemix>
  freemix-cli live-lan-set <show.freemix> [--enable|--disable] [--source program|output:<id>] [--max-viewers <1..=32>] [--pin <4-12 digits>|--clear-pin] [--key <key>] [--expect <revision>]
  freemix-cli input-add-stream <show.freemix> <nonzero-input-id> <name> <rtsp|rtp|udp|tcp|http|https|rtmp|rtmps|srt URL or .m3u8> [stream options]
  freemix-cli input-stream-set <show.freemix> <input> [--url <url>] [stream options] [--key <key>] [--expect <revision>]
  freemix-cli input-stream-status <show.freemix>
    stream options: [--transport auto|tcp|udp] [--jitter-ms <40..=5000>] [--backoff-min-ms <100..=60000>] [--backoff-max-ms <100..=60000>] [--give-up never|<1..=10000>] [--timeout-ms <500..=60000>] [--stall freeze|black] [--mode caller|listener|rendezvous] [--latency-ms <20..=8000>] [--key-length 16|24|32] [--audio mix|1-2|3-4|5-6|7-8] [--stream-id <id>] [--stream-key <key>] [--passphrase <10..=79 chars>]
    stream set options: [--clear-stream-id] [--clear-stream-key] [--clear-passphrase]; an omitted stream key or passphrase is kept
  freemix-cli input-add-capture <show.freemix> <nonzero-input-id> <name> <device-stable-key> [capture options]
  freemix-cli input-capture-set <show.freemix> <input> [capture options] [--key <key>] [--expect <revision>]
  freemix-cli input-capture-status <show.freemix>
    capture options: [--format <W>x<H>@<num>/<den>:<yuyv|nv12|mjpeg>:<p|tff|bff>|auto] [--audio none|paired|alsa:<device>] [--loss hold|black]
  freemix-cli input-add-system-audio <show.freemix> <nonzero-input-id> <name> <pulse|alsa> <device: a PulseAudio source such as <sink>.monitor or @DEFAULT_MONITOR@, or an ALSA PCM such as hw:Loopback,1>
  freemix-cli input-system-audio-status <show.freemix>
  freemix-cli input-add-image <show.freemix> <nonzero-input-id> <name> <asset://key|/absolute/picture> [--fit fit|fill|stretch|1:1]
  freemix-cli input-add-slideshow <show.freemix> <nonzero-input-id> <name> <--folder <asset://folder|/absolute/folder> | --picture <asset://key|/absolute/picture>[|caption]...> [slideshow options]
    slideshow options: [--fit fit|fill|stretch|1:1] [--slide-frames <1..=1000000>] [--fade-frames <1..=3600>|--cut] [--ken-burns] [--random <seed>] [--no-loop] [--manual]; host pictures and folders are copied into the project
  freemix-cli input-add-screen-capture <show.freemix> <nonzero-input-id> <name> <screen <index>|window <app> <title>> [--match exact|contains|any] [--window-id <id>] [--crop <left>,<top>,<right>,<bottom>] [--cursor on|off] [--fps <1-60>] [--stall freeze|black] [--audio none|system|app]
  freemix-cli input-screen-capture-status <show.freemix>
  freemix-cli remote-status <127.0.0.1:port>
  freemix-cli remote-diagnostics <127.0.0.1:port>
  freemix-cli remote-alert <127.0.0.1:port> <rule> acknowledge|silence <1..1440 minutes> | unsilence
  freemix-cli remote-alert-rules <127.0.0.1:port>
  freemix-cli remote-alert-rule <127.0.0.1:port> <rule> [--on|--off] [--fires-at <value>] [--clears-at <value>] [--hold <0..=3600 seconds>] | --defaults [--key <key>] [--expect <revision>]
      Prints, or changes for the show, each alert rule's setting (Protocol 2.112):
      whether it is evaluated, the reading it fires and clears at (a percentage,
      free GiB, events a second, or a count), and its hold. One AlertRule line
      per rule; a change is one undoable project edit.
    also prints one SourceSync line per live source clock: drift, ratio, buffer, and recovery counts
    then a Health line (CPU and GPU-proxy utilization) and one Alert and AlertNotification line per firing alert and recent notification
  freemix-cli remote-audio-strip <127.0.0.1:port> <input> <gain-millidb:-96000..=24000> <balance-bp:-10000..=10000> <muted:on|off> <soloed:on|off> <follow-video:on|off> <delay-samples:0..=48000> [--pan <bp>] [--key <key>] [--expect <revision>]
  freemix-cli remote-audio-label|remote-audio-group|remote-audio-group-name|remote-audio-group-level|remote-audio-group-solo|remote-audio-group-parent|remote-audio-solo-mode|remote-audio-monitor-device|remote-audio-monitor-level|remote-audio-strip-dsp|remote-audio-loudness-reset <127.0.0.1:port> <arguments as above> [--key <key>] [--expect <revision>]
  freemix-cli remote-rename <127.0.0.1:port> <input> <name> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-add <127.0.0.1:port> solid <input> <name> <red:0..=255> <green:0..=255> <blue:0..=255> <alpha:0..=255> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-add <127.0.0.1:port> bars <input> <name> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-add-stream <127.0.0.1:port> <input> <name> <url> [stream options] [--key <key>] [--expect <revision>]
  freemix-cli remote-input-stream-set <127.0.0.1:port> <input> [--url <url>] [stream options] [--key <key>] [--expect <revision>]
  freemix-cli remote-input-stream-status <127.0.0.1:port>
  freemix-cli remote-input-add-capture <127.0.0.1:port> <input> <name> <device-stable-key> [capture options] [--key <key>] [--expect <revision>]
  freemix-cli remote-input-capture-set <127.0.0.1:port> <input> [capture options] [--key <key>] [--expect <revision>]
  freemix-cli remote-input-capture-status <127.0.0.1:port>
  freemix-cli remote-capture-devices <127.0.0.1:port>
  freemix-cli remote-input-add-system-audio <127.0.0.1:port> <input> <name> <pulse|alsa> <device> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-system-audio-status <127.0.0.1:port>
  freemix-cli remote-system-audio-devices <127.0.0.1:port>
  freemix-cli remote-audio-output-devices <127.0.0.1:port>
  freemix-cli remote-input-add-image <127.0.0.1:port> <input> <name> <asset://key|/absolute/picture on the server> [--fit fit|fill|stretch|1:1] [--key <key>] [--expect <revision>]
  freemix-cli remote-input-add-slideshow <127.0.0.1:port> <input> <name> <--folder <folder> | --picture <picture>[|caption]...> [slideshow options] [--key <key>] [--expect <revision>]
  freemix-cli remote-page-navigate <127.0.0.1:port> <input> <next|previous|first|last|goto <page from 1>> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-add-screen-capture <127.0.0.1:port> <input> <name> <screen <index>|window <app> <title>|picked-window> [capture options] [--key <key>] [--expect <revision>]
  freemix-cli remote-input-screen-capture-status <127.0.0.1:port>
  freemix-cli remote-screen-capture-targets <127.0.0.1:port>
  freemix-cli remote-screen-capture-pick <127.0.0.1:port> <input>
  freemix-cli remote-input-remove <127.0.0.1:port> <input> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-duplicate <127.0.0.1:port> <source-input> <new-input> [<name>] [--key <key>] [--expect <revision>]
  freemix-cli remote-input-replace <127.0.0.1:port> <input> <solid:RRGGBBAA[:hz]|bars[:hz]|media:<file|asset://key>|device:<key>|network:<endpoint>|system-audio:<pulse|alsa>:<device>> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-categories <127.0.0.1:port> [<id>=<name>...] [--key <key>] [--expect <revision>]
  freemix-cli remote-input-category <127.0.0.1:port> <input> <category-id|none> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-color <127.0.0.1:port> <input> <colour|none> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-pause <127.0.0.1:port> <input> <on|off> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-close <127.0.0.1:port> <input> <on|off> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-import-batch <127.0.0.1:port> <first-new-input> <media-file|asset://key>... [--key <key>] [--expect <revision>]
  freemix-cli remote-input-reorder <127.0.0.1:port> <input> [<input>...] [--key <key>] [--expect <revision>]
  freemix-cli remote-preview <127.0.0.1:port> <input> [--key <key>] [--expect <revision>]
  freemix-cli remote-cut <127.0.0.1:port> [--key <key>] [--expect <revision>]
  freemix-cli remote-playlist <127.0.0.1:port> set <off|all|one> <-|id:input:frames:cut|fade|alpha_fade|wipe|slide|zoom:transition-frames:0|1[,...]> [--key <key>] [--expect <revision>]
  freemix-cli remote-playlist <127.0.0.1:port> start [now|after:<frames>|at:<frame>] [--from <entry>] [--key <key>] [--expect <revision>]
  freemix-cli remote-playlist <127.0.0.1:port> <stop|pause|resume|next|previous> [--key <key>] [--expect <revision>]
  freemix-cli remote-playlist <127.0.0.1:port> jump <entry> [--key <key>] [--expect <revision>]
  freemix-cli cue-lists <project.freemix>
  freemix-cli remote-cue-lists <127.0.0.1:port>
  freemix-cli cue-list-set <project.freemix> <list> <name> <-|id:name:0|1:action[,action...][;...]> [--key <key>] [--expect <revision>]
  freemix-cli remote-cue-list-set <127.0.0.1:port> <list> <name> <-|id:name:0|1:action[,action...][;...]> [--key <key>] [--expect <revision>]
    actions: preview.<input> take.<input|->.<cut|fade|...>.<frames> overlay_on.<channel>.<input> overlay_off.<channel> title_on.<input>.<channel> title_text.<input>.<field>.<text> title_number.<input>.<field>.<set|add>.<value> title_off.<input> fade_to_black.<0|1>.<frames> playlist_start playlist_stop audio_gain.<input>.<millidB> audio_mute.<input>.<0|1> record.<1|2>.<0|1> stream.<target>.<0|1> wait.<frames> delay.<millis> run_script.<script>
  freemix-cli cue-list-remove <project.freemix> <list> [--key <key>] [--expect <revision>]
  freemix-cli remote-cue-list-remove <127.0.0.1:port> <list> [--key <key>] [--expect <revision>]
  freemix-cli go <project.freemix> <list> [cue] [--selected <input>] [--key <key>] [--expect <revision>]
  freemix-cli remote-go <127.0.0.1:port> <list> [cue] [--selected <input>] [--key <key>] [--expect <revision>]
  freemix-cli cue-standby <project.freemix> <list> <cue> [--key <key>] [--expect <revision>]
  freemix-cli remote-cue-standby <127.0.0.1:port> <list> <cue> [--key <key>] [--expect <revision>]
  freemix-cli <cue-back|cue-skip|cue-stop|cue-reset> <project.freemix> <list> [--key <key>] [--expect <revision>]
  freemix-cli <remote-cue-back|remote-cue-skip|remote-cue-stop|remote-cue-reset> <127.0.0.1:port> <list> [--key <key>] [--expect <revision>]
  freemix-cli cue-arm <project.freemix> <list> <milliseconds> [cue] [--selected <input>] [--key <key>] [--expect <revision>]
  freemix-cli remote-cue-arm <127.0.0.1:port> <list> <milliseconds> [cue] [--selected <input>] [--key <key>] [--expect <revision>]
      An action's input may be a role resolved when the cue fires: preview,
      program, or selected (RC-004). --selected names the input the
      selected role means; the CLI has no selection of its own, so without it a
      GO or arm of a cue acting on the selected input is refused.
  freemix-cli cue-disarm <project.freemix> <list> [--key <key>] [--expect <revision>]
  freemix-cli remote-cue-disarm <127.0.0.1:port> <list> [--key <key>] [--expect <revision>]
  freemix-cli scripts <project.freemix>
  freemix-cli remote-scripts <127.0.0.1:port>
  freemix-cli script-set <project.freemix> <script> <name> <source-file|-> [--key <key>] [--expect <revision>]
  freemix-cli remote-script-set <127.0.0.1:port> <script> <name> <source-file|-> [--key <key>] [--expect <revision>]
  freemix-cli script-remove <project.freemix> <script> [--key <key>] [--expect <revision>]
  freemix-cli remote-script-remove <127.0.0.1:port> <script> [--key <key>] [--expect <revision>]
  freemix-cli script-run <project.freemix> <script|name> [--key <key>] [--expect <revision>]
  freemix-cli remote-script-run <127.0.0.1:port> <script|name> [--key <key>] [--expect <revision>]
  freemix-cli script-stop <project.freemix> [--run <run>] [--key <key>] [--expect <revision>]
  freemix-cli remote-script-stop <127.0.0.1:port> [--run <run>] [--key <key>] [--expect <revision>]
  freemix-cli remote-undo <127.0.0.1:port> [--edit <revision>] [--key <key>] [--expect <revision>]
  freemix-cli remote-redo <127.0.0.1:port> [--edit <revision>] [--key <key>] [--expect <revision>]
  freemix-cli remote-lock <127.0.0.1:port> <on|off> [--input <input>] [--key <key>] [--expect <revision>]
  freemix-cli remote-fade <127.0.0.1:port> <frames> [--key <key>] [--expect <revision>]
  freemix-cli remote-alpha-fade <127.0.0.1:port> <frames> [--key <key>] [--expect <revision>]
  freemix-cli remote-slide <127.0.0.1:port> <frames> [--key <key>] [--expect <revision>]
  freemix-cli remote-zoom <127.0.0.1:port> <frames> [--key <key>] [--expect <revision>]
  freemix-cli remote-transition <127.0.0.1:port> <kind> <frames> [--key <key>] [--expect <revision>]
  freemix-cli remote-transition-favorite-set <127.0.0.1:port> <slot:1..=4> <cut|kind|stinger-N> <frames> [--key <key>] [--expect <revision>]
  freemix-cli remote-input-media-<play|pause|stop|complete|seek|trim|rate|end-action|end-trigger|audio-track|audio-policy> <127.0.0.1:port> <input-id> <operands> [--key <key>] [--expect <revision>]
      seek <frame|N/Ds|Ns>; complete declares a growing file finished
  freemix-cli remote-stinger <127.0.0.1:port> <slot:1..=8> <frames> [--key <key>] [--expect <revision>]
  freemix-cli remote-stinger-configure <127.0.0.1:port> <slot:1..=8> <media-input> <true|false> <cut-point-frames> <muted|stinger-only|mix-with-program> <cut|fade|keep-program> [--key <key>] [--expect <revision>]
  freemix-cli remote-stinger-remove <127.0.0.1:port> <slot:1..=8> [--key <key>] [--expect <revision>]
  freemix-cli remote-overlay-take <127.0.0.1:port> <channel:1..=8> <source-input> [--key <key>] [--expect <revision>]
  freemix-cli remote-overlay-update <127.0.0.1:port> <channel:1..=8> <source-input> [--key <key>] [--expect <revision>]
  freemix-cli remote-overlay-off <127.0.0.1:port> <channel:1..=8> [--key <key>] [--expect <revision>]
  freemix-cli remote-overlay-output <127.0.0.1:port> <channel:1..=8> <output> <true|false> [--key <key>] [--expect <revision>]
  freemix-cli remote-overlay-transition <127.0.0.1:port> <channel:1..=8> <cut|fade|kind> <frames> [--key <key>] [--expect <revision>]
  freemix-cli remote-overlay-appearance <127.0.0.1:port> <channel:1..=8> <full-frame|top-left|top-right|bottom-left|bottom-right> <none|thin-white|thick-white> [--key <key>] [--expect <revision>]
  freemix-cli remote-overlay-queue <127.0.0.1:port> <channel:1..=8> <source-input> [--key <key>] [--expect <revision>]
  freemix-cli remote-overlay-next <127.0.0.1:port> <channel:1..=8> [--key <key>] [--expect <revision>]
  freemix-cli remote-wipe <127.0.0.1:port> <frames> [--key <key>] [--expect <revision>]
  freemix-cli remote-tbar-start <127.0.0.1:port> <mix kind|stinger-<slot> <clip-frames>> [--key <key>] [--expect <revision>]
  freemix-cli remote-tbar-position <127.0.0.1:port> <basis-points:0..=10000> [--key <key>] [--expect <revision>]
  freemix-cli remote-tbar-commit <127.0.0.1:port> [--key <key>] [--expect <revision>]
  freemix-cli remote-tbar-cancel <127.0.0.1:port> [--key <key>] [--expect <revision>]
  freemix-cli remote-ftb <127.0.0.1:port> <live|black> <frames> [--key <key>] [--expect <revision>]
  freemix-cli remote-ftb-output <127.0.0.1:port> <output> <affected:on|off> [--key <key>] [--expect <revision>]
  freemix-cli remote-ftb-output-duration <127.0.0.1:port> <output> <frames:1..=3600|none> [--key <key>] [--expect <revision>]
  freemix-cli remote-ftb-curve <127.0.0.1:port> <linear|ease> [--key <key>] [--expect <revision>]
  freemix-cli remote-display-output-set <127.0.0.1:port> <slot:1|2> <spec as above> [--key <key>] [--expect <revision>]
  freemix-cli remote-display-output-clear <127.0.0.1:port> <slot:1|2> [--key <key>] [--expect <revision>]
  freemix-cli remote-display-output-list <127.0.0.1:port>
  freemix-cli remote-display-inventory <127.0.0.1:port>
  freemix-cli remote-replay <127.0.0.1:port>
  freemix-cli remote-replay-set <127.0.0.1:port> [replay-set options]
  freemix-cli remote-replay-do <127.0.0.1:port> <mark-in|mark-out|capture [--name <name>]|last <5|10|20>|rename <event> <name>|delete <event>|cue <input> <event> [--angle <input>]|angle <input> <angle>|play <input> [25|50|75|100]|pause <input>|cue-in <input>|step <input> <frames>|link <input> [<linked>]|playlist <input> [<event>[/<angle>][@<speed>] ...]|playlist-next <input>|playlist-prev <input>> [--key <key>]
  freemix-cli remote-replay-multiview <127.0.0.1:port> <input> --output <file.png> [--width <16..=640>] [--height <16..=360>]
  freemix-cli remote-audio-plugin-chain <127.0.0.1:port> <input> [<plugin>[:<bypass 0|1>][:<latency>[:<state-hex>]] ...]
      Prints the daemon's replay recording, events, and Replay inputs, edits
      what it records, or sends one live replay command: mark in or out,
      capture the marks or the last 5, 10, or 20 seconds, rename or delete an
      event, cue an event angle on a Replay input, switch angle, play at 25,
      50, 75, or 100 percent, pause, jump back to the cue-in point, or step
      (jog) a cued input -600 to 600 frames, paused (Protocol 2.114).
  freemix-cli remote-reel <127.0.0.1:port>
  freemix-cli remote-reel-set <127.0.0.1:port> [reel-set options]
  freemix-cli remote-reel-delete <127.0.0.1:port> --id <1..=18446744073709551615> [--key <key>] [--expect <revision>]
  freemix-cli remote-reel-export <127.0.0.1:port> --job <1..=18446744073709551615> --reel <1..=18446744073709551615> --destination <absolute-path> [--key <key>] [--expect <revision>]
  freemix-cli remote-reel-cancel <127.0.0.1:port> --job <1..=18446744073709551615> [--key <key>] [--expect <revision>]
  freemix-cli remote-reel-forget <127.0.0.1:port> --job <1..=18446744073709551615> [--key <key>] [--expect <revision>]
      Prints the daemon's highlight reels and export jobs, replaces or creates
      a reel, deletes one, or controls an export (Protocol 2.107, OR-013).
      remote-reel-export sends reel_export and polls the lossy reel status
      until the job is completed, failed, or cancelled within an hour, then
      prints one ReelExport(job=..., state=..., path=..., bytes=...) line;
      a failed job appends its failure text. --destination must be an
      absolute path of at most 1024 bytes.
  freemix-cli remote-live-lan <127.0.0.1:port>
  freemix-cli remote-live-lan-set <127.0.0.1:port> [live-lan-set options]
  freemix-cli remote-stream-start <127.0.0.1:port> <existing-stream-id> [--key <key>] [--expect <revision>]
  freemix-cli remote-stream-stop <127.0.0.1:port> <existing-stream-id> [--key <key>] [--expect <revision>]
  freemix-cli remote-stream-retry <127.0.0.1:port> <running-stream-id> [--key <key>] [--expect <revision>]
  freemix-cli remote-stream-start-all <127.0.0.1:port> [--key <key>] [--expect <revision>]
  freemix-cli remote-stream-stop-all <127.0.0.1:port> [--key <key>] [--expect <revision>]
  freemix-cli remote-stream-set <127.0.0.1:port> <nonzero-stream-id> [--name <name>] [--protocol rtmp|rtmps|srt|hls|omt] [--endpoint <host/app|host:port|/abs/dir or full URL>] [--backup-endpoint <endpoint> | --clear-backup-endpoint] [--output <id>] [--bitrate-kbps <1000..=100000>] [--startup stopped|reconcile] [--reconnect <initial_ms>:<max_ms>:<attempts|none>] [--stream-key <key> | --clear-stream-key] [--srt-latency-ms <20..=8000>] [--srt-passphrase <passphrase> | --clear-srt-passphrase] [--key <key>] [--expect <revision>]
      Adds or reconfigures a destination of a stopped stream. A new one needs
      --name, --protocol (or a full endpoint URL), --endpoint and --output;
      for an existing one every omitted option keeps the daemon's value and
      an omitted stream key or SRT passphrase is kept. Neither is printed.
  freemix-cli remote-stream-remove <127.0.0.1:port> <stopped-stream-id> [--key <key>] [--expect <revision>]
  freemix-cli remote-streams <127.0.0.1:port>
      Lists each destination's configuration (URL without key, key=yes|no)
      and its latest status: state, kbps, dropped, reconnects, uptime, error.
  freemix-cli remote-record-start <127.0.0.1:port> [--recorder <1|2>] [--key <key>] [--expect <revision>]
  freemix-cli remote-record-stop <127.0.0.1:port> [--recorder <1|2>] [--key <key>] [--expect <revision>]
  freemix-cli render <show.freemix> <output.ppm> [--width <px>] [--height <px>]
  freemix-cli demo <show.freemix> [output.ppm]
  freemix-cli project-export-bundle <source.freemix> <destination.freemix>
  freemix-cli project-from-template <template.freemix> <destination.freemix> --name <name>
  freemix-cli project-append <target.freemix> <source.freemix>
  freemix-cli project-import-media <show.freemix> <media-file|pdf|pptx|odp> [--input <new-nonzero-input-id> [--name <name>]]
  freemix-cli project-relink <show.freemix> [--search <dir>]... [--apply]
  freemix-cli project-autosave <show.freemix> [--keep <1..=64>]
  freemix-cli project-recover <show.freemix> [--restore]
  freemix-cli recent-projects <recent.json>
  freemix-cli recent-projects-add <recent.json> <show.freemix> [--capacity <1..=100>]
  freemix-cli recent-projects-remove <recent.json> <show.freemix>
  freemix-cli recent-projects-prune <recent.json>
      Bundle commands never overwrite an existing destination. Export and
      template copy only project.json and referenced media (no journal or
      autosaves). project-relink previews by default; --apply writes it.
      project-recover reports the newest usable autosave; --restore replaces
      project.json with it. The recent-projects file is whatever path you pass;
      freemix-cli keeps no hidden state."
    );
}

fn engine_manual_kind(kind: ManualTransitionKind) -> EngineManualTransitionKind {
    fm_control::engine_manual_kind(kind)
}

const fn switcher_overlay_transition(kind: CliOverlayTransition) -> OverlayTransitionKind {
    fm_control::domain_overlay_transition(kind)
}

const fn protocol_overlay_transition(
    kind: CliOverlayTransition,
) -> fm_protocol::OverlayTransitionKind {
    kind
}

/// The project favourite a CLI assignment names.
fn model_favorite(
    effect: fm_protocol::TransitionFavoriteEffect,
    frames: u32,
) -> AppResult<fm_model::TransitionFavorite> {
    fm_model::TransitionFavorite::new(fm_control::domain_effect(effect), frames)
        .map_err(|error| AppFailure(error.to_string()).into())
}

/// Prints `transition-kinds`: one `mix` line per automatic mix and one
/// `overlay` line per overlay channel transition, by CLI token.
fn print_transition_kinds() {
    for kind in fm_protocol::AutoTransitionKind::ALL {
        println!("mix\t{}", crate::args::cli_token(kind.token()));
    }
    for kind in fm_protocol::OverlayTransitionKind::ALL {
        println!("overlay\t{}", crate::args::cli_token(kind.token()));
    }
}

/// Prints `transition-favorites`: `slot\teffect\tframes` per favourite.
fn print_transition_favorites(project: &Project) {
    for (index, favorite) in project.transition_favorites().slots().iter().enumerate() {
        let favorite = fm_control::wire_favorite(*favorite);
        println!(
            "{}\t{}\t{}",
            index + 1,
            crate::args::cli_token(&favorite.effect.token()),
            favorite.duration_frames
        );
    }
}

/// Renders Program mixing to Preview with `kind` at exactly
/// `numerator/denominator`, through the simulated pipeline's reference
/// shapes (the same CPU reference the native compositor matches).
fn render_transition_frame(
    path: &Path,
    kind: fm_protocol::AutoTransitionKind,
    (numerator, denominator): (u32, u32),
    output: &Path,
    (width, height): (u32, u32),
) -> AppResult<()> {
    let project = inspect_engine(path)?;
    let realized = project.engine.realized_switcher();
    let (from, to) = (realized.program(), realized.preview());
    if [from, to]
        .into_iter()
        .any(|input| is_scene_input(&project.project, input))
    {
        return Err(AppFailure(
            "transition-render-frame does not support Scene input endpoints".into(),
        )
        .into());
    }
    if denominator == 0 || numerator > denominator {
        return Err(AppFailure(format!(
            "transition progress {numerator}/{denominator} must be within 0 through 1"
        ))
        .into());
    }
    let pipeline = simulated_pipeline(&project.project, width, height)?;
    let frame = pipeline.render(
        project.engine.frame_cursor().get(),
        fm_switcher::ProgramFrame {
            primary: from,
            secondary: Some(to),
            transition_kind: Some(fm_control::domain_transition_kind(kind)),
            mix_numerator: numerator,
            mix_denominator: denominator,
            mix_start_numerator: numerator,
            mix_end_numerator: numerator,
        },
    )?;
    write_ppm_atomic(output, |file| write_ppm(&frame, file))?;
    println!(
        "rendered {}x{} {} {numerator}/{denominator} from input {from} to input {to} to {}",
        width,
        height,
        crate::args::cli_token(kind.token()),
        output.display()
    );
    Ok(())
}

const fn switcher_overlay_position(kind: CliOverlayPosition) -> OverlayPositionPreset {
    match kind {
        CliOverlayPosition::FullFrame => OverlayPositionPreset::FullFrame,
        CliOverlayPosition::TopLeft => OverlayPositionPreset::TopLeft,
        CliOverlayPosition::TopRight => OverlayPositionPreset::TopRight,
        CliOverlayPosition::BottomLeft => OverlayPositionPreset::BottomLeft,
        CliOverlayPosition::BottomRight => OverlayPositionPreset::BottomRight,
    }
}

const fn switcher_overlay_border(kind: CliOverlayBorder) -> OverlayBorderPreset {
    match kind {
        CliOverlayBorder::None => OverlayBorderPreset::None,
        CliOverlayBorder::ThinWhite => OverlayBorderPreset::ThinWhite,
        CliOverlayBorder::ThickWhite => OverlayBorderPreset::ThickWhite,
    }
}

const fn protocol_overlay_position(kind: CliOverlayPosition) -> fm_protocol::OverlayPositionPreset {
    match kind {
        CliOverlayPosition::FullFrame => fm_protocol::OverlayPositionPreset::FullFrame,
        CliOverlayPosition::TopLeft => fm_protocol::OverlayPositionPreset::TopLeft,
        CliOverlayPosition::TopRight => fm_protocol::OverlayPositionPreset::TopRight,
        CliOverlayPosition::BottomLeft => fm_protocol::OverlayPositionPreset::BottomLeft,
        CliOverlayPosition::BottomRight => fm_protocol::OverlayPositionPreset::BottomRight,
    }
}

const fn protocol_overlay_border(kind: CliOverlayBorder) -> fm_protocol::OverlayBorderPreset {
    match kind {
        CliOverlayBorder::None => fm_protocol::OverlayBorderPreset::None,
        CliOverlayBorder::ThinWhite => fm_protocol::OverlayBorderPreset::ThinWhite,
        CliOverlayBorder::ThickWhite => fm_protocol::OverlayBorderPreset::ThickWhite,
    }
}

const fn protocol_stinger_audio_policy(
    policy: CliStingerAudioPolicy,
) -> fm_protocol::StingerAudioPolicy {
    match policy {
        CliStingerAudioPolicy::Muted => fm_protocol::StingerAudioPolicy::Muted,
        CliStingerAudioPolicy::StingerOnly => fm_protocol::StingerAudioPolicy::StingerOnly,
        CliStingerAudioPolicy::MixWithProgram => fm_protocol::StingerAudioPolicy::MixWithProgram,
    }
}

const fn protocol_stinger_fallback(
    fallback: CliStingerFallback,
) -> fm_protocol::StingerMissingMediaFallback {
    match fallback {
        CliStingerFallback::Cut => fm_protocol::StingerMissingMediaFallback::Cut,
        CliStingerFallback::Fade => fm_protocol::StingerMissingMediaFallback::Fade,
        CliStingerFallback::KeepProgram => fm_protocol::StingerMissingMediaFallback::KeepProgram,
    }
}

const fn overlay_position_name(position: OverlayPositionPreset) -> &'static str {
    match position {
        OverlayPositionPreset::FullFrame => "full-frame",
        OverlayPositionPreset::TopLeft => "top-left",
        OverlayPositionPreset::TopRight => "top-right",
        OverlayPositionPreset::BottomLeft => "bottom-left",
        OverlayPositionPreset::BottomRight => "bottom-right",
    }
}

const fn overlay_border_name(border: OverlayBorderPreset) -> &'static str {
    match border {
        OverlayBorderPreset::None => "none",
        OverlayBorderPreset::ThinWhite => "thin-white",
        OverlayBorderPreset::ThickWhite => "thick-white",
    }
}

fn engine_t_bar_command(action: TBarAction) -> EngineCommand {
    match action {
        TBarAction::Start(kind) => EngineCommand::StartManualTransition {
            kind: engine_manual_kind(kind),
        },
        TBarAction::SetPosition(position) => EngineCommand::SetManualTransitionPosition {
            position: EngineManualTransitionPosition::new(position)
                .expect("CLI parser bounds manual positions"),
        },
        TBarAction::Commit => EngineCommand::CommitManualTransition,
        TBarAction::Cancel => EngineCommand::CancelManualTransition,
    }
}

const fn model_stinger_audio_policy(policy: CliStingerAudioPolicy) -> ModelStingerAudioPolicy {
    match policy {
        CliStingerAudioPolicy::Muted => ModelStingerAudioPolicy::Muted,
        CliStingerAudioPolicy::StingerOnly => ModelStingerAudioPolicy::StingerOnly,
        CliStingerAudioPolicy::MixWithProgram => ModelStingerAudioPolicy::MixWithProgram,
    }
}

const fn model_stinger_fallback(fallback: CliStingerFallback) -> StingerMissingMediaFallback {
    match fallback {
        CliStingerFallback::Cut => StingerMissingMediaFallback::Cut,
        CliStingerFallback::Fade => StingerMissingMediaFallback::Fade,
        CliStingerFallback::KeepProgram => StingerMissingMediaFallback::KeepProgram,
    }
}

fn protocol_t_bar_payload(action: TBarAction) -> fm_protocol::CommandPayload {
    match action {
        TBarAction::Start(kind) => fm_protocol::CommandPayload::StartManualTransition { kind },
        TBarAction::SetPosition(position) => {
            fm_protocol::CommandPayload::SetManualTransitionPosition {
                position: fm_protocol::ManualTransitionPosition::new(position)
                    .expect("CLI parser bounds manual positions"),
            }
        }
        TBarAction::Commit => fm_protocol::CommandPayload::CommitManualTransition,
        TBarAction::Cancel => fm_protocol::CommandPayload::CancelManualTransition,
    }
}

fn clock_domain() -> ClockDomainId {
    ClockDomainId::new(NonZeroU128::new(1).expect("one is nonzero"))
}

pub(crate) fn generate_project_id() -> AppResult<ProjectId> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let sequence = u128::from(PROJECT_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed));
    let process = u128::from(std::process::id());
    let value = timestamp ^ (sequence << 64) ^ (process << 96);
    Ok(ProjectId::new(NonZeroU128::new(value | 1).expect(
        "setting the low bit makes the project ID nonzero",
    )))
}

pub(crate) fn input_id(value: u128) -> AppResult<InputId> {
    NonZeroU128::new(value)
        .map(InputId::new)
        .ok_or_else(|| AppFailure("input ID must be nonzero".into()).into())
}

fn scene_id(value: u128) -> AppResult<SceneId> {
    NonZeroU128::new(value)
        .map(SceneId::new)
        .ok_or_else(|| AppFailure("scene ID must be nonzero".into()).into())
}

fn output_id(value: u128) -> AppResult<OutputId> {
    NonZeroU128::new(value)
        .map(OutputId::new)
        .ok_or_else(|| AppFailure("output ID must be nonzero".into()).into())
}

fn bus_id(value: u128) -> AppResult<BusId> {
    NonZeroU128::new(value)
        .map(BusId::new)
        .ok_or_else(|| AppFailure("bus ID must be nonzero".into()).into())
}

fn stream_target_id(value: u128) -> AppResult<StreamTargetId> {
    NonZeroU128::new(value)
        .map(StreamTargetId::new)
        .ok_or_else(|| AppFailure("stream destination ID must be nonzero".into()).into())
}

fn switcher_stream_target(target: StreamTargetId) -> fm_switcher::StreamTargetId {
    fm_switcher::StreamTargetId::from_non_zero(target.get())
}

fn wire_stream_target(value: u128) -> AppResult<fm_protocol::WireStreamTargetId> {
    Ok(fm_protocol::WireStreamTargetId::new(
        stream_target_id(value)?.get(),
    ))
}

fn required_routing(value: Option<InputId>, field: &'static str) -> AppResult<InputId> {
    value.ok_or_else(|| AppFailure(format!("project is missing {field} routing")).into())
}

fn required_main_mix(project: &Project) -> AppResult<MainMix> {
    project
        .main_mix()
        .ok_or_else(|| AppFailure("project is missing canonical main-mix routing".into()).into())
}

fn source_pattern(input: &Input) -> AppResult<SourcePattern> {
    let InputKind::Simulated(simulated) = &input.kind else {
        return Err(AppFailure(format!(
            "input {} ({:?}) is not simulated; render supports only simulated inputs",
            input.id, input.name
        ))
        .into());
    };
    Ok(match simulated.video {
        SimulatedVideo::Bars => SourcePattern::Bars,
        SimulatedVideo::Solid(color) => {
            SourcePattern::Solid(Rgba8::new(color.red, color.green, color.blue, color.alpha))
        }
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExecuteResult {
    replayed: bool,
    rejection: Option<AppFailure>,
    /// An accepted script run's script, frame, actions, and instructions.
    script_ran: Option<(u64, u64, u32, u32)>,
    /// Every script step an accepted cue operation or advance ran, in
    /// order, with its frame (Protocol 2.61).
    cue_scripts: Vec<CueScriptRan>,
}

/// A script step a cue ran: its frame and what the run did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CueScriptRan {
    frame: u64,
    run: fm_engine::CueScriptRun,
}

impl CueScriptRan {
    fn print(self) {
        remote::print_script_ran(
            self.run.script,
            self.frame,
            self.run.actions,
            self.run.instructions,
        );
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AppFailure(pub(crate) String);

impl core::fmt::Display for AppFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Error for AppFailure {}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_persistence::MutationBatch;

    #[test]
    fn persisted_receipts_keep_only_the_most_recent_bounded_window() {
        let root = std::env::temp_dir().join(format!(
            "freemix-cli-receipt-window-{}-{}",
            std::process::id(),
            PPM_OUTPUT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("receipts.freemix");
        let mut project = default_project("Receipts".into()).unwrap();
        let commands = MAX_PERSISTED_RECEIPTS + 5;
        for index in 0..commands {
            reject_unexpected(
                execute(
                    &mut project.engine,
                    EngineCommand::Cut,
                    1,
                    Some(format!("cut-{index:05}")),
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        }
        // A rejection at the newest revision is more recent than the accepted
        // command that produced that revision, so it must be retained too.
        let rejected = execute(
            &mut project.engine,
            EngineCommand::Cut,
            1,
            Some("stale".into()),
            Some(0),
        )
        .unwrap();
        assert!(rejected.rejection.is_some());
        save_engine(&path, &project).unwrap();

        let stored = ProjectStore::new(&path).unwrap().load().unwrap();
        let keys = stored
            .idempotency_receipts()
            .iter()
            .map(IdempotencyReceipt::key)
            .collect::<Vec<_>>();
        assert_eq!(keys.len(), MAX_PERSISTED_RECEIPTS);
        assert!(keys.contains(&"stale"));
        assert!(keys.contains(&format!("cut-{:05}", commands - 1).as_str()));
        assert!(!keys.contains(&format!("cut-{:05}", commands - MAX_PERSISTED_RECEIPTS).as_str()));
        assert!(
            keys.contains(&format!("cut-{:05}", commands - MAX_PERSISTED_RECEIPTS + 1).as_str())
        );
        assert_eq!(stored.position().revision, commands as u64);

        // The pruned bundle restores, replays a retained key, and accepts new work.
        let mut restored = load_engine(&path).unwrap();
        let replay = execute(
            &mut restored.engine,
            EngineCommand::Cut,
            1,
            Some(format!("cut-{:05}", commands - 1)),
            None,
        )
        .unwrap();
        assert!(replay.replayed);
        reject_unexpected(
            execute(&mut restored.engine, EngineCommand::Cut, 1, None, None).unwrap(),
        )
        .unwrap();
        save_engine(&path, &restored).unwrap();
        let stored = ProjectStore::new(&path).unwrap().load().unwrap();
        assert_eq!(stored.idempotency_receipts().len(), MAX_PERSISTED_RECEIPTS);
        assert_eq!(stored.position().revision, commands as u64 + 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ppm_publication_write_failure_keeps_previous_target() {
        use std::io::Write as _;

        let root = std::env::temp_dir().join(format!(
            "freemix-cli-ppm-publication-{}-{}",
            std::process::id(),
            PPM_OUTPUT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let output = root.join("output.ppm");
        let previous = b"complete PPM";
        fs::write(&output, previous).unwrap();

        let error = write_ppm_atomic(&output, |file| {
            file.write_all(b"partial PPM")?;
            Err(io::Error::other("controlled write failure"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&output).unwrap(), previous);
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stable_rejection_codes_round_trip_and_unknown_codes_fail() {
        let stable = [
            (RejectionCode::PermissionDenied, "permission_denied"),
            (RejectionCode::DeadlineExceeded, "deadline_exceeded"),
            (RejectionCode::RevisionConflict, "revision_conflict"),
            (RejectionCode::InvalidCommand, "invalid_command"),
            (RejectionCode::NotFound, "not_found"),
            (RejectionCode::Conflict, "conflict"),
            (RejectionCode::Unavailable, "unavailable"),
            (RejectionCode::ResourceExhausted, "resource_exhausted"),
            (RejectionCode::Internal, "internal"),
        ];
        for (code, name) in stable {
            assert_eq!(persisted_rejection_code(code).unwrap(), name);
            assert_eq!(runtime_rejection_code(name).unwrap(), code);
        }

        let error = runtime_rejection_code("future_code").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown rejection code `future_code`")
        );
    }

    #[test]
    fn local_load_rejects_unapplied_batch_without_mutating_bundle() {
        let root = std::env::temp_dir().join(format!(
            "freemix-cli-journal-{}-{}",
            std::process::id(),
            PROJECT_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("show.freemix");
        save_engine(&path, &default_project("Journal".into()).unwrap()).unwrap();
        let store = ProjectStore::new(&path).unwrap();
        store
            .append_batch(&MutationBatch::new(1, 0, 1, b"unapplied".to_vec()))
            .unwrap();
        let manifest = fs::read(path.join("project.json")).unwrap();

        let error = load_stored_project(&path).unwrap_err();
        assert_eq!(
            error.to_string(),
            "project has unapplied journal batches that freemix-cli cannot safely interpret"
        );
        assert_eq!(fs::read(path.join("project.json")).unwrap(), manifest);
        let scan = store.scan_journal().unwrap();
        assert_eq!(scan.batches().len(), 1);
        assert_eq!(scan.batches()[0].payload(), b"unapplied");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_stinger_only_marks_requested_preload_ready() {
        let program = InputId::new(NonZeroU128::new(1).unwrap());
        let preview = InputId::new(NonZeroU128::new(2).unwrap());
        let inputs = vec![program, preview];
        let named_inputs = inputs
            .iter()
            .copied()
            .map(|input| (input, format!("Input {input}")))
            .collect();
        let mut show = ShowState::new("restore", named_inputs, program, preview).unwrap();
        let mut realized = SwitcherState::new(inputs, program, preview).unwrap();
        let slot = fm_model::StingerSlotNumber::new(1).unwrap();
        let config = |preload| {
            StingerConfig::new(
                slot,
                preview,
                preload,
                1,
                ModelStingerAudioPolicy::Muted,
                StingerMissingMediaFallback::KeepProgram,
            )
        };

        restore_stinger(&mut show, &mut realized, config(false)).unwrap();
        let switcher_slot = StingerSlotId::new(1).unwrap();
        assert_eq!(
            show.desired_switcher()
                .stinger(switcher_slot)
                .preload_state(),
            fm_switcher::StingerPreloadState::NotRequested
        );
        assert_eq!(
            realized.stinger(switcher_slot).preload_state(),
            fm_switcher::StingerPreloadState::NotRequested
        );

        restore_stinger(&mut show, &mut realized, config(true)).unwrap();
        assert_eq!(
            show.desired_switcher()
                .stinger(switcher_slot)
                .preload_state(),
            fm_switcher::StingerPreloadState::Ready
        );
        assert_eq!(
            realized.stinger(switcher_slot).preload_state(),
            fm_switcher::StingerPreloadState::Ready
        );
    }
}
