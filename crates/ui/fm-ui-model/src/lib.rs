//! Replicated client-side production state.
//!
//! [`ClientModel`] is project-bound and owns no production state. It reduces
//! snapshots, ordered durable events, runtime realizations, and command results
//! into a read model for UI clients. Authoritative and optimistic state remain
//! separate so a render frame can use [`ClientModel::view`] without mistaking
//! intent for realization.

use core::{cmp::Ordering, fmt, num::NonZeroU128};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use fm_command::{CommandId, Revision};
pub use fm_protocol::EditHistoryStatus;
use fm_protocol::{
    AnimationPresetChange, AnimationPresetStateMessage, GradeChange, GradeStateMessage,
    MediaStateMessage, MediaStatus, SceneAnimationStateMessage, SceneLayoutStateMessage,
    TitleAnimationStateMessage, TitleLayoutStateMessage, WireSceneLayout, WireTitleLayout,
};
use fm_protocol::{
    CommandResult, EditLockScope, EngineIdentity, EventCursor, EventMessage, EventPayload,
    FadeToBlackState, FieldIssue, ManualTransitionKind, ManualTransitionPosition,
    ManualTransitionStatus as ProtocolManualTransitionStatus, PlaylistEntrySpec, PlaylistLoopMode,
    PlaylistRunState, PlaylistStateMessage, RecordingStatus, ResumeCursor, ServerHello,
    ServerIdentity, SnapshotMessage, StingerAudioPolicy, StingerMissingMediaFallback,
    StingerReadiness, StreamRealizedState as ProtocolStreamRealizedState, TitleFieldStatus,
    TitleFieldUpdate, TitlePhase, TitleStateMessage, WireRecorderSlot,
};
pub use fm_protocol::{
    InputCategoryStatus, InputLifecycleOperation, InputLifecycleStatus, InputOfflineReason,
    TRANSITION_FAVORITE_COUNT, TransitionFavoriteStatus,
};
use fm_types::{FrameRate, InputId, MAX_INPUT_NAME_BYTES, OutputId, ProjectId, SceneId};

pub use animation_presets::AnimationPresetsStatus;
pub use cue_lists::CueRunUpdate;
pub use grading::{GradeLutStatus, GradingStatus, LibraryGradeStatus};
pub use scripts::{
    ScriptCheck, ScriptCheckKind, ScriptRunReport, ScriptRunUpdate, ScriptVerb,
    check_script_syntax, script_verbs, script_waits,
};
pub use title_animation::{SceneAnimationStatus, TitleAnimationStatus};

pub mod animation_editor;
mod animation_presets;
pub mod cue_lists;
pub mod grade_editor;
mod grading;
pub mod i18n;
mod input_lifecycle;
mod media_lists;

pub mod scene_designer;
pub mod scripts;
pub mod shortcuts;
pub mod strip_dsp;
pub mod title_animation;
pub mod title_editor;

/// Maximum number of replicated stream statuses accepted in one snapshot.
pub const MAX_STREAM_STATUSES: usize = 64;
/// Largest replicated stream status label in bytes.
pub const MAX_STREAM_NAME_BYTES: usize = 128;
/// Number of independently replicated program recorder slots.
pub const RECORDER_SLOT_COUNT: usize = 2;

/// A project-aware cursor used at the UI/protocol boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectCursor {
    pub project_id: ProjectId,
    pub engine: EngineIdentity,
    pub revision: Revision,
}

impl ProjectCursor {
    /// Converts this cursor to the project-agnostic protocol cursor.
    #[must_use]
    pub fn protocol_cursor(&self) -> EventCursor {
        EventCursor {
            engine: self.engine.clone(),
            revision: self.revision.get(),
        }
    }

    /// Converts this cursor to the current project-aware resume DTO.
    #[must_use]
    pub fn resume_cursor(&self) -> ResumeCursor {
        ResumeCursor {
            server: ServerIdentity {
                engine_id: self.engine.engine_id.clone(),
                project_id: self.project_id.to_string(),
                state_epoch: self.engine.state_epoch,
                log_id: self.engine.log_id.clone(),
            },
            revision: self.revision.get(),
        }
    }

    /// Converts and validates a project-aware protocol resume cursor.
    ///
    /// # Errors
    ///
    /// Rejects a different project or an incomplete engine identity.
    pub fn from_resume_cursor(
        project_id: ProjectId,
        cursor: ResumeCursor,
    ) -> Result<Self, ModelError> {
        if cursor.server.project_id != project_id.to_string() {
            return Err(ModelError::ProtocolProjectMismatch {
                expected: project_id,
                observed: cursor.server.project_id,
            });
        }
        let engine = EngineIdentity {
            engine_id: cursor.server.engine_id,
            state_epoch: cursor.server.state_epoch,
            log_id: cursor.server.log_id,
        };
        validate_engine(&engine)?;
        Ok(Self {
            project_id,
            engine,
            revision: Revision::new(cursor.revision),
        })
    }
}

/// Program and Preview selections for one switcher state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BusSelection {
    pub program: InputId,
    pub preview: InputId,
}

impl BusSelection {
    #[must_use]
    pub const fn new(program: InputId, preview: InputId) -> Self {
        Self { program, preview }
    }
}

/// Authoritative desired and runtime-realized switcher state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SwitcherState {
    pub desired: BusSelection,
    pub realized: BusSelection,
    pub desired_manual_transition: ManualTransitionStatus,
    pub realized_manual_transition: ManualTransitionStatus,
    pub desired_fade_to_black: FadeToBlackState,
    pub realized_fade_to_black: FadeToBlackState,
    pub runtime_generation: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StingerStatus {
    pub slot: u8,
    pub media_input: InputId,
    pub preload: bool,
    pub cut_point_frames: u32,
    pub audio_policy: StingerAudioPolicy,
    pub missing_media_fallback: StingerMissingMediaFallback,
    pub readiness: StingerReadiness,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverlayStatus {
    pub channel: u8,
    pub source: Option<InputId>,
    pub active: bool,
    pub opacity: u8,
    pub transition: fm_protocol::OverlayTransitionKind,
    pub duration_frames: u32,
    pub position: fm_protocol::OverlayPositionPreset,
    pub border: fm_protocol::OverlayBorderPreset,
    pub queued_sources: Vec<InputId>,
    pub included_outputs: Vec<OutputId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputAudioStripStatus {
    pub input: InputId,
    pub gain_millidb: i32,
    pub balance_basis_points: i32,
    /// Mono source position (Protocol 2.40); balance trims stereo.
    pub pan_basis_points: i32,
    /// The strip's group, `1..=4` (Protocol 2.40).
    pub group: Option<u8>,
    pub muted: bool,
    pub soloed: bool,
    pub follow_video: bool,
    pub delay_samples: u32,
}

/// Replicated desired and last-confirmed realized state of one stream target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamStatus {
    pub target: NonZeroU128,
    pub name: String,
    pub desired_running: bool,
    pub realized: ProtocolStreamRealizedState,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputStatus {
    pub output: OutputId,
    pub name: String,
}

/// One replicated title input.
///
/// A snapshot announces each title with only its field revision: `complete`
/// is `false` and `name` and `fields` are empty until the session's
/// `title_state` record for it is applied
/// ([`ClientModel::apply_title_state`]). `fields` is then the complete value
/// set in strictly ascending field order, and `phase`, `phase_frame`, and
/// `phase_frames` the title's animation progress at the snapshot's frame
/// cursor (idle and zero until then).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleStatus {
    pub input: InputId,
    pub name: String,
    pub field_revision: u64,
    pub fields: Vec<TitleFieldStatus>,
    pub phase: TitlePhase,
    pub phase_frame: u64,
    pub phase_frames: u64,
    pub complete: bool,
    /// The template's animation: `None` until the snapshot's
    /// `title_animation_state` for this title arrives, then kept current by
    /// `title_animation_changed`.
    pub animation: Option<TitleAnimationStatus>,
    /// The template's canvas and element outlines: `None` until the
    /// snapshot's `title_layout_state` for this title arrives (Protocol
    /// 2.30). No command changes a layout between snapshots.
    pub layout: Option<Arc<WireTitleLayout>>,
}

impl TitleStatus {
    /// A title a snapshot announced, awaiting its `title_state`.
    #[must_use]
    pub const fn announced(input: InputId, field_revision: u64) -> Self {
        Self {
            input,
            name: String::new(),
            field_revision,
            fields: Vec::new(),
            phase: TitlePhase::Idle,
            phase_frame: 0,
            phase_frames: 0,
            complete: false,
            animation: None,
            layout: None,
        }
    }
}

/// One replicated Scene input's animation and layout (Protocol 2.31,
/// GX-011 stage 5).
///
/// A snapshot announces each Scene input in its `scenes` list: `scene` and
/// `animation` are `None` until the session's `scene_animation_state` for
/// it is applied, and `layout` until the `scene_layout_state` of its scene
/// is (Protocol 2.35: layouts are keyed by scene). Scene inputs showing the
/// same scene report the same animation and layout, and
/// `scene_animation_changed` keeps all of them current.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneStatus {
    pub input: InputId,
    /// The scene the input shows (Protocol 2.35).
    pub scene: Option<SceneId>,
    pub animation: Option<SceneAnimationStatus>,
    /// The scene's canvas and layer outlines, kept current by
    /// `scene_layout_changed` (Protocol 2.39).
    pub layout: Option<Arc<WireSceneLayout>>,
}

impl SceneStatus {
    /// A Scene input a snapshot announced, awaiting its follow-on records.
    #[must_use]
    pub const fn announced(input: InputId) -> Self {
        Self {
            input,
            scene: None,
            animation: None,
            layout: None,
        }
    }
}

/// One replicated project scene's name and layout (Protocol 2.35), whether
/// or not a Scene input shows it.
///
/// A snapshot announces every scene in its `layout_scenes` list: `name` is
/// empty and `layout` `None` until the session's `scene_layout_state` for
/// it is applied. A live scene layer edit replaces the layout
/// (`scene_layout_changed`, Protocol 2.39); the name changes only with a new
/// snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneLayoutStatus {
    pub scene: SceneId,
    pub name: String,
    pub layout: Option<Arc<WireSceneLayout>>,
}

impl SceneLayoutStatus {
    /// A scene a snapshot announced, awaiting its `scene_layout_state`.
    #[must_use]
    pub const fn announced(scene: SceneId) -> Self {
        Self {
            scene,
            name: String::new(),
            layout: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActiveManualTransition {
    pub kind: ManualTransitionKind,
    pub from: InputId,
    pub to: InputId,
    pub interval_start: ManualTransitionPosition,
    pub position: ManualTransitionPosition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManualTransitionStatus {
    Inactive,
    Active(ActiveManualTransition),
}

impl ManualTransitionStatus {
    #[must_use]
    pub const fn from_protocol(status: ProtocolManualTransitionStatus) -> Self {
        match status {
            ProtocolManualTransitionStatus::Inactive => Self::Inactive,
            ProtocolManualTransitionStatus::Active(state) => Self::Active(ActiveManualTransition {
                kind: state.kind,
                from: state.from.to_domain(),
                to: state.to.to_domain(),
                interval_start: state.interval_start,
                position: state.position,
            }),
        }
    }
}

/// The padlock target of one replicated edit lock.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EditLockTarget {
    Project,
    Input(InputId),
}

impl EditLockTarget {
    #[must_use]
    pub const fn from_protocol(scope: EditLockScope) -> Self {
        match scope {
            EditLockScope::Project => Self::Project,
            EditLockScope::Input(input) => Self::Input(input.to_domain()),
        }
    }
}

/// Replicated engaged edit padlocks; `inputs` is in ascending identifier order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EditLocks {
    pub project: bool,
    pub inputs: Vec<InputId>,
}

impl EditLocks {
    #[must_use]
    pub fn from_protocol(status: fm_protocol::EditLockStatus) -> Self {
        Self {
            project: status.project,
            inputs: status
                .inputs
                .into_iter()
                .map(fm_protocol::WireInputId::to_domain)
                .collect(),
        }
    }

    /// Returns whether `input` has its own padlock engaged.
    #[must_use]
    pub fn is_input_locked(&self, input: InputId) -> bool {
        self.inputs.contains(&input)
    }

    /// Returns whether an edit of `input` (rename or move) is blocked by
    /// either the project padlock or the input's own padlock.
    #[must_use]
    pub fn blocks_input_edit(&self, input: InputId) -> bool {
        self.project || self.is_input_locked(input)
    }

    fn set(&mut self, target: EditLockTarget, locked: bool) {
        match target {
            EditLockTarget::Project => self.project = locked,
            EditLockTarget::Input(input) => {
                let position = self.inputs.binary_search(&input);
                match (position, locked) {
                    (Err(index), true) => self.inputs.insert(index, input),
                    (Ok(index), false) => {
                        self.inputs.remove(index);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// An authoritative project snapshot suitable for the UI reducer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectSnapshot {
    pub cursor: ProjectCursor,
    pub show_name: String,
    pub inputs: Vec<InputId>,
    pub input_names: Vec<String>,
    pub outputs: Vec<OutputStatus>,
    pub input_audio_strips: Vec<InputAudioStripStatus>,
    pub stingers: Vec<StingerStatus>,
    pub streams: Vec<StreamStatus>,
    pub recordings: Vec<RecordingStatus>,
    pub desired_overlays: Vec<OverlayStatus>,
    pub realized_overlays: Vec<OverlayStatus>,
    pub switcher: SwitcherState,
    pub edit_locks: EditLocks,
    pub edit_history: EditHistoryStatus,
    /// Every title input, ascending by input, each incomplete.
    pub titles: Vec<TitleStatus>,
    /// Every Scene input, ascending by input, each incomplete.
    pub scenes: Vec<SceneStatus>,
    /// Every project scene, ascending by scene, each incomplete (Protocol
    /// 2.35).
    pub scene_layouts: Vec<SceneLayoutStatus>,
    /// The engine's video frame rate; playlist frames count these frames.
    pub frame_rate: FrameRate,
    /// Whether a `playlist_state` completes this snapshot. Otherwise the
    /// playlist is empty, stopped, and does not loop.
    pub playlist_follows: bool,
    /// How many `grade_state` records complete this snapshot.
    pub grade_states: u16,
    /// How many `animation_preset_state` records complete this snapshot
    /// (Protocol 2.34).
    pub animation_preset_states: u16,
    /// The favourite transition buttons, slot 1 first (Protocol 2.36).
    pub transition_favorites: [TransitionFavoriteStatus; TRANSITION_FAVORITE_COUNT],
    /// How many `media_state` records complete this snapshot (Protocol
    /// 2.37).
    pub media_states: u16,
    /// How many `cue_list_state` records complete this snapshot (Protocol
    /// 2.50).
    pub cue_list_states: u16,
    /// How many `script_state` records complete this snapshot (Protocol
    /// 2.58).
    pub script_states: u16,
    /// How many `shortcut_state` records complete this snapshot (Protocol
    /// 2.70).
    pub shortcut_states: u16,
    /// How many `still_state` records complete this snapshot (IN-008,
    /// IN-009).
    pub still_states: u16,
    /// How many `media_list_state` records complete this snapshot (IN-006).
    pub media_list_states: u16,
    /// Each input's lifecycle, parallel to `inputs` (Protocol 2.38).
    pub input_lifecycle: Vec<InputLifecycleStatus>,
    /// The project's input categories (Protocol 2.38).
    pub input_categories: Vec<InputCategoryStatus>,
    /// Strip labels and groups (Protocol 2.40).
    pub audio_mixer: fm_protocol::AudioMixerStatus,
    /// The outputs Fade to Black leaves live, ascending (Protocol 2.40).
    pub fade_to_black_excluded_outputs: Vec<OutputId>,
    /// The outputs that fade over their own duration in frames, ascending
    /// by output (Protocol 2.48).
    pub fade_to_black_output_durations: Vec<(OutputId, u32)>,
    /// How automatic Fade-to-Black moves ease (Protocol 2.48).
    pub fade_to_black_curve: fm_protocol::FadeToBlackCurve,
    /// Every configured display-output slot, ascending (Protocol 2.42).
    pub display_outputs: Vec<fm_protocol::DisplayOutputEntry>,
    /// Every configured slot's runtime state, ascending (Protocol 2.42).
    pub display_output_states: Vec<fm_protocol::DisplayOutputStatus>,
    /// Every network stream input, ascending, with its redacted config and
    /// status (Protocol 2.46).
    pub network_streams: Vec<fm_protocol::NetworkStreamEntry>,
    /// Every capture input, ascending, with its stable key, settings, and
    /// status (Protocol 2.47).
    pub capture_inputs: Vec<fm_protocol::CaptureInputEntry>,
    /// Every system audio input, ascending, with its device and status
    /// (Protocol 2.55).
    pub system_audio_inputs: Vec<fm_protocol::SystemAudioEntry>,
    /// Every configured stream destination in authored order, without its
    /// key (Protocol 2.52).
    pub stream_targets: Vec<fm_protocol::StreamTargetEntry>,
    /// The `LiveLAN` configuration, without its PIN (Protocol 2.57).
    pub live_lan: fm_protocol::LiveLanEntry,
    /// The `LiveLAN` stream's latest status (Protocol 2.57).
    pub live_lan_status: fm_protocol::LiveLanStatus,
}

impl ProjectSnapshot {
    /// Adds the project identity omitted by the current protocol snapshot DTO.
    #[must_use]
    #[allow(clippy::too_many_lines)] // one conversion per snapshot field
    pub fn from_protocol(project_id: ProjectId, message: SnapshotMessage) -> Self {
        let stingers = protocol_stingers(message.stingers);
        let streams = protocol_streams(message.streams);
        let desired_overlays = protocol_overlays(message.desired_overlays);
        let realized_overlays = protocol_overlays(message.realized_overlays);
        let input_audio_strips = protocol_input_audio_strips(message.input_audio_strips);
        let input_lifecycle = message.inputs.iter().map(|input| input.lifecycle).collect();
        let (inputs, input_names) = message
            .inputs
            .into_iter()
            .map(|input| (input.input.to_domain(), input.name))
            .unzip();
        let outputs = message
            .outputs
            .into_iter()
            .map(|output| OutputStatus {
                output: output.output.to_domain(),
                name: output.name,
            })
            .collect();
        Self {
            cursor: ProjectCursor {
                project_id,
                engine: message.engine,
                revision: Revision::new(message.revision),
            },
            show_name: message.show_name,
            inputs,
            input_names,
            outputs,
            input_audio_strips,
            stingers,
            streams,
            recordings: message.recordings,
            desired_overlays,
            realized_overlays,
            switcher: SwitcherState {
                desired: BusSelection::new(
                    message.desired_program.to_domain(),
                    message.desired_preview.to_domain(),
                ),
                realized: BusSelection::new(
                    message.realized_program.to_domain(),
                    message.realized_preview.to_domain(),
                ),
                desired_manual_transition: manual_status_from_protocol(
                    message.desired_manual_transition,
                ),
                realized_manual_transition: manual_status_from_protocol(
                    message.realized_manual_transition,
                ),
                desired_fade_to_black: fade_to_black_from_protocol(message.desired_fade_to_black),
                realized_fade_to_black: fade_to_black_from_protocol(message.realized_fade_to_black),
                runtime_generation: None,
            },
            edit_locks: EditLocks::from_protocol(message.edit_locks),
            edit_history: message.edit_history,
            titles: message
                .titles
                .into_iter()
                .map(|title| TitleStatus::announced(title.input.to_domain(), title.field_revision))
                .collect(),
            scenes: message
                .scenes
                .into_iter()
                .map(|input| SceneStatus::announced(input.to_domain()))
                .collect(),
            scene_layouts: message
                .layout_scenes
                .into_iter()
                .map(|scene| SceneLayoutStatus::announced(scene.to_domain()))
                .collect(),
            frame_rate: message.frame_rate,
            playlist_follows: message.playlist_follows,
            grade_states: message.grade_states,
            animation_preset_states: message.animation_preset_states,
            transition_favorites: message.transition_favorites,
            media_states: message.media_states,
            cue_list_states: message.cue_list_states,
            script_states: message.script_states,
            shortcut_states: message.shortcut_states,
            still_states: message.still_states,
            media_list_states: message.media_list_states,
            input_lifecycle,
            input_categories: message.input_categories,
            audio_mixer: message.audio_mixer,
            fade_to_black_excluded_outputs: message
                .fade_to_black_excluded_outputs
                .into_iter()
                .map(fm_protocol::WireOutputId::to_domain)
                .collect(),
            fade_to_black_output_durations: message
                .fade_to_black_output_durations
                .into_iter()
                .map(|(output, frames)| (output.to_domain(), frames))
                .collect(),
            fade_to_black_curve: message.fade_to_black_curve,
            display_outputs: message.display_outputs,
            display_output_states: message.display_output_states,
            network_streams: message.network_streams,
            capture_inputs: message.capture_inputs,
            system_audio_inputs: message.system_audio_inputs,
            stream_targets: message.stream_targets,
            live_lan: message.live_lan,
            live_lan_status: message.live_lan_status,
        }
    }
}

/// A UI-owned durable desired-state change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DurableChange {
    InputOrderChanged {
        inputs: Vec<InputId>,
    },
    InputRenamed {
        input: InputId,
        name: String,
    },
    /// An input appended to the end of the order with a default Master
    /// strip and no padlock.
    InputAdded {
        input: InputId,
        name: String,
        /// The redacted configuration of an added network stream (Protocol
        /// 2.46).
        network_stream: Option<Box<fm_protocol::NetworkStreamConfig>>,
        /// The device and settings of an added capture input (Protocol
        /// 2.47).
        capture: Option<Box<fm_protocol::CaptureInputConfig>>,
        /// The device of an added system audio input (Protocol 2.55).
        system_audio: Option<Box<fm_protocol::SystemAudioConfig>>,
        /// The picture or slideshow of an added image or slideshow input
        /// (IN-008, IN-009).
        still: Option<Box<fm_protocol::WireStillSource>>,
    },
    /// An input removed with its strip and padlock.
    InputRemoved {
        input: InputId,
    },
    DesiredSwitcher {
        selection: BusSelection,
        manual_transition: ManualTransitionStatus,
        fade_to_black: FadeToBlackState,
        overlays: Vec<OverlayStatus>,
        input_audio_strips: Vec<InputAudioStripStatus>,
    },
    StingerSlotsChanged {
        selection: BusSelection,
        manual_transition: ManualTransitionStatus,
        fade_to_black: FadeToBlackState,
        stingers: Vec<StingerStatus>,
        overlays: Vec<OverlayStatus>,
        input_audio_strips: Vec<InputAudioStripStatus>,
    },
    StreamsChanged {
        streams: Vec<StreamStatus>,
    },
    /// A stream destination was added, reconfigured, or removed, with the
    /// resulting roster (Protocol 2.52).
    StreamTargetChanged {
        target: NonZeroU128,
        entry: Option<Box<fm_protocol::StreamTargetEntry>>,
        streams: Vec<StreamStatus>,
    },
    RecordingChanged {
        recorder: WireRecorderSlot,
        active: bool,
    },
    EditLockChanged {
        target: EditLockTarget,
        locked: bool,
    },
    /// A title's complete values and field revision after a save or take,
    /// with the complete desired overlay state.
    TitleChanged {
        input: InputId,
        field_revision: u64,
        fields: Vec<TitleFieldUpdate>,
        overlays: Vec<OverlayStatus>,
    },
    /// The complete playlist after a playlist command, with the desired
    /// Program and Preview (an entry taken on air reaches Program on the
    /// playlist's `frame`).
    PlaylistChanged {
        selection: BusSelection,
        playlist: PlaylistStatus,
    },
    /// One accepted grade edit.
    GradeChanged(GradeChange),
    /// A title template's animation after an edit, undo, or redo: every
    /// title input sharing the template (ascending) and the animation they
    /// all show.
    TitleAnimationChanged {
        inputs: Vec<InputId>,
        animation: TitleAnimationStatus,
    },
    /// A scene's layer animation after an edit, undo, or redo (Protocol
    /// 2.31): every Scene input showing the scene (ascending) and the
    /// animation they all show.
    SceneAnimationChanged {
        inputs: Vec<InputId>,
        animation: SceneAnimationStatus,
    },
    /// A scene's whole layout after a live scene layer edit, undo, or redo
    /// (Protocol 2.39): the edit applied and the layout every Scene input
    /// showing the scene shows.
    SceneLayoutChanged {
        scene: SceneId,
        edit: fm_protocol::SceneLayerEdit,
        layout: Arc<WireSceneLayout>,
    },
    /// One accepted animation preset library edit (Protocol 2.34).
    AnimationPresetChanged(AnimationPresetChange),
    /// One favourite transition button after an assignment, undo, or redo
    /// (Protocol 2.36).
    TransitionFavoriteChanged {
        slot: u8,
        favorite: TransitionFavoriteStatus,
    },
    /// A media input's complete status after an accepted media command
    /// (Protocol 2.37).
    MediaChanged {
        status: Box<MediaStatus>,
    },
    /// A slideshow page navigation was accepted (IN-009); the slide it
    /// lands on arrives as a `slideshow_page` runtime event.
    PageNavigated {
        input: InputId,
    },
    /// An accepted input lifecycle edit (Protocol 2.38): the operation and
    /// the inputs it appended, in order.
    InputLifecycleChanged {
        operation: InputLifecycleOperation,
        added: Vec<(InputId, String)>,
    },
    /// An accepted mixer label, group, or group-level operation (Protocol
    /// 2.40).
    AudioMixerChanged(fm_protocol::AudioMixerOperation),
    /// Fade to Black now fades `output` or leaves it live (Protocol 2.40).
    FadeToBlackOutputChanged {
        output: OutputId,
        affected: bool,
    },
    /// `output` now fades over its own duration (Protocol 2.48).
    FadeToBlackOutputDurationChanged {
        output: OutputId,
        duration_frames: Option<u32>,
    },
    /// Automatic Fade-to-Black moves now ease along `curve` (Protocol 2.48).
    FadeToBlackCurveChanged {
        curve: fm_protocol::FadeToBlackCurve,
    },
    /// A display-output slot was configured or cleared (Protocol 2.42).
    DisplayOutputChanged {
        slot: fm_protocol::DisplayOutputSlot,
        config: Option<Box<fm_protocol::DisplayOutputConfig>>,
    },
    /// The `LiveLAN` configuration changed (Protocol 2.57); never the PIN.
    LiveLanChanged(fm_protocol::LiveLanEntry),
    /// A network stream input was reconfigured (Protocol 2.46); the URL is
    /// redacted.
    NetworkStreamChanged {
        input: InputId,
        config: Box<fm_protocol::NetworkStreamConfig>,
    },
    /// A capture input's settings changed (Protocol 2.47).
    CaptureSettingsChanged {
        input: InputId,
        settings: Box<fm_protocol::CaptureSettings>,
    },
    /// A cue list after an edit, undo, or redo (Protocol 2.50).
    CueListChanged(Box<fm_protocol::CueListStatus>),
    /// A cue list was removed.
    CueListRemoved {
        list: u64,
    },
    /// A live cue operation or clock advance.
    CueRunChanged(Box<CueRunUpdate>),
    /// A script after an edit, undo, or redo (Protocol 2.58).
    ScriptChanged(Box<fm_protocol::ScriptStatus>),
    /// A script was removed.
    ScriptRemoved {
        script: u64,
    },
    /// An accepted script run, or one part of a run that waited.
    ScriptRan(Box<ScriptRunUpdate>),
    /// A script's run was stopped, its fault cleared, or the part due on
    /// `frame` failed (Protocol 2.62); `run` is the run afterwards.
    ScriptRunChanged {
        script: u64,
        frame: u64,
        run: fm_protocol::ScriptRunStatus,
    },
    /// Every script run was stopped and every fault cleared on `frame`
    /// (Protocol 2.69); `scripts` are idle now.
    ScriptRunsStopped {
        frame: u64,
        scripts: Vec<u64>,
    },
    /// A user shortcut after an edit, undo, or redo (Protocol 2.72).
    ShortcutChanged(Box<fm_protocol::ShortcutStatus>),
    /// A user shortcut was removed.
    ShortcutRemoved {
        shortcut: u64,
    },
    /// A shortcut's cue actions were accepted.
    ShortcutFired(Box<shortcuts::ShortcutFireUpdate>),
    /// A media list input after `operation` on `frame` (IN-006); `created`
    /// names the input a creation added.
    MediaListChanged {
        frame: u64,
        operation: &'static str,
        created: Option<String>,
        status: Box<fm_protocol::MediaListStatus>,
    },
}

/// The replicated playlist as of its snapshot's `playlist_state` or its last
/// `playlist_changed`. Durations and frames are engine frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaylistStatus {
    /// The engine frame this status was observed at: the target frame of
    /// the last playlist command, or the frame cursor when the snapshot's
    /// `playlist_state` was written (zero for a snapshot's implied empty
    /// playlist).
    pub frame: u64,
    pub loop_mode: PlaylistLoopMode,
    pub entries: Vec<PlaylistEntrySpec>,
    pub state: PlaylistRunState,
}

impl PlaylistStatus {
    /// The empty, stopped, non-looping playlist a snapshot implies when no
    /// `playlist_state` follows it.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            frame: 0,
            loop_mode: PlaylistLoopMode::Off,
            entries: Vec::new(),
            state: PlaylistRunState::Stopped,
        }
    }

    /// The playlist a snapshot's `playlist_state` carries.
    #[must_use]
    pub fn from_protocol(message: PlaylistStateMessage) -> Self {
        Self {
            frame: message.frame,
            loop_mode: message.loop_mode,
            entries: message.entries,
            state: message.state,
        }
    }
}

/// One globally ordered project event.
///
/// `edit_history` is the server's undo/redo summary after this revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableProjectEvent {
    pub cursor: ProjectCursor,
    pub change: DurableChange,
    pub edit_history: EditHistoryStatus,
}

impl DurableProjectEvent {
    /// Adds project identity and converts the current protocol event DTO.
    #[must_use]
    #[allow(clippy::too_many_lines)] // one match over every protocol event
    pub fn from_protocol(project_id: ProjectId, message: EventMessage) -> Self {
        let change = match message.payload {
            EventPayload::InputOrderChanged { inputs } => DurableChange::InputOrderChanged {
                inputs: inputs
                    .into_iter()
                    .map(fm_protocol::WireInputId::to_domain)
                    .collect(),
            },
            EventPayload::InputRenamed { input, name } => DurableChange::InputRenamed {
                input: input.to_domain(),
                name,
            },
            EventPayload::InputAdded {
                input,
                name,
                network_stream,
                capture,
                system_audio,
                still,
            } => DurableChange::InputAdded {
                input: input.to_domain(),
                name,
                network_stream,
                capture,
                system_audio,
                still,
            },
            EventPayload::CaptureSettingsChanged { input, settings } => {
                DurableChange::CaptureSettingsChanged {
                    input: input.to_domain(),
                    settings,
                }
            }
            EventPayload::NetworkStreamChanged { input, config } => {
                DurableChange::NetworkStreamChanged {
                    input: input.to_domain(),
                    config,
                }
            }
            EventPayload::InputRemoved { input } => DurableChange::InputRemoved {
                input: input.to_domain(),
            },
            EventPayload::MediaListChanged {
                frame,
                operation,
                created,
                status,
            } => DurableChange::MediaListChanged {
                frame,
                operation,
                created,
                status,
            },
            EventPayload::DesiredSwitcher {
                program,
                preview,
                manual_transition,
                fade_to_black,
                overlays,
                input_audio_strips,
            } => DurableChange::DesiredSwitcher {
                selection: BusSelection::new(program.to_domain(), preview.to_domain()),
                manual_transition: manual_status_from_protocol(manual_transition),
                fade_to_black: fade_to_black_from_protocol(fade_to_black),
                overlays: protocol_overlays(overlays),
                input_audio_strips: protocol_input_audio_strips(input_audio_strips),
            },
            EventPayload::StingerSlotsChanged {
                program,
                preview,
                manual_transition,
                fade_to_black,
                stingers,
                overlays,
                input_audio_strips,
            } => DurableChange::StingerSlotsChanged {
                selection: BusSelection::new(program.to_domain(), preview.to_domain()),
                manual_transition: manual_status_from_protocol(manual_transition),
                fade_to_black: fade_to_black_from_protocol(fade_to_black),
                stingers: protocol_stingers(stingers),
                overlays: protocol_overlays(overlays),
                input_audio_strips: protocol_input_audio_strips(input_audio_strips),
            },
            EventPayload::StreamsChanged { streams } => DurableChange::StreamsChanged {
                streams: protocol_streams(streams),
            },
            EventPayload::StreamTargetChanged {
                target,
                entry,
                streams,
            } => DurableChange::StreamTargetChanged {
                target: target.get(),
                entry,
                streams: protocol_streams(streams),
            },
            EventPayload::RecordingChanged { recorder, active } => {
                DurableChange::RecordingChanged { recorder, active }
            }
            EventPayload::EditLockChanged { scope, locked } => DurableChange::EditLockChanged {
                target: EditLockTarget::from_protocol(scope),
                locked,
            },
            EventPayload::TitleChanged {
                input,
                field_revision,
                fields,
                overlays,
            } => DurableChange::TitleChanged {
                input: input.to_domain(),
                field_revision,
                fields,
                overlays: protocol_overlays(overlays),
            },
            EventPayload::PlaylistChanged {
                frame,
                loop_mode,
                entries,
                state,
                program,
                preview,
            } => DurableChange::PlaylistChanged {
                selection: BusSelection::new(program.to_domain(), preview.to_domain()),
                playlist: PlaylistStatus {
                    frame,
                    loop_mode,
                    entries,
                    state,
                },
            },
            EventPayload::GradeChanged(change) => DurableChange::GradeChanged(change),
            EventPayload::TransitionFavoriteChanged { slot, favorite } => {
                DurableChange::TransitionFavoriteChanged { slot, favorite }
            }
            EventPayload::MediaChanged { status, .. } => DurableChange::MediaChanged { status },
            EventPayload::PageNavigated { input, .. } => DurableChange::PageNavigated {
                input: input.to_domain(),
            },
            EventPayload::InputLifecycleChanged {
                operation, added, ..
            } => DurableChange::InputLifecycleChanged {
                operation,
                added: added
                    .into_iter()
                    .map(|added| (added.input.to_domain(), added.name))
                    .collect(),
            },
            EventPayload::AnimationPresetChanged(change) => {
                DurableChange::AnimationPresetChanged(change)
            }
            payload @ EventPayload::TitleAnimationChanged { .. } => title_animation_change(payload),
            EventPayload::SceneAnimationChanged {
                inputs,
                animation_revision,
                clip,
            } => DurableChange::SceneAnimationChanged {
                inputs: inputs
                    .into_iter()
                    .map(fm_protocol::WireInputId::to_domain)
                    .collect(),
                animation: SceneAnimationStatus {
                    revision: animation_revision,
                    clip: Arc::from(clip),
                },
            },
            EventPayload::SceneLayoutChanged { edit, layout } => {
                DurableChange::SceneLayoutChanged {
                    scene: edit.scene.to_domain(),
                    edit,
                    layout: Arc::from(layout),
                }
            }
            EventPayload::AudioMixerChanged(operation) => {
                DurableChange::AudioMixerChanged(operation)
            }
            EventPayload::FadeToBlackOutputChanged { output, affected } => {
                DurableChange::FadeToBlackOutputChanged {
                    output: output.to_domain(),
                    affected,
                }
            }
            EventPayload::FadeToBlackOutputDurationChanged {
                output,
                duration_frames,
            } => DurableChange::FadeToBlackOutputDurationChanged {
                output: output.to_domain(),
                duration_frames,
            },
            EventPayload::FadeToBlackCurveChanged { curve } => {
                DurableChange::FadeToBlackCurveChanged { curve }
            }
            EventPayload::DisplayOutputChanged { slot, config } => {
                DurableChange::DisplayOutputChanged { slot, config }
            }
            EventPayload::LiveLanChanged(entry) => DurableChange::LiveLanChanged(entry),
            EventPayload::CueListChanged(list) => DurableChange::CueListChanged(list),
            EventPayload::CueListRemoved { list } => DurableChange::CueListRemoved { list },
            EventPayload::CueRunChanged(change) => {
                DurableChange::CueRunChanged(Box::new(CueRunUpdate::from_protocol(*change)))
            }
            EventPayload::ScriptChanged(script) => DurableChange::ScriptChanged(script),
            EventPayload::ScriptRemoved { script } => DurableChange::ScriptRemoved { script },
            EventPayload::ScriptRan(change) => {
                DurableChange::ScriptRan(Box::new(ScriptRunUpdate::from_protocol(*change)))
            }
            EventPayload::ScriptRunChanged { script, frame, run } => {
                DurableChange::ScriptRunChanged { script, frame, run }
            }
            EventPayload::ScriptRunsStopped { frame, scripts } => {
                DurableChange::ScriptRunsStopped { frame, scripts }
            }
            EventPayload::ShortcutChanged(shortcut) => DurableChange::ShortcutChanged(shortcut),
            EventPayload::ShortcutRemoved { shortcut } => {
                DurableChange::ShortcutRemoved { shortcut }
            }
            EventPayload::ShortcutFired(change) => DurableChange::ShortcutFired(Box::new(
                shortcuts::ShortcutFireUpdate::from_protocol(*change),
            )),
        };
        Self {
            cursor: ProjectCursor {
                project_id,
                engine: message.cursor.engine,
                revision: Revision::new(message.cursor.revision),
            },
            change,
            edit_history: message.edit_history,
        }
    }

    /// Combines a project-aware resume cursor with a UI-decoded durable change.
    ///
    /// The current durable batch DTO intentionally leaves event payloads
    /// opaque, so transport adapters decode only the event types understood by
    /// this model and pass the resulting change here.
    ///
    /// # Errors
    ///
    /// Rejects a cursor for another project or an incomplete engine identity.
    pub fn from_resume_cursor(
        project_id: ProjectId,
        cursor: ResumeCursor,
        change: DurableChange,
        edit_history: EditHistoryStatus,
    ) -> Result<Self, ModelError> {
        Ok(Self {
            cursor: ProjectCursor::from_resume_cursor(project_id, cursor)?,
            change,
            edit_history,
        })
    }
}

/// Independently ordered runtime confirmation of a durable switcher revision.
///
/// The realized routing is looked up from the desired state retained for
/// `revision`; runtime messages do not carry or modify durable desired state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeRealization {
    pub project_id: ProjectId,
    pub engine: EngineIdentity,
    pub revision: Revision,
    pub generation: u64,
    pub sequence: u64,
    pub manual_transition: ManualTransitionStatus,
    pub fade_to_black: FadeToBlackState,
}

/// Authoritative project data at the last applied cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectState {
    show_name: String,
    inputs: Vec<InputId>,
    input_names: Vec<String>,
    outputs: Vec<OutputStatus>,
    input_audio_strips: Vec<InputAudioStripStatus>,
    stingers: Vec<StingerStatus>,
    streams: Vec<StreamStatus>,
    recordings: Vec<RecordingStatus>,
    desired_overlays: Vec<OverlayStatus>,
    realized_overlays: Vec<OverlayStatus>,
    switcher: SwitcherState,
    edit_locks: EditLocks,
    edit_history: EditHistoryStatus,
    titles: Vec<TitleStatus>,
    scenes: Vec<SceneStatus>,
    scene_layouts: Vec<SceneLayoutStatus>,
    frame_rate: FrameRate,
    /// `None` while the snapshot's announced `playlist_state` is awaited.
    playlist: Option<PlaylistStatus>,
    grading: GradingStatus,
    /// `grade_state` records the snapshot announced that have not arrived.
    grade_states_pending: u16,
    animation_presets: AnimationPresetsStatus,
    /// `animation_preset_state` records the snapshot announced that have
    /// not arrived.
    animation_preset_states_pending: u16,
    transition_favorites: [TransitionFavoriteStatus; TRANSITION_FAVORITE_COUNT],
    /// Every media input's replicated status (Protocol 2.37), kept current
    /// by `media_changed` and `media` runtime events.
    media: BTreeMap<InputId, MediaStatus>,
    /// `media_state` records the snapshot announced that have not arrived.
    media_states_pending: u16,
    /// Every cue list in project order (Protocol 2.50).
    cue_lists: Vec<fm_protocol::CueListStatus>,
    /// `cue_list_state` records the snapshot announced that have not
    /// arrived.
    cue_list_states_pending: u16,
    /// The newest engine frame a `cue_list_state` or `cue_run_changed`
    /// reported (Protocol 2.64), from which Studio counts an armed GO down.
    cue_frame: Option<u64>,
    /// Every script in project order (Protocol 2.58).
    scripts: Vec<fm_protocol::ScriptStatus>,
    /// `script_state` records the snapshot announced that have not arrived.
    script_states_pending: u16,
    /// The last accepted run of each script since this snapshot, direct
    /// or by a cue's script step (Protocol 2.61), the latest last.
    script_runs: Vec<ScriptRunReport>,
    /// Every script whose run waits or faulted, with that state (Protocol
    /// 2.62; several since 2.69); every other script is idle.
    script_run_states: BTreeMap<u64, fm_protocol::ScriptRunStatus>,
    /// Every user shortcut in project order (Protocol 2.72).
    shortcuts: Vec<fm_protocol::ShortcutStatus>,
    /// `shortcut_state` records the snapshot announced that have not
    /// arrived.
    shortcut_states_pending: u16,
    /// The last accepted fire of a shortcut's cue actions since this
    /// snapshot.
    last_shortcut_fire: Option<shortcuts::ShortcutFireReport>,
    /// Every media list input's list (IN-006).
    media_lists: BTreeMap<InputId, fm_protocol::MediaListStatus>,
    /// `media_list_state` records the snapshot announced that have not
    /// arrived.
    media_list_states_pending: u16,
    /// Every input's non-default lifecycle (Protocol 2.38).
    input_lifecycle: BTreeMap<InputId, InputLifecycleStatus>,
    /// The project's input categories (Protocol 2.38).
    input_categories: Vec<InputCategoryStatus>,
    /// Strip labels and groups (Protocol 2.40).
    audio_mixer: fm_protocol::AudioMixerStatus,
    /// Every input strip's non-default DSP chain (Protocol 2.75), kept
    /// current by `audio_mixer_changed`.
    audio_strip_dsp: BTreeMap<InputId, fm_types::strip_dsp::StripDsp>,
    /// `audio_strip_dsp_state` records the snapshot announced that have not
    /// arrived.
    audio_strip_dsp_states_pending: u16,
    /// The outputs Fade to Black leaves live, ascending (Protocol 2.40).
    fade_to_black_excluded_outputs: Vec<OutputId>,
    /// The outputs with their own Fade-to-Black duration (Protocol 2.48).
    fade_to_black_output_durations: Vec<(OutputId, u32)>,
    /// How automatic Fade-to-Black moves ease (Protocol 2.48).
    fade_to_black_curve: fm_protocol::FadeToBlackCurve,
    /// Every configured display-output slot (Protocol 2.42).
    display_outputs: BTreeMap<fm_protocol::DisplayOutputSlot, fm_protocol::DisplayOutputConfig>,
    /// Each slot's runtime state as last reported (Protocol 2.42); kept for
    /// unconfigured slots too.
    display_output_states:
        BTreeMap<fm_protocol::DisplayOutputSlot, fm_protocol::WireDisplayOutputState>,
    /// Every network stream input's redacted config and latest status
    /// (Protocol 2.46).
    network_streams: BTreeMap<InputId, fm_protocol::NetworkStreamEntry>,
    /// Every capture input's stable key, settings, and latest status
    /// (Protocol 2.47).
    capture_inputs: BTreeMap<InputId, fm_protocol::CaptureInputEntry>,
    /// Every system audio input's device and latest status (Protocol 2.55).
    system_audio_inputs: BTreeMap<InputId, fm_protocol::SystemAudioEntry>,
    /// Every image and slideshow input's source and, for a slideshow, the
    /// slide on air (IN-008, IN-009).
    still_inputs: BTreeMap<InputId, StillInputStatus>,
    /// `still_state` records the snapshot announced that have not arrived.
    still_states_pending: u16,
    /// Every configured stream destination in authored order (Protocol
    /// 2.52).
    stream_targets: Vec<fm_protocol::StreamTargetEntry>,
    /// The `LiveLAN` configuration, without its PIN (Protocol 2.57).
    live_lan: fm_protocol::LiveLanEntry,
    /// The `LiveLAN` stream's status as last reported (Protocol 2.57).
    live_lan_status: fm_protocol::LiveLanStatus,
}

impl ProjectState {
    /// The `LiveLAN` configuration, without its PIN (Protocol 2.57).
    #[must_use]
    pub const fn live_lan(&self) -> &fm_protocol::LiveLanEntry {
        &self.live_lan
    }

    /// The `LiveLAN` stream's status as clients see it (Protocol 2.57).
    #[must_use]
    pub fn live_lan_status(&self) -> fm_protocol::LiveLanStatus {
        self.live_lan_status.for_config(self.live_lan.enabled)
    }

    /// Network stream input `input`'s redacted config and latest status
    /// (Protocol 2.46).
    #[must_use]
    pub fn network_stream(&self, input: InputId) -> Option<&fm_protocol::NetworkStreamEntry> {
        self.network_streams.get(&input)
    }

    /// Capture input `input`'s stable key, settings, and latest status
    /// (Protocol 2.47).
    #[must_use]
    pub fn capture_input(&self, input: InputId) -> Option<&fm_protocol::CaptureInputEntry> {
        self.capture_inputs.get(&input)
    }

    /// System audio input `input`'s device and latest status (Protocol
    /// 2.55).
    #[must_use]
    pub fn system_audio_input(&self, input: InputId) -> Option<&fm_protocol::SystemAudioEntry> {
        self.system_audio_inputs.get(&input)
    }

    /// Image or slideshow input `input`'s source and slide on air (IN-008,
    /// IN-009).
    #[must_use]
    pub fn still_input(&self, input: InputId) -> Option<&StillInputStatus> {
        self.still_inputs.get(&input)
    }

    /// Every image and slideshow input, ascending by input.
    #[must_use]
    pub const fn still_inputs(&self) -> &BTreeMap<InputId, StillInputStatus> {
        &self.still_inputs
    }

    /// Returns whether every `still_state` the snapshot announced arrived.
    #[must_use]
    pub const fn stills_complete(&self) -> bool {
        self.still_states_pending == 0
    }

    /// Returns every configured display-output slot (Protocol 2.42).
    #[must_use]
    pub const fn display_outputs(
        &self,
    ) -> &BTreeMap<fm_protocol::DisplayOutputSlot, fm_protocol::DisplayOutputConfig> {
        &self.display_outputs
    }

    /// Returns slot `slot`'s last reported runtime state; a configured slot
    /// nothing reported on waits for a presenter (Protocol 2.42).
    #[must_use]
    pub fn display_output_state(
        &self,
        slot: fm_protocol::DisplayOutputSlot,
    ) -> fm_protocol::WireDisplayOutputState {
        self.display_output_states.get(&slot).cloned().unwrap_or(
            fm_protocol::WireDisplayOutputState::Waiting(fm_protocol::WaitReason::NoPresenter),
        )
    }

    /// Returns the replicated strip labels and groups (Protocol 2.40).
    #[must_use]
    pub const fn audio_mixer(&self) -> &fm_protocol::AudioMixerStatus {
        &self.audio_mixer
    }

    /// Returns input `input`'s strip DSP chain (Protocol 2.75); the default
    /// chain for an input without one.
    #[must_use]
    pub fn input_audio_dsp(&self, input: InputId) -> fm_types::strip_dsp::StripDsp {
        self.audio_strip_dsp
            .get(&input)
            .copied()
            .unwrap_or(fm_types::strip_dsp::StripDsp::DEFAULT)
    }

    /// Returns the Master bus DSP chain (Protocol 2.75).
    #[must_use]
    pub const fn master_audio_dsp(&self) -> &fm_types::strip_dsp::StripDsp {
        &self.audio_mixer.master_dsp
    }

    /// Returns the DSP chain `target` names (Protocol 2.75).
    #[must_use]
    pub fn audio_dsp(
        &self,
        target: fm_types::strip_dsp::StripDspTarget,
    ) -> fm_types::strip_dsp::StripDsp {
        match target {
            fm_types::strip_dsp::StripDspTarget::Master => self.audio_mixer.master_dsp,
            fm_types::strip_dsp::StripDspTarget::Input(input) => self.input_audio_dsp(input),
        }
    }

    /// Returns whether every `audio_strip_dsp_state` the snapshot announced
    /// arrived.
    #[must_use]
    pub const fn audio_strip_dsps_complete(&self) -> bool {
        self.audio_strip_dsp_states_pending == 0
    }

    /// Returns the outputs Fade to Black leaves live, ascending.
    #[must_use]
    pub fn fade_to_black_excluded_outputs(&self) -> &[OutputId] {
        &self.fade_to_black_excluded_outputs
    }

    /// Returns the outputs with their own Fade-to-Black duration.
    #[must_use]
    pub fn fade_to_black_output_durations(&self) -> &[(OutputId, u32)] {
        &self.fade_to_black_output_durations
    }

    /// Returns how automatic Fade-to-Black moves ease.
    #[must_use]
    pub const fn fade_to_black_curve(&self) -> fm_protocol::FadeToBlackCurve {
        self.fade_to_black_curve
    }

    /// Returns input `input`'s replicated lifecycle (Protocol 2.38).
    #[must_use]
    pub fn input_lifecycle(&self, input: InputId) -> InputLifecycleStatus {
        self.input_lifecycle
            .get(&input)
            .copied()
            .unwrap_or_default()
    }

    /// Returns the project's input categories (Protocol 2.38).
    #[must_use]
    pub fn input_categories(&self) -> &[InputCategoryStatus] {
        &self.input_categories
    }

    /// Returns every media input's replicated status, ascending by input.
    /// Complete only once [`Self::media_complete`].
    #[must_use]
    pub const fn media(&self) -> &BTreeMap<InputId, MediaStatus> {
        &self.media
    }

    /// Returns media input `input`'s replicated status.
    #[must_use]
    pub fn media_status(&self, input: InputId) -> Option<&MediaStatus> {
        self.media.get(&input)
    }

    /// Returns whether every `media_state` the snapshot announced arrived.
    #[must_use]
    pub const fn media_complete(&self) -> bool {
        self.media_states_pending == 0
    }

    /// Returns every replicated cue list in project order (Protocol 2.50).
    #[must_use]
    pub fn cue_lists(&self) -> &[fm_protocol::CueListStatus] {
        &self.cue_lists
    }

    /// The newest engine frame a cue record reported (Protocol 2.64): the
    /// frame of the latest `cue_list_state` or `cue_run_changed`, `None`
    /// before either arrived.
    #[must_use]
    pub const fn cue_frame(&self) -> Option<u64> {
        self.cue_frame
    }

    /// Returns cue list `list`.
    #[must_use]
    pub fn cue_list(&self, list: u64) -> Option<&fm_protocol::CueListStatus> {
        self.cue_lists.iter().find(|candidate| candidate.id == list)
    }

    /// Returns whether every `cue_list_state` the snapshot announced
    /// arrived.
    #[must_use]
    pub const fn cue_lists_complete(&self) -> bool {
        self.cue_list_states_pending == 0
    }

    /// Returns every replicated script in project order (Protocol 2.58).
    #[must_use]
    pub fn scripts(&self) -> &[fm_protocol::ScriptStatus] {
        &self.scripts
    }

    /// Returns script `script`.
    #[must_use]
    pub fn script(&self, script: u64) -> Option<&fm_protocol::ScriptStatus> {
        self.scripts.iter().find(|candidate| candidate.id == script)
    }

    /// Returns whether every `script_state` the snapshot announced arrived.
    #[must_use]
    pub const fn scripts_complete(&self) -> bool {
        self.script_states_pending == 0
    }

    /// Returns every replicated user shortcut in project order (Protocol
    /// 2.70).
    #[must_use]
    pub fn shortcuts(&self) -> &[fm_protocol::ShortcutStatus] {
        &self.shortcuts
    }

    /// Returns user shortcut `shortcut`.
    #[must_use]
    pub fn shortcut(&self, shortcut: u64) -> Option<&fm_protocol::ShortcutStatus> {
        self.shortcuts
            .iter()
            .find(|candidate| candidate.id == shortcut)
    }

    /// Returns whether every `shortcut_state` the snapshot announced
    /// arrived.
    #[must_use]
    pub const fn shortcuts_complete(&self) -> bool {
        self.shortcut_states_pending == 0
    }

    /// Returns the last accepted fire of a shortcut's cue actions
    /// replicated since the snapshot.
    #[must_use]
    pub const fn last_shortcut_fire(&self) -> Option<shortcuts::ShortcutFireReport> {
        self.last_shortcut_fire
    }

    /// Returns every media list input's list (IN-006), by input.
    #[must_use]
    pub const fn media_lists(&self) -> &BTreeMap<InputId, fm_protocol::MediaListStatus> {
        &self.media_lists
    }

    /// Returns `input`'s media list when it is a media list input.
    #[must_use]
    pub fn media_list(&self, input: InputId) -> Option<&fm_protocol::MediaListStatus> {
        self.media_lists.get(&input)
    }

    /// Returns whether every `media_list_state` the snapshot announced
    /// arrived.
    #[must_use]
    pub const fn media_lists_complete(&self) -> bool {
        self.media_list_states_pending == 0
    }

    /// Returns the last accepted script run replicated since the snapshot,
    /// direct or by a cue's script step.
    #[must_use]
    pub fn last_script_run(&self) -> Option<ScriptRunReport> {
        self.script_runs.last().copied()
    }

    /// Returns script `script`'s run state (Protocol 2.62): waiting for its
    /// next part, faulted, or idle.
    #[must_use]
    pub fn script_run_status(&self, script: u64) -> fm_protocol::ScriptRunStatus {
        self.script_run_states
            .get(&script)
            .cloned()
            .unwrap_or_default()
    }

    /// Returns every script whose run waits or faulted, with its state, in
    /// run order (Protocol 2.69); empty while every script is idle.
    #[must_use]
    pub fn active_script_runs(&self) -> Vec<(u64, fm_protocol::ScriptRunStatus)> {
        let mut runs: Vec<_> = self
            .script_run_states
            .iter()
            .map(|(script, run)| (*script, run.clone()))
            .collect();
        runs.sort_by_key(|(_, run)| scripts::run_id(run));
        runs
    }

    /// Returns the last accepted run of script `script` replicated since
    /// the snapshot, direct or by a cue's script step (Protocol 2.61).
    #[must_use]
    pub fn script_run(&self, script: u64) -> Option<ScriptRunReport> {
        self.script_runs
            .iter()
            .find(|run| run.script == script)
            .copied()
    }

    /// Returns the replicated project grading. Complete only once
    /// [`Self::grading_complete`].
    #[must_use]
    pub const fn grading(&self) -> &GradingStatus {
        &self.grading
    }

    /// Returns whether every `grade_state` the snapshot announced arrived.
    #[must_use]
    pub const fn grading_complete(&self) -> bool {
        self.grade_states_pending == 0
    }

    /// Returns the replicated animation preset library. Complete only once
    /// [`Self::animation_presets_complete`].
    #[must_use]
    pub const fn animation_presets(&self) -> &AnimationPresetsStatus {
        &self.animation_presets
    }

    /// Returns whether every `animation_preset_state` the snapshot
    /// announced arrived.
    #[must_use]
    pub const fn animation_presets_complete(&self) -> bool {
        self.animation_preset_states_pending == 0
    }

    /// Returns the replicated playlist: the snapshot's (its `playlist_state`,
    /// or the empty playlist it implies) kept current by `playlist_changed`.
    /// `None` only while an announced `playlist_state` has not arrived.
    #[must_use]
    pub const fn playlist(&self) -> Option<&PlaylistStatus> {
        self.playlist.as_ref()
    }

    /// Returns whether the snapshot's playlist is known (it announced none,
    /// or its `playlist_state` arrived).
    #[must_use]
    pub const fn playlist_complete(&self) -> bool {
        self.playlist.is_some()
    }

    /// Returns the engine's video frame rate from the snapshot.
    #[must_use]
    pub const fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }

    /// Returns every replicated title input, ascending by input.
    #[must_use]
    pub fn titles(&self) -> &[TitleStatus] {
        &self.titles
    }

    #[must_use]
    pub fn title(&self, input: InputId) -> Option<&TitleStatus> {
        self.titles.iter().find(|title| title.input == input)
    }

    /// Returns whether every title the snapshot announced has its values.
    #[must_use]
    pub fn titles_complete(&self) -> bool {
        self.titles.iter().all(|title| title.complete)
    }

    /// Returns whether every title the snapshot announced has its
    /// `title_animation_state`.
    #[must_use]
    pub fn title_animations_complete(&self) -> bool {
        self.titles.iter().all(|title| title.animation.is_some())
    }

    /// Returns whether every title the snapshot announced has its
    /// `title_layout_state`.
    #[must_use]
    pub fn title_layouts_complete(&self) -> bool {
        self.titles.iter().all(|title| title.layout.is_some())
    }

    /// Returns every replicated Scene input, ascending by input.
    #[must_use]
    pub fn scenes(&self) -> &[SceneStatus] {
        &self.scenes
    }

    #[must_use]
    pub fn scene(&self, input: InputId) -> Option<&SceneStatus> {
        self.scenes.iter().find(|scene| scene.input == input)
    }

    /// Returns whether every Scene input the snapshot announced has its
    /// `scene_animation_state`.
    #[must_use]
    pub fn scene_animations_complete(&self) -> bool {
        self.scenes.iter().all(|scene| scene.animation.is_some())
    }

    /// Returns whether every scene the snapshot announced has its
    /// `scene_layout_state`, and so every Scene input its layout.
    #[must_use]
    pub fn scene_layouts_complete(&self) -> bool {
        self.scene_layouts
            .iter()
            .all(|scene| scene.layout.is_some())
            && self.scenes.iter().all(|scene| scene.layout.is_some())
    }

    /// Every replicated project scene, ascending by scene (Protocol 2.35).
    #[must_use]
    pub fn scene_layouts(&self) -> &[SceneLayoutStatus] {
        &self.scene_layouts
    }

    /// One replicated project scene.
    #[must_use]
    pub fn scene_layout(&self, scene: SceneId) -> Option<&SceneLayoutStatus> {
        self.scene_layouts
            .iter()
            .find(|candidate| candidate.scene == scene)
    }

    /// Returns whether `input` is on air in the replicated desired state: on
    /// desired Program, an endpoint of the desired manual transition, or the
    /// source of a desired overlay that is shown or still fading.
    ///
    /// The server's air rule also spans the realized switcher, so a title
    /// still fading out on air may be refused even after this turns false.
    #[must_use]
    pub fn title_on_air(&self, input: InputId) -> bool {
        let manual = match self.switcher.desired_manual_transition {
            ManualTransitionStatus::Active(transition) => {
                transition.from == input || transition.to == input
            }
            ManualTransitionStatus::Inactive => false,
        };
        self.switcher.desired.program == input
            || manual
            || self.desired_overlays.iter().any(|overlay| {
                overlay.source == Some(input) && (overlay.active || overlay.opacity > 0)
            })
    }

    /// Returns the replicated engaged edit padlocks.
    #[must_use]
    pub const fn edit_locks(&self) -> &EditLocks {
        &self.edit_locks
    }

    /// Returns the server's undo/redo summary at the applied cursor.
    #[must_use]
    pub const fn edit_history(&self) -> EditHistoryStatus {
        self.edit_history
    }

    #[must_use]
    pub fn show_name(&self) -> &str {
        &self.show_name
    }

    #[must_use]
    pub fn inputs(&self) -> &[InputId] {
        &self.inputs
    }

    #[must_use]
    pub fn input_name(&self, input: InputId) -> Option<&str> {
        self.inputs
            .iter()
            .position(|candidate| *candidate == input)
            .map(|index| self.input_names[index].as_str())
    }

    #[must_use]
    pub fn input_audio_strips(&self) -> &[InputAudioStripStatus] {
        &self.input_audio_strips
    }

    #[must_use]
    pub fn outputs(&self) -> &[OutputStatus] {
        &self.outputs
    }

    #[must_use]
    pub fn stingers(&self) -> &[StingerStatus] {
        &self.stingers
    }

    #[must_use]
    pub fn streams(&self) -> &[StreamStatus] {
        &self.streams
    }

    /// Every configured stream destination in authored order, without its
    /// key (Protocol 2.52).
    #[must_use]
    pub fn stream_targets(&self) -> &[fm_protocol::StreamTargetEntry] {
        &self.stream_targets
    }

    /// Returns the latest replicated two-slot recorder roster in canonical order.
    #[must_use]
    pub fn recordings(&self) -> &[RecordingStatus] {
        &self.recordings
    }

    /// Returns the replicated engine-owned recording-desired flag for one slot.
    ///
    /// Returns `false` when the slot is absent from the validated roster.
    #[must_use]
    pub fn recording_desired(&self, recorder: WireRecorderSlot) -> bool {
        self.recordings
            .iter()
            .find(|status| status.recorder == recorder)
            .is_some_and(|status| status.desired_active)
    }

    #[must_use]
    pub fn desired_overlays(&self) -> &[OverlayStatus] {
        &self.desired_overlays
    }

    #[must_use]
    pub fn realized_overlays(&self) -> &[OverlayStatus] {
        &self.realized_overlays
    }

    #[must_use]
    pub const fn switcher(&self) -> SwitcherState {
        self.switcher
    }
}

/// An immutable render snapshot, including outstanding optimistic intent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientView {
    pub cursor: ProjectCursor,
    pub show_name: String,
    pub inputs: Vec<InputId>,
    pub input_names: Vec<String>,
    pub outputs: Vec<OutputStatus>,
    pub input_audio_strips: Vec<InputAudioStripStatus>,
    pub stingers: Vec<StingerStatus>,
    pub streams: Vec<StreamStatus>,
    pub recordings: Vec<RecordingStatus>,
    pub desired_overlays: Vec<OverlayStatus>,
    pub realized_overlays: Vec<OverlayStatus>,
    pub switcher: SwitcherState,
    pub edit_locks: EditLocks,
    pub edit_history: EditHistoryStatus,
    /// The favourite transition buttons, slot 1 first (Protocol 2.36).
    pub transition_favorites: [TransitionFavoriteStatus; TRANSITION_FAVORITE_COUNT],
    /// Each input's lifecycle, parallel to `inputs` (Protocol 2.38).
    pub input_lifecycle: Vec<InputLifecycleStatus>,
    /// The project's input categories (Protocol 2.38).
    pub input_categories: Vec<InputCategoryStatus>,
    /// Strip labels and groups (Protocol 2.40).
    pub audio_mixer: fm_protocol::AudioMixerStatus,
    /// The outputs Fade to Black leaves live, ascending (Protocol 2.40).
    pub fade_to_black_excluded_outputs: Vec<OutputId>,
    /// The outputs that fade over their own duration in frames, ascending
    /// by output (Protocol 2.48).
    pub fade_to_black_output_durations: Vec<(OutputId, u32)>,
    /// How automatic Fade-to-Black moves ease (Protocol 2.48).
    pub fade_to_black_curve: fm_protocol::FadeToBlackCurve,
    /// Every configured display-output slot, ascending (Protocol 2.42).
    pub display_outputs: Vec<fm_protocol::DisplayOutputEntry>,
    /// Every configured slot's runtime state, ascending (Protocol 2.42).
    pub display_output_states: Vec<fm_protocol::DisplayOutputStatus>,
    /// The connected displays as the daemon last reported them (Protocol
    /// 2.42); `None` until a `display_inventory` arrives.
    pub display_inventory: Option<fm_protocol::DisplayInventory>,
    /// Every network stream input, ascending, with its redacted config and
    /// latest status (Protocol 2.46).
    pub network_streams: Vec<fm_protocol::NetworkStreamEntry>,
    /// Every capture input, ascending, with its stable key, settings, and
    /// latest status (Protocol 2.47).
    pub capture_inputs: Vec<fm_protocol::CaptureInputEntry>,
    /// The capture devices as the daemon last reported them (Protocol
    /// 2.47); `None` until a `capture_devices` reply or event arrives.
    pub capture_inventory: Option<fm_protocol::CaptureInventory>,
    /// Every system audio input, ascending, with its device and latest
    /// status (Protocol 2.55).
    pub system_audio_inputs: Vec<fm_protocol::SystemAudioEntry>,
    /// The server's system audio capture devices as it last listed them
    /// (Protocol 2.68); `None` until a `system_audio_devices` reply
    /// arrives.
    pub system_audio_inventory: Option<fm_protocol::SystemAudioInventory>,
    /// The web gateway's pairing and paired devices as the daemon last
    /// answered a `web_access_request` (Protocol 2.71); `None` until then.
    pub web_access: Option<fm_protocol::WebAccessMessage>,
    /// Every image and slideshow input's source, slide on air, and fetched
    /// listing, ascending by input (IN-008, IN-009).
    pub still_inputs: BTreeMap<InputId, StillInputStatus>,
    /// Every configured stream destination in authored order, without its
    /// key (Protocol 2.52).
    pub stream_targets: Vec<fm_protocol::StreamTargetEntry>,
    /// The `LiveLAN` configuration, without its PIN (Protocol 2.57).
    pub live_lan: fm_protocol::LiveLanEntry,
    /// The `LiveLAN` stream's status (Protocol 2.57).
    pub live_lan_status: fm_protocol::LiveLanStatus,
}

impl ClientView {
    /// Stream destination `target`'s configuration, if it is configured
    /// (Protocol 2.52).
    #[must_use]
    pub fn stream_target(&self, target: NonZeroU128) -> Option<&fm_protocol::StreamTargetEntry> {
        self.stream_targets
            .iter()
            .find(|entry| entry.target.get() == target)
    }

    /// Capture input `input`'s stable key, settings, and latest status, if
    /// it is a capture input (Protocol 2.47).
    #[must_use]
    pub fn capture_input(&self, input: InputId) -> Option<&fm_protocol::CaptureInputEntry> {
        self.capture_inputs
            .iter()
            .find(|entry| entry.input.to_domain() == input)
    }

    /// Network stream input `input`'s redacted config and latest status,
    /// if it is a network stream (Protocol 2.46).
    #[must_use]
    pub fn network_stream(&self, input: InputId) -> Option<&fm_protocol::NetworkStreamEntry> {
        self.network_streams
            .iter()
            .find(|entry| entry.input.to_domain() == input)
    }

    /// Image or slideshow input `input`'s source, slide on air, and fetched
    /// listing (IN-008, IN-009).
    #[must_use]
    pub fn still_input(&self, input: InputId) -> Option<&StillInputStatus> {
        self.still_inputs.get(&input)
    }

    /// System audio input `input`'s device and latest status (Protocol
    /// 2.55).
    #[must_use]
    pub fn system_audio_input(&self, input: InputId) -> Option<&fm_protocol::SystemAudioEntry> {
        self.system_audio_inputs
            .iter()
            .find(|entry| entry.input.to_domain() == input)
    }

    /// Slot `slot`'s configuration, if it is configured.
    #[must_use]
    pub fn display_output(
        &self,
        slot: fm_protocol::DisplayOutputSlot,
    ) -> Option<&fm_protocol::DisplayOutputConfig> {
        self.display_outputs
            .iter()
            .find(|entry| entry.slot == slot)
            .map(|entry| &entry.config)
    }

    /// Slot `slot`'s runtime state, if it is configured.
    #[must_use]
    pub fn display_output_state(
        &self,
        slot: fm_protocol::DisplayOutputSlot,
    ) -> Option<&fm_protocol::WireDisplayOutputState> {
        self.display_output_states
            .iter()
            .find(|status| status.slot == slot)
            .map(|status| &status.state)
    }

    /// The operator label of `input`'s strip; empty when unlabelled.
    #[must_use]
    pub fn audio_label(&self, input: InputId) -> &str {
        self.audio_mixer
            .label(fm_protocol::WireInputId::from_domain(input))
    }

    /// Whether Fade to Black fades `output`.
    #[must_use]
    pub fn fade_to_black_affects(&self, output: OutputId) -> bool {
        !self.fade_to_black_excluded_outputs.contains(&output)
    }

    /// The Fade-to-Black duration `output` has of its own, if any.
    #[must_use]
    pub fn fade_to_black_output_duration(&self, output: OutputId) -> Option<u32> {
        self.fade_to_black_output_durations
            .iter()
            .find(|(candidate, _)| *candidate == output)
            .map(|(_, frames)| *frames)
    }

    /// Returns input `input`'s lifecycle, or the default for an unknown
    /// input.
    #[must_use]
    pub fn lifecycle(&self, input: InputId) -> InputLifecycleStatus {
        self.inputs
            .iter()
            .position(|candidate| *candidate == input)
            .and_then(|index| self.input_lifecycle.get(index).copied())
            .unwrap_or_default()
    }

    /// Returns the replicated engine-owned recording-desired flag for one slot.
    ///
    /// Returns `false` when the slot is absent from the validated roster.
    #[must_use]
    pub fn recording_desired(&self, recorder: WireRecorderSlot) -> bool {
        self.recordings
            .iter()
            .find(|status| status.recorder == recorder)
            .is_some_and(|status| status.desired_active)
    }
}

/// A local change displayed while its command is outstanding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptimisticChange {
    DesiredSwitcher(BusSelection),
    DesiredProgram(InputId),
    DesiredPreview(InputId),
}

impl OptimisticChange {
    fn apply(self, selection: &mut BusSelection) {
        match self {
            Self::DesiredSwitcher(value) => *selection = value,
            Self::DesiredProgram(value) => selection.program = value,
            Self::DesiredPreview(value) => selection.preview = value,
        }
    }

    fn inputs(self) -> [Option<InputId>; 2] {
        match self {
            Self::DesiredSwitcher(value) => [Some(value.program), Some(value.preview)],
            Self::DesiredProgram(value) | Self::DesiredPreview(value) => [Some(value), None],
        }
    }
}

/// The state of an outstanding command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingStatus {
    AwaitingResult,
    AwaitingEvent { accepted_revision: Revision },
}

/// A command and its optional optimistic UI change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingCommand {
    pub id: CommandId,
    pub optimistic: Option<OptimisticChange>,
    pub status: PendingStatus,
}

/// Stable rejection details retained for operator feedback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectedCommand {
    pub id: CommandId,
    pub code: String,
    pub message: String,
    pub fields: Vec<FieldIssue>,
    pub current_revision: Revision,
    pub retryable: bool,
    /// The typed refusal reason (Protocol 2.45), worded by
    /// [`crate::i18n::refusal_reason_text`].
    pub reason: Option<fm_protocol::RefusalReason>,
}

/// Whether the local cursor can be treated as current.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncStatus {
    AwaitingSnapshot,
    Current,
    Behind {
        local_revision: Revision,
        known_revision: Revision,
    },
    RequiresSnapshot,
}

impl SyncStatus {
    #[must_use]
    pub const fn is_stale(&self) -> bool {
        !matches!(self, Self::Current)
    }
}

/// Result of installing an authoritative snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotInstalled {
    pub reconciled_commands: Vec<CommandId>,
    pub discarded_commands: Vec<CommandId>,
}

/// Result of reducing a durable event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventApplied {
    Applied { reconciled_commands: Vec<CommandId> },
    Duplicate,
}

/// Result of reducing an independently ordered runtime realization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeRealizationApplied {
    Applied,
    Duplicate,
}

/// Result of reducing a command receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandReconciled {
    Accepted {
        revision: Revision,
        awaiting_event: bool,
    },
    Rejected(RejectedCommand),
}

/// A reducer input for callers that prefer one dispatch surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    InstallSnapshot(Box<ProjectSnapshot>),
    ApplyEvent(DurableProjectEvent),
    ApplyRuntimeRealization(RuntimeRealization),
    TrackCommand {
        id: CommandId,
        optimistic: Option<OptimisticChange>,
    },
    ReconcileCommand(CommandResult),
    ObserveServer(ServerHello),
    ApplyTitleState(TitleStateMessage),
    ApplyPlaylistState(PlaylistStateMessage),
}

/// The effect produced by [`ClientModel::reduce`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reduction {
    SnapshotInstalled(SnapshotInstalled),
    EventApplied(EventApplied),
    RuntimeRealizationApplied(RuntimeRealizationApplied),
    CommandTracked,
    CommandReconciled(CommandReconciled),
    ServerObserved,
    TitleStateApplied,
    PlaylistStateApplied,
}

/// Validation or ordering failure that leaves authoritative state unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelError {
    ProjectMismatch {
        expected: ProjectId,
        observed: ProjectId,
    },
    ProtocolProjectMismatch {
        expected: ProjectId,
        observed: String,
    },
    EngineMismatch {
        expected: EngineIdentity,
        observed: EngineIdentity,
    },
    InvalidEngineIdentity,
    SnapshotRequired,
    RevisionGap {
        expected: Revision,
        observed: Revision,
    },
    OutOfOrder {
        current: Revision,
        observed: Revision,
    },
    ConflictingDuplicate {
        revision: Revision,
    },
    UnknownDurableRevision {
        revision: Revision,
    },
    RuntimeOutOfOrder {
        current_sequence: u64,
        observed_sequence: u64,
    },
    RuntimeGenerationOutOfOrder {
        current_generation: u64,
        observed_generation: u64,
    },
    ConflictingRuntimeSequence {
        sequence: u64,
    },
    RevisionExhausted,
    DuplicateInput(InputId),
    InvalidInputNames,
    DuplicateOutput(OutputId),
    InvalidOutputNames,
    InvalidInputOrder,
    InvalidInputAudioStrips,
    DuplicateStingerSlot(u8),
    InvalidStingerSlot(u8),
    InvalidStreamCount(usize),
    DuplicateStreamTarget(NonZeroU128),
    InvalidStreamNames,
    InvalidRecordingCount(usize),
    DuplicateRecorderSlot(u8),
    InvalidRecordingOrder,
    InvalidOverlayCount(usize),
    InvalidOverlayChannel(u8),
    InvalidOverlayTransitionDuration {
        channel: u8,
        duration_frames: u32,
    },
    ActiveOverlayMissingSource(u8),
    InvalidOverlayQueueDepth {
        channel: u8,
        depth: usize,
    },
    DuplicateOverlayOutput(u8),
    UnknownInput(InputId),
    UnknownOutput(OutputId),
    InvalidManualTransitionRouting,
    DuplicateCommand(CommandId),
    UnknownCommand(CommandId),
    StateUnavailable,
    InvalidEditLocks,
    /// A removed input is still on air or configured in the desired state.
    InputInUse(InputId),
    /// Title summaries are not unique show inputs in ascending order.
    InvalidTitles,
    /// The input is not a replicated title.
    UnknownTitle(InputId),
    /// A `title_state` does not complete the snapshot's announced summary
    /// at the applied revision.
    TitleStateMismatch(InputId),
    /// A title change arrived before the title's `title_state`.
    TitleIncomplete(InputId),
    /// A `playlist_state` arrived that the applied snapshot did not announce
    /// or still awaits at another revision.
    PlaylistStateMismatch,
    /// A `grade_state` arrived that the applied snapshot did not announce,
    /// at another revision, or out of order.
    GradeStateMismatch,
    /// A `grade_changed` does not apply to the replicated grading.
    InvalidGradeChange,
    /// An `animation_preset_state` arrived that the applied snapshot did
    /// not announce, at another revision, or duplicating a preset.
    AnimationPresetStateMismatch,
    /// An `animation_preset_changed` does not apply to the replicated
    /// library.
    InvalidAnimationPresetChange,
    /// A `transition_favorite_changed` names no favourite slot.
    InvalidTransitionFavorite(u8),
    /// A `media_state` arrived that the applied snapshot did not announce,
    /// at another revision, out of input order, or for an unknown input.
    MediaStateMismatch,
    /// A `still_state` arrived that the applied snapshot did not announce,
    /// at another revision, out of input order, or for an unknown input.
    StillStateMismatch,
    /// A media status (`media_changed` or a `media` runtime event) names an
    /// input outside the replicated project.
    InvalidMediaChange(InputId),
    /// An input lifecycle change (Protocol 2.38) or snapshot lifecycle does
    /// not match the replicated project.
    InvalidInputLifecycleChange,
    /// A title change's fields are not the title's field set, or its field
    /// revision went backwards.
    InvalidTitleChange(InputId),
    /// Scene inputs are not unique show inputs in ascending order, or a
    /// `scene_animation_state` does not complete the snapshot's announced
    /// Scene input at the applied revision.
    SceneStateMismatch(InputId),
    /// A `scene_layout_state` does not complete the snapshot's announced
    /// scene at the applied revision, or arrived before every
    /// `scene_animation_state` (Protocol 2.35).
    SceneLayoutMismatch(SceneId),
    /// A scene animation change names no, unordered, unknown, incomplete,
    /// or stale Scene inputs.
    InvalidSceneChange(InputId),
    /// A mixer or Fade-to-Black output change names an unknown input,
    /// group, or output, or carries an unusable label (Protocol 2.40).
    InvalidAudioMixerChange,
    /// A display-output change shows an unknown output (Protocol 2.42).
    InvalidDisplayOutputChange,
    /// A `LiveLAN` change streams an unknown output (Protocol 2.57).
    InvalidLiveLanChange,
    /// A `cue_list_state` arrived that the snapshot did not announce or at
    /// another revision (Protocol 2.50).
    CueListStateMismatch,
    /// A cue list change names an unknown list or cue, or does not apply.
    InvalidCueList(u64),
    /// A `script_state` arrived that the snapshot did not announce or at
    /// another revision (Protocol 2.58).
    ScriptStateMismatch,
    /// A `shortcut_state` arrived that the snapshot did not announce or at
    /// another revision (Protocol 2.72).
    ShortcutStateMismatch,
    /// A shortcut change names an unknown shortcut or does not fit its
    /// bounds.
    InvalidShortcut(u64),
    /// A script change names an unknown script or does not fit its bounds.
    InvalidScript(u64),
    /// A `media_list_state` arrived that the snapshot did not announce, at
    /// another revision, or for an unknown or repeated input (IN-006).
    MediaListStateMismatch,
    /// A media list change names an unknown list input or is incoherent.
    InvalidMediaListChange(InputId),
    /// An `audio_strip_dsp_state` arrived that the snapshot did not
    /// announce, at another revision, out of order, for an unknown input, or
    /// with a default or invalid chain (Protocol 2.75).
    AudioStripDspStateMismatch,
}

impl fmt::Display for ModelError {
    #[allow(clippy::too_many_lines)] // one arm per error
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProjectMismatch { expected, observed } => write!(
                formatter,
                "project identity changed from {expected} to {observed}"
            ),
            Self::ProtocolProjectMismatch { expected, observed } => write!(
                formatter,
                "protocol project identity changed from {expected} to {observed}"
            ),
            Self::EngineMismatch { .. } => formatter.write_str("engine cursor identity changed"),
            Self::InvalidEngineIdentity => formatter.write_str("engine identity is incomplete"),
            Self::SnapshotRequired => formatter.write_str("a fresh snapshot is required"),
            Self::RevisionGap { expected, observed } => write!(
                formatter,
                "event gap: expected revision {expected}, got {observed}"
            ),
            Self::OutOfOrder { current, observed } => write!(
                formatter,
                "out-of-order event revision {observed}; current revision is {current}"
            ),
            Self::ConflictingDuplicate { revision } => {
                write!(formatter, "revision {revision} has conflicting event data")
            }
            Self::UnknownDurableRevision { revision } => {
                write!(
                    formatter,
                    "runtime realization references unknown revision {revision}"
                )
            }
            Self::RuntimeOutOfOrder {
                current_sequence,
                observed_sequence,
            } => write!(
                formatter,
                "out-of-order runtime sequence {observed_sequence}; current sequence is {current_sequence}"
            ),
            Self::RuntimeGenerationOutOfOrder {
                current_generation,
                observed_generation,
            } => write!(
                formatter,
                "out-of-order runtime generation {observed_generation}; current generation is {current_generation}"
            ),
            Self::ConflictingRuntimeSequence { sequence } => write!(
                formatter,
                "runtime sequence {sequence} has conflicting realization data"
            ),
            Self::RevisionExhausted => formatter.write_str("revision counter is exhausted"),
            Self::DuplicateInput(input) => write!(formatter, "snapshot repeats input {input}"),
            Self::InvalidInputOrder => formatter.write_str("input order does not match the current input set"),
            Self::InvalidInputNames => formatter.write_str(
                "input names must contain one unique, nonblank label within the byte limit for each show input",
            ),
            Self::DuplicateOutput(output) => write!(formatter, "snapshot repeats output {output}"),
            Self::InvalidOutputNames => formatter.write_str(
                "output names must contain one unique, nonblank label for each output",
            ),
            Self::InvalidInputAudioStrips => formatter.write_str(
                "input audio strips must contain each show input exactly once with gain in -96000..=24000 millidB, balance in -10000..=10000 basis points, and delay in 0..=48000 samples",
            ),
            Self::DuplicateStingerSlot(slot) => {
                write!(formatter, "snapshot repeats Stinger slot {slot}")
            }
            Self::InvalidStingerSlot(slot) => write!(formatter, "invalid Stinger slot {slot}"),
            Self::InvalidStreamCount(count) => write!(
                formatter,
                "snapshot contains {count} stream statuses; maximum is {MAX_STREAM_STATUSES}"
            ),
            Self::DuplicateStreamTarget(target) => {
                write!(formatter, "snapshot repeats stream target {}", target.get())
            }
            Self::InvalidStreamNames => formatter.write_str(
                "stream names must contain one unique, nonblank label within the byte limit",
            ),
            Self::InvalidRecordingCount(_)
            | Self::DuplicateRecorderSlot(_)
            | Self::InvalidRecordingOrder => write_recording_message(self, formatter)
                .expect("recording errors always format"),
            Self::InvalidOverlayCount(_)
            | Self::InvalidOverlayChannel(_)
            | Self::InvalidOverlayTransitionDuration { .. }
            | Self::ActiveOverlayMissingSource(_)
            | Self::InvalidOverlayQueueDepth { .. }
            | Self::DuplicateOverlayOutput(_) => {
                write_overlay_message(self, formatter).expect("overlay errors always format")
            }
            Self::UnknownInput(input) => write!(formatter, "unknown input {input}"),
            Self::UnknownOutput(output) => write!(formatter, "unknown output {output}"),
            Self::DuplicateCommand(id) => write!(formatter, "command {id} is already pending"),
            Self::UnknownCommand(id) => write!(formatter, "command {id} is not pending"),
            Self::StateUnavailable => formatter.write_str("no project snapshot is installed"),
            Self::InvalidManualTransitionRouting
            | Self::InvalidEditLocks
            | Self::InputInUse(_)
            | Self::InvalidTitles
            | Self::UnknownTitle(_)
            | Self::TitleStateMismatch(_)
            | Self::TitleIncomplete(_)
            | Self::PlaylistStateMismatch
            | Self::GradeStateMismatch
            | Self::InvalidGradeChange
            | Self::AnimationPresetStateMismatch
            | Self::InvalidAnimationPresetChange
            | Self::MediaStateMismatch
            | Self::StillStateMismatch
            | Self::InvalidMediaChange(_)
            | Self::InvalidInputLifecycleChange
            | Self::InvalidTitleChange(_) => write_catalogue_message(self, formatter),
            Self::InvalidTransitionFavorite(slot) => write!(
                formatter,
                "favourite transition change names slot {slot}, not 1 through \
                 {TRANSITION_FAVORITE_COUNT}"
            ),
            Self::SceneStateMismatch(input) => write!(
                formatter,
                "scene state for input {input} does not match the snapshot it follows"
            ),
            Self::SceneLayoutMismatch(scene) => write!(
                formatter,
                "scene layout for scene {scene} does not match the snapshot it follows"
            ),
            Self::InvalidSceneChange(input) => {
                write!(formatter, "scene {input} animation change does not apply")
            }
            Self::InvalidAudioMixerChange => {
                formatter.write_str("mixer or Fade to Black output change does not apply")
            }
            Self::InvalidDisplayOutputChange => {
                formatter.write_str("display-output change shows an unknown output")
            }
            Self::InvalidLiveLanChange => {
                formatter.write_str("LiveLAN change streams an unknown output")
            }
            Self::CueListStateMismatch => {
                formatter.write_str("cue list state does not match the snapshot it follows")
            }
            Self::InvalidCueList(list) => {
                write!(formatter, "cue list {list} change does not apply")
            }
            Self::ShortcutStateMismatch => {
                formatter.write_str("shortcut state does not match the snapshot it follows")
            }
            Self::InvalidShortcut(shortcut) => {
                write!(formatter, "shortcut {shortcut} change does not apply")
            }
            Self::ScriptStateMismatch => {
                formatter.write_str("script state does not match the snapshot it follows")
            }
            Self::InvalidScript(script) => {
                write!(formatter, "script {script} change does not apply")
            }
            Self::MediaListStateMismatch => {
                formatter.write_str("media list state does not match the snapshot it follows")
            }
            Self::InvalidMediaListChange(input) => {
                write!(formatter, "media list change for input {input} does not apply")
            }
            Self::AudioStripDspStateMismatch => formatter
                .write_str("audio strip DSP state does not match the snapshot it follows"),
        }
    }
}

impl std::error::Error for ModelError {}

/// Formats the routing, edit-lock, input-catalogue and title errors.
fn write_catalogue_message(error: &ModelError, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match error {
        ModelError::InvalidManualTransitionRouting => formatter
            .write_str("manual transition endpoints do not match Program and Preview routing"),
        ModelError::InvalidEditLocks => formatter
            .write_str("locked inputs must be unique show inputs in ascending identifier order"),
        ModelError::InputInUse(input) => {
            write!(formatter, "removed input {input} is still in use")
        }
        ModelError::InvalidTitles => {
            formatter.write_str("titles must be unique show inputs in ascending identifier order")
        }
        ModelError::UnknownTitle(input) => write!(formatter, "input {input} is not a title"),
        ModelError::TitleStateMismatch(input) => write!(
            formatter,
            "title state for input {input} does not match the snapshot it follows"
        ),
        ModelError::TitleIncomplete(input) => {
            write!(formatter, "title {input} changed before its state arrived")
        }
        ModelError::InvalidTitleChange(input) => {
            write!(formatter, "title {input} change does not match its fields")
        }
        ModelError::PlaylistStateMismatch => {
            formatter.write_str("playlist state does not match the snapshot it follows")
        }
        ModelError::GradeStateMismatch => {
            formatter.write_str("grade state does not match the snapshot it follows")
        }
        ModelError::InvalidGradeChange => {
            formatter.write_str("grade change does not match the replicated grading")
        }
        ModelError::AnimationPresetStateMismatch => {
            formatter.write_str("animation preset state does not match the snapshot it follows")
        }
        ModelError::InvalidAnimationPresetChange => formatter
            .write_str("animation preset change does not match the replicated preset library"),
        ModelError::MediaStateMismatch => {
            formatter.write_str("media state does not match the snapshot it follows")
        }
        ModelError::StillStateMismatch => {
            formatter.write_str("still state does not match the snapshot it follows")
        }
        ModelError::InvalidMediaChange(input) => {
            write!(formatter, "media status names unknown input {input}")
        }
        ModelError::InvalidInputLifecycleChange => {
            formatter.write_str("input lifecycle change does not match the replicated project")
        }
        _ => Ok(()),
    }
}

fn write_overlay_message(
    error: &ModelError,
    formatter: &mut fmt::Formatter<'_>,
) -> Option<fmt::Result> {
    match error {
        ModelError::InvalidOverlayCount(count) => Some(write!(
            formatter,
            "snapshot contains {count} overlay channels; expected 8"
        )),
        ModelError::InvalidOverlayChannel(channel) => Some(write!(
            formatter,
            "invalid or duplicate overlay channel {channel}"
        )),
        ModelError::InvalidOverlayTransitionDuration {
            channel,
            duration_frames,
        } => Some(write!(
            formatter,
            "overlay channel {channel} transition duration {duration_frames} is outside 1..=3600 frames"
        )),
        ModelError::ActiveOverlayMissingSource(channel) => Some(write!(
            formatter,
            "active overlay channel {channel} has no source"
        )),
        ModelError::InvalidOverlayQueueDepth { channel, depth } => Some(write!(
            formatter,
            "overlay channel {channel} queue depth {depth} exceeds 64"
        )),
        ModelError::DuplicateOverlayOutput(channel) => Some(write!(
            formatter,
            "overlay channel {channel} repeats an output"
        )),
        _ => None,
    }
}

fn write_recording_message(
    error: &ModelError,
    formatter: &mut fmt::Formatter<'_>,
) -> Option<fmt::Result> {
    match error {
        ModelError::InvalidRecordingCount(count) => Some(write!(
            formatter,
            "snapshot contains {count} recorder statuses; expected {RECORDER_SLOT_COUNT}"
        )),
        ModelError::DuplicateRecorderSlot(slot) => {
            Some(write!(formatter, "snapshot repeats recorder slot {slot}"))
        }
        ModelError::InvalidRecordingOrder => {
            Some(formatter.write_str("recorder statuses must be in canonical slot order"))
        }
        _ => None,
    }
}

/// Project-bound replicated client state and reducer.
#[derive(Clone, Debug)]
pub struct ClientModel {
    project_id: ProjectId,
    cursor: Option<ProjectCursor>,
    expected_engine: Option<EngineIdentity>,
    state: Option<ProjectState>,
    applied_events: HashMap<Revision, DurableProjectEvent>,
    desired_by_revision: HashMap<
        Revision,
        (
            BusSelection,
            ManualTransitionStatus,
            FadeToBlackState,
            Vec<OverlayStatus>,
        ),
    >,
    last_runtime_realization: Option<RuntimeRealization>,
    pending: Vec<PendingCommand>,
    last_rejection: Option<RejectedCommand>,
    sync_status: SyncStatus,
    known_revision: Option<Revision>,
    /// The connected displays as last reported (Protocol 2.42). Runtime
    /// state that survives resynchronization.
    display_inventory: Option<fm_protocol::DisplayInventory>,
    /// The capture devices as last reported (Protocol 2.47). Runtime state
    /// that survives resynchronization.
    capture_inventory: Option<fm_protocol::CaptureInventory>,
    /// The system audio capture devices as last listed (Protocol 2.68).
    /// Runtime state that survives resynchronization.
    system_audio_inventory: Option<fm_protocol::SystemAudioInventory>,
    /// The latest web access answer (Protocol 2.71). Runtime state that
    /// survives resynchronization.
    web_access: Option<fm_protocol::WebAccessMessage>,
}

impl ClientModel {
    #[must_use]
    pub fn new(project_id: ProjectId) -> Self {
        Self {
            project_id,
            cursor: None,
            expected_engine: None,
            state: None,
            applied_events: HashMap::new(),
            desired_by_revision: HashMap::new(),
            last_runtime_realization: None,
            pending: Vec::new(),
            last_rejection: None,
            sync_status: SyncStatus::AwaitingSnapshot,
            known_revision: None,
            display_inventory: None,
            capture_inventory: None,
            system_audio_inventory: None,
            web_access: None,
        }
    }

    /// Records the latest `web_access` answer (Protocol 2.71).
    pub fn apply_web_access(&mut self, message: fm_protocol::WebAccessMessage) {
        self.web_access = Some(message);
    }

    /// Returns the latest web access answer (Protocol 2.71).
    #[must_use]
    pub const fn web_access(&self) -> Option<&fm_protocol::WebAccessMessage> {
        self.web_access.as_ref()
    }

    /// Records the system audio capture devices from a
    /// `system_audio_devices` reply (Protocol 2.68).
    pub fn apply_system_audio_inventory(&mut self, inventory: fm_protocol::SystemAudioInventory) {
        self.system_audio_inventory = Some(inventory);
    }

    /// Returns the system audio capture devices as last listed (Protocol
    /// 2.68).
    #[must_use]
    pub const fn system_audio_inventory(&self) -> Option<&fm_protocol::SystemAudioInventory> {
        self.system_audio_inventory.as_ref()
    }

    /// Records the capture devices from a `capture_devices` runtime event
    /// or reply (Protocol 2.47).
    pub fn apply_capture_inventory(&mut self, inventory: fm_protocol::CaptureInventory) {
        self.capture_inventory = Some(inventory);
    }

    /// Returns the capture devices as last reported (Protocol 2.47).
    #[must_use]
    pub const fn capture_inventory(&self) -> Option<&fm_protocol::CaptureInventory> {
        self.capture_inventory.as_ref()
    }

    /// Records a capture input's acquisition status from a `capture_status`
    /// runtime event (Protocol 2.47). Runtime events never move the durable
    /// cursor; a status for an input the model does not know as a capture
    /// input is ignored.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot.
    pub fn apply_runtime_capture_status(
        &mut self,
        input: InputId,
        status: fm_protocol::CaptureStatus,
    ) -> Result<(), ModelError> {
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        if let Some(entry) = state.capture_inputs.get_mut(&input) {
            entry.status = status;
        }
        Ok(())
    }

    /// Records a system audio input's capture status from a
    /// `system_audio_status` runtime event (Protocol 2.55). Runtime events
    /// never move the durable cursor; a status for an input the model does
    /// not know as a system audio input is ignored.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot.
    pub fn apply_runtime_system_audio_status(
        &mut self,
        input: InputId,
        status: fm_protocol::SystemAudioStatus,
    ) -> Result<(), ModelError> {
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        if let Some(entry) = state.system_audio_inputs.get_mut(&input) {
            entry.status = status;
        }
        Ok(())
    }

    /// Records the slide a slideshow shows from a `slideshow_page` runtime
    /// event (IN-009). Runtime events never move the durable cursor; a
    /// status for an input the model does not know as a slideshow is
    /// ignored.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot.
    pub fn apply_runtime_slideshow_page(
        &mut self,
        input: InputId,
        status: fm_protocol::SlideshowPageStatus,
    ) -> Result<(), ModelError> {
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        if let Some(entry) = state.still_inputs.get_mut(&input)
            && matches!(entry.source, fm_protocol::WireStillSource::Slideshow(_))
        {
            if entry.status.generation != status.generation {
                entry.listing = None;
            }
            entry.status = status;
        }
        Ok(())
    }

    /// Records a window of a slideshow's listing from a `slideshow_slides`
    /// reply (IN-009). A reply for another listing generation than the
    /// slideshow's, or for an input that is no slideshow, is ignored.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot.
    pub fn apply_slideshow_slides(
        &mut self,
        message: &fm_protocol::SlideshowSlidesMessage,
    ) -> Result<(), ModelError> {
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let Some(entry) = state.still_inputs.get_mut(&message.input.to_domain()) else {
            return Ok(());
        };
        if !matches!(entry.source, fm_protocol::WireStillSource::Slideshow(_))
            || message.generation != entry.status.generation
        {
            return Ok(());
        }
        let listing = entry.listing.get_or_insert_with(SlideshowListing::default);
        listing.apply(message);
        Ok(())
    }

    /// Records the connected displays from a `display_inventory` runtime
    /// event or reply (Protocol 2.42).
    pub fn apply_display_inventory(&mut self, inventory: fm_protocol::DisplayInventory) {
        self.display_inventory = Some(inventory);
    }

    /// Returns the connected displays as last reported (Protocol 2.42).
    #[must_use]
    pub const fn display_inventory(&self) -> Option<&fm_protocol::DisplayInventory> {
        self.display_inventory.as_ref()
    }

    /// Records a network stream input's receive status from a
    /// `network_stream_status` runtime event (Protocol 2.46). Runtime events
    /// never move the durable cursor; a status for an input the model does
    /// not know as a network stream is ignored.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot.
    pub fn apply_runtime_network_stream_status(
        &mut self,
        input: InputId,
        status: fm_protocol::NetworkStreamStatus,
    ) -> Result<(), ModelError> {
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        if let Some(entry) = state.network_streams.get_mut(&input) {
            entry.status = status;
        }
        Ok(())
    }

    /// Records the `LiveLAN` stream's status from a `live_lan_status` runtime
    /// event (Protocol 2.57).
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot.
    pub fn apply_runtime_live_lan_status(
        &mut self,
        status: fm_protocol::LiveLanStatus,
    ) -> Result<(), ModelError> {
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        state.live_lan_status = status;
        Ok(())
    }

    /// Records a display-output slot's runtime state from a
    /// `display_output_status` runtime event (Protocol 2.42).
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot.
    pub fn apply_runtime_display_output_status(
        &mut self,
        status: fm_protocol::DisplayOutputStatus,
    ) -> Result<(), ModelError> {
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        state
            .display_output_states
            .insert(status.slot, status.state);
        Ok(())
    }

    #[must_use]
    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    #[must_use]
    pub const fn state(&self) -> Option<&ProjectState> {
        self.state.as_ref()
    }

    /// Builds an immutable render value by layering pending intent over the
    /// authoritative desired state. Realized state is never optimistic.
    #[must_use]
    pub fn view(&self) -> Option<ClientView> {
        let state = self.state.as_ref()?;
        let cursor = self.cursor.clone()?;
        let mut switcher = state.switcher;
        for command in &self.pending {
            if let Some(change) = command.optimistic {
                change.apply(&mut switcher.desired);
            }
        }
        Some(ClientView {
            cursor,
            show_name: state.show_name.clone(),
            inputs: state.inputs.clone(),
            input_names: state.input_names.clone(),
            outputs: state.outputs.clone(),
            input_audio_strips: state.input_audio_strips.clone(),
            stingers: state.stingers.clone(),
            streams: state.streams.clone(),
            recordings: state.recordings.clone(),
            desired_overlays: state.desired_overlays.clone(),
            realized_overlays: state.realized_overlays.clone(),
            switcher,
            edit_locks: state.edit_locks.clone(),
            edit_history: state.edit_history,
            transition_favorites: state.transition_favorites,
            input_lifecycle: state
                .inputs
                .iter()
                .map(|input| state.input_lifecycle(*input))
                .collect(),
            input_categories: state.input_categories.clone(),
            audio_mixer: state.audio_mixer.clone(),
            fade_to_black_excluded_outputs: state.fade_to_black_excluded_outputs.clone(),
            fade_to_black_output_durations: state.fade_to_black_output_durations.clone(),
            fade_to_black_curve: state.fade_to_black_curve,
            display_outputs: state
                .display_outputs
                .iter()
                .map(|(slot, config)| fm_protocol::DisplayOutputEntry {
                    slot: *slot,
                    config: config.clone(),
                })
                .collect(),
            display_output_states: state
                .display_outputs
                .keys()
                .map(|slot| fm_protocol::DisplayOutputStatus {
                    slot: *slot,
                    state: state.display_output_state(*slot),
                })
                .collect(),
            display_inventory: self.display_inventory.clone(),
            network_streams: state
                .network_streams
                .values()
                .filter(|entry| state.inputs.contains(&entry.input.to_domain()))
                .cloned()
                .collect(),
            capture_inputs: state
                .capture_inputs
                .values()
                .filter(|entry| state.inputs.contains(&entry.input.to_domain()))
                .cloned()
                .collect(),
            capture_inventory: self.capture_inventory.clone(),
            system_audio_inventory: self.system_audio_inventory.clone(),
            web_access: self.web_access.clone(),
            system_audio_inputs: state
                .system_audio_inputs
                .values()
                .filter(|entry| state.inputs.contains(&entry.input.to_domain()))
                .cloned()
                .collect(),
            still_inputs: state
                .still_inputs
                .iter()
                .filter(|(input, _)| state.inputs.contains(input))
                .map(|(input, entry)| (*input, entry.clone()))
                .collect(),
            stream_targets: state.stream_targets.clone(),
            live_lan: state.live_lan,
            live_lan_status: state.live_lan_status(),
        })
    }

    #[must_use]
    pub const fn sync_status(&self) -> &SyncStatus {
        &self.sync_status
    }

    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.sync_status.is_stale()
    }

    /// Returns the last validated cursor to send during reconnect.
    #[must_use]
    pub const fn reconnect_cursor(&self) -> Option<&ProjectCursor> {
        self.cursor.as_ref()
    }

    /// Returns the last validated cursor in the current protocol DTO.
    #[must_use]
    pub fn protocol_reconnect_cursor(&self) -> Option<ResumeCursor> {
        self.cursor.as_ref().map(ProjectCursor::resume_cursor)
    }

    #[must_use]
    pub fn pending_commands(&self) -> &[PendingCommand] {
        &self.pending
    }

    #[must_use]
    pub const fn last_rejection(&self) -> Option<&RejectedCommand> {
        self.last_rejection.as_ref()
    }

    /// Reduces an action in place.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when identity, ordering, state, or command
    /// invariants are violated.
    pub fn reduce(&mut self, action: Action) -> Result<Reduction, ModelError> {
        match action {
            Action::InstallSnapshot(snapshot) => self
                .install_snapshot(*snapshot)
                .map(Reduction::SnapshotInstalled),
            Action::ApplyEvent(event) => self.apply_event(event).map(Reduction::EventApplied),
            Action::ApplyRuntimeRealization(realization) => self
                .apply_runtime_realization(realization)
                .map(Reduction::RuntimeRealizationApplied),
            Action::TrackCommand { id, optimistic } => {
                self.track_command(id, optimistic)?;
                Ok(Reduction::CommandTracked)
            }
            Action::ReconcileCommand(result) => self
                .reconcile_command(&result)
                .map(Reduction::CommandReconciled),
            Action::ObserveServer(hello) => {
                self.observe_server(&hello)?;
                Ok(Reduction::ServerObserved)
            }
            Action::ApplyTitleState(message) => {
                self.apply_title_state(message)?;
                Ok(Reduction::TitleStateApplied)
            }
            Action::ApplyPlaylistState(message) => {
                self.apply_playlist_state(message)?;
                Ok(Reduction::PlaylistStateApplied)
            }
        }
    }

    /// Clones and reduces this model, leaving the original untouched.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as [`Self::reduce`].
    pub fn reduced(&self, action: Action) -> Result<(Self, Reduction), ModelError> {
        let mut next = self.clone();
        let reduction = next.reduce(action)?;
        Ok((next, reduction))
    }

    /// Installs a complete authoritative snapshot.
    ///
    /// # Errors
    ///
    /// Rejects the snapshot when its project or expected engine identity does
    /// not match, its engine identity is incomplete, or routing references an
    /// absent or duplicate input.
    #[allow(clippy::too_many_lines)]
    pub fn install_snapshot(
        &mut self,
        snapshot: ProjectSnapshot,
    ) -> Result<SnapshotInstalled, ModelError> {
        self.validate_project(snapshot.cursor.project_id)?;
        validate_engine(&snapshot.cursor.engine)?;
        if let Some(expected) = &self.expected_engine
            && expected != &snapshot.cursor.engine
        {
            return Err(ModelError::EngineMismatch {
                expected: expected.clone(),
                observed: snapshot.cursor.engine,
            });
        }
        if let Some(current) = &self.cursor
            && current.engine == snapshot.cursor.engine
            && snapshot.cursor.revision < current.revision
        {
            return Err(ModelError::OutOfOrder {
                current: current.revision,
                observed: snapshot.cursor.revision,
            });
        }
        if self
            .expected_engine
            .as_ref()
            .is_some_and(|expected| expected == &snapshot.cursor.engine)
            && let Some(known) = self.known_revision
            && snapshot.cursor.revision < known
        {
            return Err(ModelError::OutOfOrder {
                current: known,
                observed: snapshot.cursor.revision,
            });
        }
        validate_snapshot_inputs(&snapshot)?;

        let identity_changed = self
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.engine != snapshot.cursor.engine);
        let mut reconciled_commands = Vec::new();
        let mut discarded_commands = Vec::new();
        let input_set: HashSet<_> = snapshot.inputs.iter().copied().collect();
        self.pending.retain(|command| {
            let accepted = matches!(
                command.status,
                PendingStatus::AwaitingEvent { accepted_revision }
                    if accepted_revision <= snapshot.cursor.revision
            );
            let valid = command.optimistic.is_none_or(|change| {
                change
                    .inputs()
                    .into_iter()
                    .flatten()
                    .all(|input| input_set.contains(&input))
            });
            if accepted {
                reconciled_commands.push(command.id.clone());
                false
            } else if identity_changed || !valid {
                discarded_commands.push(command.id.clone());
                false
            } else {
                true
            }
        });

        if identity_changed {
            self.desired_by_revision.clear();
            self.last_runtime_realization = None;
        }
        self.desired_by_revision.insert(
            snapshot.cursor.revision,
            (
                snapshot.switcher.desired,
                snapshot.switcher.desired_manual_transition,
                snapshot.switcher.desired_fade_to_black,
                snapshot.desired_overlays.clone(),
            ),
        );
        self.expected_engine = Some(snapshot.cursor.engine.clone());
        self.known_revision = Some(snapshot.cursor.revision);
        self.cursor = Some(snapshot.cursor);
        let input_lifecycle = snapshot
            .inputs
            .iter()
            .copied()
            .zip(snapshot.input_lifecycle)
            .filter(|(_, lifecycle)| *lifecycle != InputLifecycleStatus::default())
            .collect();
        self.state = Some(ProjectState {
            show_name: snapshot.show_name,
            inputs: snapshot.inputs,
            input_names: snapshot.input_names,
            outputs: snapshot.outputs,
            input_audio_strips: snapshot.input_audio_strips,
            stingers: snapshot.stingers,
            streams: snapshot.streams,
            recordings: snapshot.recordings,
            desired_overlays: snapshot.desired_overlays,
            realized_overlays: snapshot.realized_overlays,
            switcher: snapshot.switcher,
            edit_locks: snapshot.edit_locks,
            edit_history: snapshot.edit_history,
            titles: snapshot.titles,
            scenes: snapshot.scenes,
            scene_layouts: snapshot.scene_layouts,
            frame_rate: snapshot.frame_rate,
            playlist: (!snapshot.playlist_follows).then(PlaylistStatus::empty),
            grading: GradingStatus::default(),
            grade_states_pending: snapshot.grade_states,
            animation_presets: AnimationPresetsStatus::default(),
            animation_preset_states_pending: snapshot.animation_preset_states,
            transition_favorites: snapshot.transition_favorites,
            media: BTreeMap::new(),
            media_states_pending: snapshot.media_states,
            cue_lists: Vec::new(),
            cue_list_states_pending: snapshot.cue_list_states,
            cue_frame: None,
            scripts: Vec::new(),
            script_states_pending: snapshot.script_states,
            script_runs: Vec::new(),
            script_run_states: BTreeMap::new(),
            shortcuts: Vec::new(),
            shortcut_states_pending: snapshot.shortcut_states,
            last_shortcut_fire: None,
            media_lists: BTreeMap::new(),
            media_list_states_pending: snapshot.media_list_states,
            input_lifecycle,
            input_categories: snapshot.input_categories,
            audio_strip_dsp: BTreeMap::new(),
            audio_strip_dsp_states_pending: snapshot.audio_mixer.strip_dsp_states,
            audio_mixer: snapshot.audio_mixer,
            fade_to_black_excluded_outputs: snapshot.fade_to_black_excluded_outputs,
            fade_to_black_output_durations: snapshot.fade_to_black_output_durations,
            fade_to_black_curve: snapshot.fade_to_black_curve,
            display_outputs: snapshot
                .display_outputs
                .into_iter()
                .map(|entry| (entry.slot, entry.config))
                .collect(),
            display_output_states: snapshot
                .display_output_states
                .into_iter()
                .map(|status| (status.slot, status.state))
                .collect(),
            network_streams: snapshot
                .network_streams
                .into_iter()
                .map(|entry| (entry.input.to_domain(), entry))
                .collect(),
            capture_inputs: snapshot
                .capture_inputs
                .into_iter()
                .map(|entry| (entry.input.to_domain(), entry))
                .collect(),
            system_audio_inputs: snapshot
                .system_audio_inputs
                .into_iter()
                .map(|entry| (entry.input.to_domain(), entry))
                .collect(),
            still_inputs: BTreeMap::new(),
            still_states_pending: snapshot.still_states,
            stream_targets: snapshot.stream_targets,
            live_lan: snapshot.live_lan,
            live_lan_status: snapshot.live_lan_status,
        });
        self.applied_events.clear();
        self.sync_status = SyncStatus::Current;
        Ok(SnapshotInstalled {
            reconciled_commands,
            discarded_commands,
        })
    }

    /// Applies one contiguous, globally ordered event.
    ///
    /// # Errors
    ///
    /// Rejects project/engine changes, gaps, unknown or conflicting old
    /// revisions, invalid input references, and updates before a snapshot.
    pub fn apply_event(&mut self, event: DurableProjectEvent) -> Result<EventApplied, ModelError> {
        self.validate_project(event.cursor.project_id)?;
        validate_engine(&event.cursor.engine)?;
        let Some(cursor) = self.cursor.as_ref() else {
            return Err(ModelError::StateUnavailable);
        };
        if cursor.engine != event.cursor.engine {
            self.sync_status = SyncStatus::RequiresSnapshot;
            return Err(ModelError::EngineMismatch {
                expected: cursor.engine.clone(),
                observed: event.cursor.engine,
            });
        }
        if matches!(self.sync_status, SyncStatus::RequiresSnapshot) {
            return Err(ModelError::SnapshotRequired);
        }
        if let Some(applied) = self.applied_events.get(&event.cursor.revision) {
            return if applied == &event {
                Ok(EventApplied::Duplicate)
            } else {
                self.sync_status = SyncStatus::RequiresSnapshot;
                Err(ModelError::ConflictingDuplicate {
                    revision: event.cursor.revision,
                })
            };
        }
        if event.cursor.revision <= cursor.revision {
            return Err(ModelError::OutOfOrder {
                current: cursor.revision,
                observed: event.cursor.revision,
            });
        }
        let expected = cursor
            .revision
            .checked_next()
            .map_err(|_| ModelError::RevisionExhausted)?;
        if event.cursor.revision != expected {
            self.known_revision =
                Some(self.known_revision.map_or(event.cursor.revision, |known| {
                    known.max(event.cursor.revision)
                }));
            self.sync_status = SyncStatus::Behind {
                local_revision: cursor.revision,
                known_revision: event.cursor.revision,
            };
            return Err(ModelError::RevisionGap {
                expected,
                observed: event.cursor.revision,
            });
        }

        let state = self.state.as_ref().ok_or(ModelError::StateUnavailable)?;
        validate_change(&event.change, state)?;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        apply_change(state, event.change.clone());
        state.edit_history = event.edit_history;

        let revision = event.cursor.revision;
        self.desired_by_revision.insert(
            revision,
            (
                state.switcher.desired,
                state.switcher.desired_manual_transition,
                state.switcher.desired_fade_to_black,
                state.desired_overlays.clone(),
            ),
        );
        self.cursor = Some(event.cursor.clone());
        self.applied_events.insert(revision, event);
        let reconciled_commands = self.reconcile_through(revision);
        let known = self
            .known_revision
            .map_or(revision, |value| value.max(revision));
        self.known_revision = Some(known);
        self.sync_status = if known > revision {
            SyncStatus::Behind {
                local_revision: revision,
                known_revision: known,
            }
        } else {
            SyncStatus::Current
        };
        Ok(EventApplied::Applied {
            reconciled_commands,
        })
    }

    /// Completes one title the installed snapshot announced.
    ///
    /// `message` must be at the applied revision (title states follow their
    /// snapshot before any event) and name an announced, still incomplete
    /// title with the announced field revision. Its fields must be strictly
    /// ascending.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::TitleStateMismatch`] for any other mismatch; the model is
    /// then unchanged and the caller must resynchronize.
    pub fn apply_title_state(&mut self, message: TitleStateMessage) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let input = message.input.to_domain();
        let mismatch = ModelError::TitleStateMismatch(input);
        if Revision::new(message.revision) != revision
            || !message
                .fields
                .windows(2)
                .all(|pair| pair[0].field < pair[1].field)
        {
            return Err(mismatch);
        }
        let title = state
            .titles
            .iter_mut()
            .find(|title| title.input == input)
            .ok_or(mismatch.clone())?;
        if title.complete || title.field_revision != message.field_revision {
            return Err(mismatch);
        }
        title.name = message.name;
        title.fields = message.fields;
        title.phase = message.phase;
        title.phase_frame = message.phase_frame;
        title.phase_frames = message.phase_frames;
        title.complete = true;
        Ok(())
    }

    /// Completes the installed snapshot with the `playlist_state` it
    /// announced.
    ///
    /// `message` must be at the applied revision (it follows its snapshot
    /// before any event), the snapshot must still await it, and its entries
    /// must name show inputs.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot,
    /// [`ModelError::UnknownInput`] for an entry naming no show input, and
    /// [`ModelError::PlaylistStateMismatch`] otherwise; the model is then
    /// unchanged and the caller must resynchronize.
    pub fn apply_playlist_state(
        &mut self,
        message: PlaylistStateMessage,
    ) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        if Revision::new(message.revision) != revision || state.playlist.is_some() {
            return Err(ModelError::PlaylistStateMismatch);
        }
        if let Some(entry) = message
            .entries
            .iter()
            .find(|entry| !state.inputs.contains(&entry.input.to_domain()))
        {
            return Err(ModelError::UnknownInput(entry.input.to_domain()));
        }
        state.playlist = Some(PlaylistStatus::from_protocol(message));
        Ok(())
    }

    /// Adds one of the `grade_state` records the installed snapshot
    /// announced.
    ///
    /// `message` must be at the applied revision, the snapshot must still
    /// await a grade state, and the entry must follow the documented order
    /// and name only show inputs and known library grades.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::GradeStateMismatch`] otherwise; the model is then
    /// unchanged and the caller must resynchronize.
    pub fn apply_grade_state(&mut self, message: GradeStateMessage) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        if Revision::new(message.revision) != revision || state.grade_states_pending == 0 {
            return Err(ModelError::GradeStateMismatch);
        }
        let mut grading = state.grading.clone();
        if !grading.push_entry(message.entry, &state.inputs) {
            return Err(ModelError::GradeStateMismatch);
        }
        state.grading = grading;
        state.grade_states_pending -= 1;
        Ok(())
    }

    /// Adds one of the `animation_preset_state` records the installed
    /// snapshot announced (Protocol 2.34), in library order.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::AnimationPresetStateMismatch`] when the record is at
    /// another revision, unannounced, or duplicates a preset id or name; the
    /// model is then unchanged and the caller must resynchronize.
    pub fn apply_animation_preset_state(
        &mut self,
        message: AnimationPresetStateMessage,
    ) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        if Revision::new(message.revision) != revision || state.animation_preset_states_pending == 0
        {
            return Err(ModelError::AnimationPresetStateMismatch);
        }
        if !state.animation_presets.push_entry(message.entry) {
            return Err(ModelError::AnimationPresetStateMismatch);
        }
        state.animation_preset_states_pending -= 1;
        Ok(())
    }

    /// Adds one of the `media_state` records the installed snapshot
    /// announced (Protocol 2.37), in ascending input order.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::MediaStateMismatch`] when the record is at another
    /// revision, unannounced, out of input order, or for an unknown input;
    /// the model is then unchanged and the caller must resynchronize.
    pub fn apply_media_state(&mut self, message: MediaStateMessage) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let input = message.status.input.to_domain();
        if Revision::new(message.revision) != revision
            || state.media_states_pending == 0
            || !state.inputs.contains(&input)
            || state
                .media
                .last_key_value()
                .is_some_and(|(last, _)| last.get() >= input.get())
        {
            return Err(ModelError::MediaStateMismatch);
        }
        state.media.insert(input, message.status);
        state.media_states_pending -= 1;
        Ok(())
    }

    /// Adds one of the `still_state` records the installed snapshot
    /// announced (IN-008, IN-009), in ascending input order.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::StillStateMismatch`] when the record is at another
    /// revision, unannounced, out of input order, or for an unknown input;
    /// the model is then unchanged and the caller must resynchronize.
    pub fn apply_still_state(
        &mut self,
        message: fm_protocol::StillStateMessage,
    ) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let input = message.input.to_domain();
        if Revision::new(message.revision) != revision
            || state.still_states_pending == 0
            || !state.inputs.contains(&input)
            || state
                .still_inputs
                .last_key_value()
                .is_some_and(|(last, _)| last.get() >= input.get())
        {
            return Err(ModelError::StillStateMismatch);
        }
        state.still_inputs.insert(
            input,
            StillInputStatus {
                source: message.source,
                status: message.status,
                listing: None,
            },
        );
        state.still_states_pending -= 1;
        Ok(())
    }

    /// Adds one of the `cue_list_state` records the installed snapshot
    /// announced (Protocol 2.50), in project list order.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::CueListStateMismatch`] when the record is at another
    /// revision, unannounced, or duplicates a list; a list naming unknown
    /// inputs is refused like a `cue_list_changed`.
    pub fn apply_cue_list_state(
        &mut self,
        message: fm_protocol::CueListStateMessage,
    ) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        if Revision::new(message.revision) != revision
            || state.cue_list_states_pending == 0
            || state.cue_list(message.list.id).is_some()
        {
            return Err(ModelError::CueListStateMismatch);
        }
        cue_lists::validate_list(&message.list, state)?;
        state.cue_lists.push(message.list);
        state.cue_list_states_pending -= 1;
        state.cue_frame = Some(
            state
                .cue_frame
                .map_or(message.frame, |seen| seen.max(message.frame)),
        );
        Ok(())
    }

    /// Adds one of the `script_state` records the installed snapshot
    /// announced (Protocol 2.58), in project order.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::ScriptStateMismatch`] when the record is at another
    /// revision, unannounced, or duplicates a script; a script outside its
    /// bounds is refused like a `script_changed`.
    pub fn apply_script_state(
        &mut self,
        message: fm_protocol::ScriptStateMessage,
    ) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        if Revision::new(message.revision) != revision
            || state.script_states_pending == 0
            || state.script(message.script.id).is_some()
        {
            return Err(ModelError::ScriptStateMismatch);
        }
        scripts::validate_script(&message.script)?;
        if message.run != fm_protocol::ScriptRunStatus::Idle {
            // Run ids are unique, and at most MAX_SCRIPT_RUNS runs wait.
            let id = scripts::run_id(&message.run);
            let waiting = state
                .script_run_states
                .values()
                .filter(|run| matches!(run, fm_protocol::ScriptRunStatus::Waiting { .. }))
                .count();
            if state
                .script_run_states
                .values()
                .any(|run| scripts::run_id(run) == id)
                || (matches!(message.run, fm_protocol::ScriptRunStatus::Waiting { .. })
                    && waiting >= fm_protocol::MAX_SCRIPT_RUNS)
            {
                return Err(ModelError::ScriptStateMismatch);
            }
            state
                .script_run_states
                .insert(message.script.id, message.run);
        }
        state.scripts.push(message.script);
        state.script_states_pending -= 1;
        Ok(())
    }

    /// Adds one of the `shortcut_state` records the installed snapshot
    /// announced (Protocol 2.72), in project order.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::ShortcutStateMismatch`] when the record is at another
    /// revision, unannounced, or duplicates a shortcut; a shortcut outside
    /// its bounds is refused like a `shortcut_changed`.
    pub fn apply_shortcut_state(
        &mut self,
        message: fm_protocol::ShortcutStateMessage,
    ) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        if Revision::new(message.revision) != revision
            || state.shortcut_states_pending == 0
            || state.shortcut(message.shortcut.id).is_some()
        {
            return Err(ModelError::ShortcutStateMismatch);
        }
        shortcuts::validate_shortcut(&message.shortcut)?;
        state.shortcuts.push(message.shortcut);
        state.shortcut_states_pending -= 1;
        Ok(())
    }

    /// Adds one of the `media_list_state` records the installed snapshot
    /// announced (IN-006).
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::MediaListStateMismatch`] when the record is at another
    /// revision, unannounced, incoherent, or names an unknown or repeated
    /// input.
    pub fn apply_media_list_state(
        &mut self,
        message: fm_protocol::MediaListStateMessage,
    ) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let input = message.status.input.to_domain();
        if Revision::new(message.revision) != revision
            || state.media_list_states_pending == 0
            || !state.inputs.contains(&input)
            || state.media_lists.contains_key(&input)
            || !media_lists::status_is_valid(&message.status)
        {
            return Err(ModelError::MediaListStateMismatch);
        }
        state.media_lists.insert(input, message.status);
        state.media_list_states_pending -= 1;
        Ok(())
    }

    /// Adds one of the `audio_strip_dsp_state` records the installed
    /// snapshot announced (Protocol 2.75), in ascending input order.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::AudioStripDspStateMismatch`] when the record is at
    /// another revision, unannounced, out of order, for an unknown input, or
    /// carries a default or invalid chain.
    pub fn apply_audio_strip_dsp_state(
        &mut self,
        message: fm_protocol::AudioStripDspStateMessage,
    ) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let input = message.input.to_domain();
        if Revision::new(message.revision) != revision
            || state.audio_strip_dsp_states_pending == 0
            || !state.inputs.contains(&input)
            || state
                .audio_strip_dsp
                .last_key_value()
                .is_some_and(|(last, _)| *last >= input)
            || message.chain.is_default()
            || message.chain.validate().is_err()
        {
            return Err(ModelError::AudioStripDspStateMismatch);
        }
        state.audio_strip_dsp.insert(input, message.chain);
        state.audio_strip_dsp_states_pending -= 1;
        Ok(())
    }

    /// Replaces a media input's status from a `media` runtime event
    /// (Protocol 2.37). Runtime events never move the durable cursor.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::InvalidMediaChange`] for an input outside the project.
    pub fn apply_runtime_media(&mut self, status: MediaStatus) -> Result<(), ModelError> {
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let input = status.input.to_domain();
        if !state.inputs.contains(&input) {
            return Err(ModelError::InvalidMediaChange(input));
        }
        state.media.insert(input, status);
        Ok(())
    }

    /// Records an input's source availability from an `input_availability`
    /// runtime event (Protocol 2.38). Runtime events never move the durable
    /// cursor.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::UnknownInput`] for an input outside the project.
    pub fn apply_runtime_input_availability(
        &mut self,
        input: InputId,
        offline: Option<InputOfflineReason>,
    ) -> Result<(), ModelError> {
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        if !state.inputs.contains(&input) {
            return Err(ModelError::UnknownInput(input));
        }
        let mut lifecycle = state.input_lifecycle(input);
        lifecycle.offline = offline;
        input_lifecycle::store_lifecycle(state, input, lifecycle);
        Ok(())
    }

    /// Adds the template animation of one title the installed snapshot
    /// announced, from its `title_animation_state`.
    ///
    /// `message` must be at the applied revision (animation states follow
    /// their snapshot before any event) and name an announced title whose
    /// animation has not arrived.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::TitleStateMismatch`] otherwise; the model is then
    /// unchanged and the caller must resynchronize.
    pub fn apply_title_animation_state(
        &mut self,
        message: TitleAnimationStateMessage,
    ) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let input = message.input.to_domain();
        let mismatch = ModelError::TitleStateMismatch(input);
        if Revision::new(message.revision) != revision {
            return Err(mismatch);
        }
        let title = state
            .titles
            .iter_mut()
            .find(|title| title.input == input)
            .ok_or(mismatch.clone())?;
        if title.animation.is_some() {
            return Err(mismatch);
        }
        title.animation = Some(TitleAnimationStatus {
            revision: message.animation_revision,
            clip: Arc::from(message.clip),
        });
        Ok(())
    }

    /// Adds the template layout of one title the installed snapshot
    /// announced, from its `title_layout_state` (Protocol 2.30).
    ///
    /// `message` must be at the applied revision and name an announced
    /// title whose animation has arrived and whose layout has not.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::TitleStateMismatch`] otherwise; the model is then
    /// unchanged and the caller must resynchronize.
    pub fn apply_title_layout_state(
        &mut self,
        message: TitleLayoutStateMessage,
    ) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let input = message.input.to_domain();
        let mismatch = ModelError::TitleStateMismatch(input);
        if Revision::new(message.revision) != revision {
            return Err(mismatch);
        }
        let title = state
            .titles
            .iter_mut()
            .find(|title| title.input == input)
            .ok_or(mismatch.clone())?;
        if title.animation.is_none() || title.layout.is_some() {
            return Err(mismatch);
        }
        title.layout = Some(Arc::from(message.layout));
        Ok(())
    }

    /// Adds the animation of one Scene input the installed snapshot
    /// announced, from its `scene_animation_state` (Protocol 2.31).
    ///
    /// `message` must be at the applied revision and name an announced
    /// Scene input whose animation has not arrived.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::SceneStateMismatch`] otherwise; the model is then
    /// unchanged and the caller must resynchronize.
    pub fn apply_scene_animation_state(
        &mut self,
        message: SceneAnimationStateMessage,
    ) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let input = message.input.to_domain();
        let mismatch = ModelError::SceneStateMismatch(input);
        if Revision::new(message.revision) != revision {
            return Err(mismatch);
        }
        let scene = state
            .scenes
            .iter_mut()
            .find(|scene| scene.input == input)
            .ok_or(mismatch.clone())?;
        if scene.animation.is_some() {
            return Err(mismatch);
        }
        scene.scene = Some(message.scene.to_domain());
        scene.animation = Some(SceneAnimationStatus {
            revision: message.animation_revision,
            clip: Arc::from(message.clip),
        });
        Ok(())
    }

    /// Adds the name and layout of one scene the installed snapshot
    /// announced, from its `scene_layout_state` (Protocol 2.31; keyed by
    /// scene since 2.35), and the layout of every Scene input showing it.
    ///
    /// `message` must be at the applied revision, follow every announced
    /// `scene_animation_state`, and name an announced scene whose layout
    /// has not arrived.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::StateUnavailable`] before a snapshot and
    /// [`ModelError::SceneLayoutMismatch`] otherwise; the model is then
    /// unchanged and the caller must resynchronize.
    pub fn apply_scene_layout_state(
        &mut self,
        message: SceneLayoutStateMessage,
    ) -> Result<(), ModelError> {
        let revision = self
            .cursor
            .as_ref()
            .ok_or(ModelError::StateUnavailable)?
            .revision;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        let id = message.scene.to_domain();
        let mismatch = ModelError::SceneLayoutMismatch(id);
        if Revision::new(message.revision) != revision
            || !state.scenes.iter().all(|scene| scene.animation.is_some())
        {
            return Err(mismatch);
        }
        let entry = state
            .scene_layouts
            .iter_mut()
            .find(|entry| entry.scene == id)
            .ok_or(mismatch.clone())?;
        if entry.layout.is_some() {
            return Err(mismatch);
        }
        let layout: Arc<WireSceneLayout> = Arc::from(message.layout);
        entry.name = message.name;
        entry.layout = Some(Arc::clone(&layout));
        for scene in &mut state.scenes {
            if scene.scene == Some(id) {
                scene.layout = Some(Arc::clone(&layout));
            }
        }
        Ok(())
    }

    /// Applies runtime routing for a known durable desired-state revision.
    ///
    /// This path has independent ordering and intentionally leaves the durable
    /// cursor, known revision, sync status, and command reconciliation intact.
    ///
    /// # Errors
    ///
    /// Rejects project/engine changes, unknown durable revisions, and stale or
    /// conflicting runtime sequences.
    pub fn apply_runtime_realization(
        &mut self,
        realization: RuntimeRealization,
    ) -> Result<RuntimeRealizationApplied, ModelError> {
        self.validate_project(realization.project_id)?;
        validate_engine(&realization.engine)?;
        let cursor = self.cursor.as_ref().ok_or(ModelError::StateUnavailable)?;
        if cursor.engine != realization.engine {
            return Err(ModelError::EngineMismatch {
                expected: cursor.engine.clone(),
                observed: realization.engine,
            });
        }
        if let Some(previous) = &self.last_runtime_realization {
            match realization.generation.cmp(&previous.generation) {
                Ordering::Less => {
                    return Err(ModelError::RuntimeGenerationOutOfOrder {
                        current_generation: previous.generation,
                        observed_generation: realization.generation,
                    });
                }
                Ordering::Greater => {}
                Ordering::Equal => match realization.sequence.cmp(&previous.sequence) {
                    Ordering::Less => {
                        return Err(ModelError::RuntimeOutOfOrder {
                            current_sequence: previous.sequence,
                            observed_sequence: realization.sequence,
                        });
                    }
                    Ordering::Equal if previous == &realization => {
                        return Ok(RuntimeRealizationApplied::Duplicate);
                    }
                    Ordering::Equal => {
                        return Err(ModelError::ConflictingRuntimeSequence {
                            sequence: realization.sequence,
                        });
                    }
                    Ordering::Greater => {}
                },
            }
        }
        let (desired, _, _, desired_overlays) = self
            .desired_by_revision
            .get(&realization.revision)
            .cloned()
            .ok_or(ModelError::UnknownDurableRevision {
                revision: realization.revision,
            })?;
        let realized_manual_transition = realization.manual_transition;
        let realized_fade_to_black = realization.fade_to_black;
        let state = self.state.as_ref().ok_or(ModelError::StateUnavailable)?;
        let inputs = state.inputs.iter().copied().collect();
        validate_manual_transition(realized_manual_transition, desired, &inputs)?;
        let state = self.state.as_mut().ok_or(ModelError::StateUnavailable)?;
        state.switcher.realized = desired;
        state.switcher.realized_manual_transition = realized_manual_transition;
        state.switcher.realized_fade_to_black = realized_fade_to_black;
        state.realized_overlays = desired_overlays;
        state.switcher.runtime_generation = Some(realization.generation);
        self.last_runtime_realization = Some(realization);
        Ok(RuntimeRealizationApplied::Applied)
    }

    /// Registers a command before it is sent.
    ///
    /// # Errors
    ///
    /// Rejects duplicate command IDs, optimistic references to unknown inputs,
    /// and registration before a snapshot is installed.
    pub fn track_command(
        &mut self,
        id: CommandId,
        optimistic: Option<OptimisticChange>,
    ) -> Result<(), ModelError> {
        if self.pending.iter().any(|command| command.id == id) {
            return Err(ModelError::DuplicateCommand(id));
        }
        let state = self.state.as_ref().ok_or(ModelError::StateUnavailable)?;
        if let Some(change) = optimistic {
            validate_optimistic(change, &state.inputs)?;
        }
        self.pending.push(PendingCommand {
            id,
            optimistic,
            status: PendingStatus::AwaitingResult,
        });
        Ok(())
    }

    /// Reconciles a protocol command result by command ID.
    ///
    /// Accepted optimistic state remains visible until its ordered durable
    /// revision is applied. Rejection removes it immediately.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::UnknownCommand`] when the result does not match an
    /// outstanding command.
    pub fn reconcile_command(
        &mut self,
        result: &CommandResult,
    ) -> Result<CommandReconciled, ModelError> {
        match result {
            CommandResult::Accepted { id, revision, .. } => {
                let id = CommandId::new(id.clone());
                let index = self.pending_index(&id)?;
                let revision = Revision::new(*revision);
                let current = self.cursor.as_ref().map(|cursor| cursor.revision);
                let awaiting_event = current.is_none_or(|value| value < revision);
                if awaiting_event {
                    self.pending[index].status = PendingStatus::AwaitingEvent {
                        accepted_revision: revision,
                    };
                    self.note_known_revision(revision);
                } else {
                    self.pending.remove(index);
                }
                Ok(CommandReconciled::Accepted {
                    revision,
                    awaiting_event,
                })
            }
            CommandResult::Rejected {
                id,
                code,
                message,
                fields,
                current_revision,
                retryable,
                reason,
            } => {
                let id = CommandId::new(id.clone());
                let index = self.pending_index(&id)?;
                self.pending.remove(index);
                let rejection = RejectedCommand {
                    id,
                    code: code.clone(),
                    message: message.clone(),
                    fields: fields.clone(),
                    current_revision: Revision::new(*current_revision),
                    retryable: *retryable,
                    reason: reason.clone(),
                };
                self.note_known_revision(rejection.current_revision);
                self.last_rejection = Some(rejection.clone());
                Ok(CommandReconciled::Rejected(rejection))
            }
        }
    }

    /// Validates a server handshake against the reconnect cursor and updates
    /// staleness from the server's current revision.
    ///
    /// # Errors
    ///
    /// Rejects an incomplete engine identity. Identity replacement itself is
    /// represented by [`SyncStatus::RequiresSnapshot`], as required by the
    /// reconnect protocol.
    pub fn observe_server(&mut self, hello: &ServerHello) -> Result<(), ModelError> {
        validate_engine(&hello.engine)?;
        self.expected_engine = Some(hello.engine.clone());
        let known = Revision::new(hello.current_revision);
        self.known_revision = Some(known);
        let Some(cursor) = &self.cursor else {
            self.sync_status = SyncStatus::AwaitingSnapshot;
            return Ok(());
        };
        if cursor.engine != hello.engine || !hello.resume {
            self.sync_status = SyncStatus::RequiresSnapshot;
            return Ok(());
        }
        self.sync_status = match known.cmp(&cursor.revision) {
            Ordering::Equal => SyncStatus::Current,
            Ordering::Greater => SyncStatus::Behind {
                local_revision: cursor.revision,
                known_revision: known,
            },
            Ordering::Less => SyncStatus::RequiresSnapshot,
        };
        Ok(())
    }

    fn validate_project(&self, observed: ProjectId) -> Result<(), ModelError> {
        if observed == self.project_id {
            Ok(())
        } else {
            Err(ModelError::ProjectMismatch {
                expected: self.project_id,
                observed,
            })
        }
    }

    fn pending_index(&self, id: &CommandId) -> Result<usize, ModelError> {
        self.pending
            .iter()
            .position(|command| &command.id == id)
            .ok_or_else(|| ModelError::UnknownCommand(id.clone()))
    }

    fn reconcile_through(&mut self, revision: Revision) -> Vec<CommandId> {
        let mut reconciled = Vec::new();
        self.pending.retain(|command| {
            if matches!(
                command.status,
                PendingStatus::AwaitingEvent { accepted_revision }
                    if accepted_revision <= revision
            ) {
                reconciled.push(command.id.clone());
                false
            } else {
                true
            }
        });
        reconciled
    }

    fn note_known_revision(&mut self, revision: Revision) {
        self.known_revision = Some(
            self.known_revision
                .map_or(revision, |known| known.max(revision)),
        );
        if let Some(cursor) = &self.cursor
            && revision > cursor.revision
            && !matches!(self.sync_status, SyncStatus::RequiresSnapshot)
        {
            self.sync_status = SyncStatus::Behind {
                local_revision: cursor.revision,
                known_revision: revision,
            };
        }
    }
}

fn validate_engine(engine: &EngineIdentity) -> Result<(), ModelError> {
    if engine.engine_id.is_empty() || engine.log_id.is_empty() {
        Err(ModelError::InvalidEngineIdentity)
    } else {
        Ok(())
    }
}

fn valid_input_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= MAX_INPUT_NAME_BYTES
}

fn valid_output_name(name: &str) -> bool {
    !name.trim().is_empty()
}

/// Whether the snapshot's per-input lifecycle parallels its inputs and
/// names only listed categories, and the category list is valid.
fn valid_snapshot_lifecycle(snapshot: &ProjectSnapshot) -> bool {
    snapshot.input_lifecycle.len() == snapshot.inputs.len()
        && input_lifecycle::valid_categories(&snapshot.input_categories)
        && snapshot.input_lifecycle.iter().all(|lifecycle| {
            lifecycle.category.is_none_or(|id| {
                snapshot
                    .input_categories
                    .iter()
                    .any(|category| category.id == id)
            })
        })
}

fn validate_snapshot_inputs(snapshot: &ProjectSnapshot) -> Result<(), ModelError> {
    if snapshot.input_names.len() != snapshot.inputs.len() {
        return Err(ModelError::InvalidInputNames);
    }
    let mut input_names = HashSet::with_capacity(snapshot.input_names.len());
    if snapshot
        .input_names
        .iter()
        .any(|name| !valid_input_name(name) || !input_names.insert(name.as_str()))
    {
        return Err(ModelError::InvalidInputNames);
    }
    let mut inputs = HashSet::with_capacity(snapshot.inputs.len());
    for input in &snapshot.inputs {
        if !inputs.insert(*input) {
            return Err(ModelError::DuplicateInput(*input));
        }
    }
    if !valid_snapshot_lifecycle(snapshot) {
        return Err(ModelError::InvalidInputLifecycleChange);
    }
    let mut output_names = HashSet::with_capacity(snapshot.outputs.len());
    let mut outputs = HashSet::with_capacity(snapshot.outputs.len());
    for output in &snapshot.outputs {
        if !valid_output_name(&output.name)
            || !output_names.insert(output.name.to_ascii_lowercase())
        {
            return Err(ModelError::InvalidOutputNames);
        }
        if !outputs.insert(output.output) {
            return Err(ModelError::DuplicateOutput(output.output));
        }
    }
    for input in [
        snapshot.switcher.desired.program,
        snapshot.switcher.desired.preview,
        snapshot.switcher.realized.program,
        snapshot.switcher.realized.preview,
    ] {
        if !inputs.contains(&input) {
            return Err(ModelError::UnknownInput(input));
        }
    }
    validate_stingers(&snapshot.stingers, &inputs)?;
    if !snapshot
        .edit_locks
        .inputs
        .windows(2)
        .all(|pair| pair[0] < pair[1])
        || snapshot
            .edit_locks
            .inputs
            .iter()
            .any(|input| !inputs.contains(input))
    {
        return Err(ModelError::InvalidEditLocks);
    }
    if !snapshot
        .titles
        .windows(2)
        .all(|pair| pair[0].input < pair[1].input)
        || snapshot
            .titles
            .iter()
            .any(|title| !inputs.contains(&title.input))
    {
        return Err(ModelError::InvalidTitles);
    }
    if let Some(scene) = snapshot
        .scenes
        .windows(2)
        .find(|pair| pair[0].input >= pair[1].input)
        .map(|pair| pair[1].input)
        .or_else(|| {
            snapshot
                .scenes
                .iter()
                .find(|scene| !inputs.contains(&scene.input))
                .map(|scene| scene.input)
        })
    {
        return Err(ModelError::SceneStateMismatch(scene));
    }
    validate_streams(&snapshot.streams)?;
    validate_recordings(&snapshot.recordings)?;
    validate_input_audio_strips(&snapshot.input_audio_strips, &inputs)?;
    validate_overlays(&snapshot.desired_overlays, &inputs, &outputs)?;
    validate_overlays(&snapshot.realized_overlays, &inputs, &outputs)?;
    validate_manual_transition(
        snapshot.switcher.desired_manual_transition,
        snapshot.switcher.desired,
        &inputs,
    )?;
    validate_manual_transition(
        snapshot.switcher.realized_manual_transition,
        snapshot.switcher.realized,
        &inputs,
    )?;
    Ok(())
}

fn validate_overlays(
    overlays: &[OverlayStatus],
    inputs: &HashSet<InputId>,
    outputs: &HashSet<OutputId>,
) -> Result<(), ModelError> {
    if overlays.len() != 8 {
        return Err(ModelError::InvalidOverlayCount(overlays.len()));
    }
    let mut channels = HashSet::with_capacity(8);
    for overlay in overlays {
        if !(1..=8).contains(&overlay.channel) || !channels.insert(overlay.channel) {
            return Err(ModelError::InvalidOverlayChannel(overlay.channel));
        }
        if overlay.active && overlay.source.is_none() {
            return Err(ModelError::ActiveOverlayMissingSource(overlay.channel));
        }
        if !(1..=3_600).contains(&overlay.duration_frames) {
            return Err(ModelError::InvalidOverlayTransitionDuration {
                channel: overlay.channel,
                duration_frames: overlay.duration_frames,
            });
        }
        if let Some(source) = overlay.source
            && !inputs.contains(&source)
        {
            return Err(ModelError::UnknownInput(source));
        }
        if overlay.queued_sources.len() > 64 {
            return Err(ModelError::InvalidOverlayQueueDepth {
                channel: overlay.channel,
                depth: overlay.queued_sources.len(),
            });
        }
        for source in &overlay.queued_sources {
            if !inputs.contains(source) {
                return Err(ModelError::UnknownInput(*source));
            }
        }
        if let Some(output) = overlay
            .included_outputs
            .iter()
            .find(|output| !outputs.contains(output))
        {
            return Err(ModelError::UnknownOutput(*output));
        }
        let included_outputs = overlay
            .included_outputs
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        if included_outputs.len() != overlay.included_outputs.len() {
            return Err(ModelError::DuplicateOverlayOutput(overlay.channel));
        }
    }
    Ok(())
}

fn validate_stingers(
    stingers: &[StingerStatus],
    inputs: &HashSet<InputId>,
) -> Result<(), ModelError> {
    let mut slots = HashSet::with_capacity(stingers.len());
    for stinger in stingers {
        if !(1..=8).contains(&stinger.slot) {
            return Err(ModelError::InvalidStingerSlot(stinger.slot));
        }
        if !slots.insert(stinger.slot) {
            return Err(ModelError::DuplicateStingerSlot(stinger.slot));
        }
        if !inputs.contains(&stinger.media_input) {
            return Err(ModelError::UnknownInput(stinger.media_input));
        }
    }
    Ok(())
}

fn validate_streams(streams: &[StreamStatus]) -> Result<(), ModelError> {
    if streams.len() > MAX_STREAM_STATUSES {
        return Err(ModelError::InvalidStreamCount(streams.len()));
    }
    let mut targets = HashSet::with_capacity(streams.len());
    for stream in streams {
        if stream.name.trim().is_empty() || stream.name.len() > MAX_STREAM_NAME_BYTES {
            return Err(ModelError::InvalidStreamNames);
        }
        if !targets.insert(stream.target) {
            return Err(ModelError::DuplicateStreamTarget(stream.target));
        }
    }
    Ok(())
}

fn validate_recordings(recordings: &[RecordingStatus]) -> Result<(), ModelError> {
    if recordings.len() != RECORDER_SLOT_COUNT {
        return Err(ModelError::InvalidRecordingCount(recordings.len()));
    }
    let mut slots = HashSet::with_capacity(recordings.len());
    for (index, status) in recordings.iter().enumerate() {
        if !slots.insert(status.recorder) {
            return Err(ModelError::DuplicateRecorderSlot(status.recorder.get()));
        }
        if usize::from(status.recorder.get()) != index + 1 {
            return Err(ModelError::InvalidRecordingOrder);
        }
    }
    Ok(())
}

fn validate_input_audio_strips(
    strips: &[InputAudioStripStatus],
    inputs: &HashSet<InputId>,
) -> Result<(), ModelError> {
    if strips.len() != inputs.len() {
        return Err(ModelError::InvalidInputAudioStrips);
    }
    let mut seen = HashSet::with_capacity(strips.len());
    if strips.iter().any(|status| {
        !(-96_000..=24_000).contains(&status.gain_millidb)
            || !(-10_000..=10_000).contains(&status.balance_basis_points)
            || status.delay_samples > 48_000
            || !inputs.contains(&status.input)
            || !seen.insert(status.input)
    }) {
        return Err(ModelError::InvalidInputAudioStrips);
    }
    Ok(())
}

/// Checks a `title_changed` against the complete replicated title: the same
/// field ids in order, a field revision that never goes backwards, and valid
/// desired overlays.
fn validate_title_change(change: &DurableChange, state: &ProjectState) -> Result<(), ModelError> {
    let DurableChange::TitleChanged {
        input,
        field_revision,
        fields,
        overlays,
    } = change
    else {
        return Ok(());
    };
    let (input, field_revision) = (*input, *field_revision);
    let title = state.title(input).ok_or(ModelError::UnknownTitle(input))?;
    if !title.complete {
        return Err(ModelError::TitleIncomplete(input));
    }
    if field_revision < title.field_revision
        || fields.len() != title.fields.len()
        || fields
            .iter()
            .zip(&title.fields)
            .any(|(update, current)| update.field != current.field)
    {
        return Err(ModelError::InvalidTitleChange(input));
    }
    let input_set = state.inputs.iter().copied().collect();
    let output_set = state.outputs.iter().map(|output| output.output).collect();
    validate_overlays(overlays, &input_set, &output_set)
}

/// Checks that a `playlist_changed` names only show inputs.
fn validate_playlist_change(
    selection: BusSelection,
    playlist: &PlaylistStatus,
    state: &ProjectState,
) -> Result<(), ModelError> {
    for input in [selection.program, selection.preview]
        .into_iter()
        .chain(playlist.entries.iter().map(|entry| entry.input.to_domain()))
    {
        if !state.inputs.contains(&input) {
            return Err(ModelError::UnknownInput(input));
        }
    }
    Ok(())
}

/// The model change of a `title_animation_changed` payload.
fn title_animation_change(payload: EventPayload) -> DurableChange {
    let EventPayload::TitleAnimationChanged {
        inputs,
        animation_revision,
        clip,
    } = payload
    else {
        unreachable!("only title animation changes are delegated")
    };
    DurableChange::TitleAnimationChanged {
        inputs: inputs
            .into_iter()
            .map(fm_protocol::WireInputId::to_domain)
            .collect(),
        animation: TitleAnimationStatus {
            revision: animation_revision,
            clip: Arc::from(clip),
        },
    }
}

/// A title animation change names ascending titles whose animations have
/// arrived, each advanced past its current revision.
fn validate_animation_change(
    change: &DurableChange,
    state: &ProjectState,
) -> Result<(), ModelError> {
    let DurableChange::TitleAnimationChanged { inputs, animation } = change else {
        unreachable!("only title animation changes are delegated")
    };
    let Some(first) = inputs.first() else {
        return Err(ModelError::InvalidTitles);
    };
    if !inputs.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(ModelError::InvalidTitleChange(*first));
    }
    for input in inputs {
        let title = state
            .title(*input)
            .ok_or(ModelError::UnknownTitle(*input))?;
        let current = title
            .animation
            .as_ref()
            .ok_or(ModelError::TitleIncomplete(*input))?;
        if animation.revision <= current.revision {
            return Err(ModelError::InvalidTitleChange(*input));
        }
    }
    Ok(())
}

/// A scene animation change names ascending Scene inputs whose animations
/// have arrived, each advanced past its current revision.
fn validate_scene_animation_change(
    inputs: &[InputId],
    animation: &SceneAnimationStatus,
    state: &ProjectState,
) -> Result<(), ModelError> {
    let Some(first) = inputs.first() else {
        return Err(ModelError::InvalidTitles);
    };
    if !inputs.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(ModelError::InvalidSceneChange(*first));
    }
    for input in inputs {
        let current = state
            .scene(*input)
            .and_then(|scene| scene.animation.as_ref())
            .ok_or(ModelError::InvalidSceneChange(*input))?;
        if animation.revision <= current.revision {
            return Err(ModelError::InvalidSceneChange(*input));
        }
    }
    Ok(())
}

fn validate_grade_change(change: &GradeChange, state: &ProjectState) -> Result<(), ModelError> {
    if state.grading_complete() && state.grading.accepts(change, &state.inputs) {
        Ok(())
    } else {
        Err(ModelError::InvalidGradeChange)
    }
}

#[allow(clippy::too_many_lines)]
fn validate_change(change: &DurableChange, state: &ProjectState) -> Result<(), ModelError> {
    let (selection, manual_transition, stingers, overlays, input_audio_strips) = match change {
        DurableChange::InputOrderChanged { inputs } => {
            let current: HashSet<_> = state.inputs.iter().copied().collect();
            let proposed: HashSet<_> = inputs.iter().copied().collect();
            if inputs.len() != state.inputs.len()
                || proposed.len() != inputs.len()
                || current != proposed
            {
                return Err(ModelError::InvalidInputOrder);
            }
            return Ok(());
        }
        DurableChange::InputRenamed { input, name } => {
            let index = state
                .inputs
                .iter()
                .position(|candidate| candidate == input)
                .ok_or(ModelError::UnknownInput(*input))?;
            if !valid_input_name(name)
                || state
                    .input_names
                    .iter()
                    .enumerate()
                    .any(|(other, current)| other != index && current == name)
            {
                return Err(ModelError::InvalidInputNames);
            }
            return Ok(());
        }
        DurableChange::InputAdded { .. } | DurableChange::InputRemoved { .. } => {
            return validate_input_catalogue_change(change, state);
        }
        DurableChange::InputLifecycleChanged { operation, added } => {
            return input_lifecycle::validate(operation, added, state);
        }
        DurableChange::AudioMixerChanged(operation) => {
            return validate_audio_mixer_change(operation, state);
        }
        DurableChange::FadeToBlackOutputChanged { output, .. }
        | DurableChange::FadeToBlackOutputDurationChanged { output, .. } => {
            return if state.outputs.iter().any(|status| status.output == *output) {
                Ok(())
            } else {
                Err(ModelError::InvalidAudioMixerChange)
            };
        }
        DurableChange::LiveLanChanged(entry) => {
            return match entry.source {
                fm_protocol::LiveLanSource::Output(output)
                    if !state.outputs.iter().any(|status| status.output == output) =>
                {
                    Err(ModelError::InvalidLiveLanChange)
                }
                _ => Ok(()),
            };
        }
        DurableChange::DisplayOutputChanged { config, .. } => {
            return match config.as_deref().map(|config| config.source) {
                Some(fm_protocol::DisplayOutputSource::Output(output))
                    if !state.outputs.iter().any(|status| status.output == output) =>
                {
                    Err(ModelError::InvalidDisplayOutputChange)
                }
                _ => Ok(()),
            };
        }
        DurableChange::TitleChanged { .. } => return validate_title_change(change, state),
        DurableChange::GradeChanged(change) => return validate_grade_change(change, state),
        DurableChange::AnimationPresetChanged(change) => {
            return if state.animation_presets_complete() && state.animation_presets.accepts(change)
            {
                Ok(())
            } else {
                Err(ModelError::InvalidAnimationPresetChange)
            };
        }
        DurableChange::TitleAnimationChanged { .. } => {
            return validate_animation_change(change, state);
        }
        DurableChange::SceneAnimationChanged { inputs, animation } => {
            return validate_scene_animation_change(inputs, animation, state);
        }
        DurableChange::SceneLayoutChanged { scene, .. } => {
            return if state
                .scene_layouts
                .iter()
                .any(|entry| entry.scene == *scene && entry.layout.is_some())
            {
                Ok(())
            } else {
                Err(ModelError::SceneLayoutMismatch(*scene))
            };
        }
        DurableChange::PlaylistChanged {
            selection,
            playlist,
        } => return validate_playlist_change(*selection, playlist, state),
        DurableChange::CueListChanged(list) => {
            return if state.cue_lists_complete() {
                cue_lists::validate_list(list, state)
            } else {
                Err(ModelError::InvalidCueList(list.id))
            };
        }
        DurableChange::CueListRemoved { list } => {
            return if state.cue_list(*list).is_some() {
                Ok(())
            } else {
                Err(ModelError::InvalidCueList(*list))
            };
        }
        DurableChange::ScriptChanged(script) => {
            // A new script must fit the project bound; a replaced one does.
            let fits =
                state.script(script.id).is_some() || state.scripts.len() < fm_protocol::MAX_SCRIPTS;
            return if state.scripts_complete() && fits {
                scripts::validate_script(script)
            } else {
                Err(ModelError::InvalidScript(script.id))
            };
        }
        DurableChange::ScriptRemoved { script }
        | DurableChange::ScriptRunChanged { script, .. } => {
            return if state.script(*script).is_some() {
                Ok(())
            } else {
                Err(ModelError::InvalidScript(*script))
            };
        }
        DurableChange::MediaListChanged {
            created, status, ..
        } => {
            return media_lists::validate_change(state, created.as_ref(), status);
        }
        DurableChange::ScriptRunsStopped { scripts, .. } => {
            return match scripts
                .iter()
                .find(|script| state.script(**script).is_none())
            {
                Some(script) => Err(ModelError::InvalidScript(*script)),
                None => Ok(()),
            };
        }
        DurableChange::ShortcutChanged(shortcut) => {
            // A new shortcut must fit the project bound; a replaced one does.
            let fits = state.shortcut(shortcut.id).is_some()
                || state.shortcuts.len() < fm_protocol::MAX_SHORTCUTS;
            return if state.shortcuts_complete() && fits {
                shortcuts::validate_shortcut(shortcut)
            } else {
                Err(ModelError::InvalidShortcut(shortcut.id))
            };
        }
        DurableChange::ShortcutRemoved { shortcut } => {
            return if state.shortcut(*shortcut).is_some() {
                Ok(())
            } else {
                Err(ModelError::InvalidShortcut(*shortcut))
            };
        }
        DurableChange::ShortcutFired(update) => {
            shortcuts::validate_fire(update, state)?;
            (
                update.effects.selection,
                update.effects.manual_transition,
                None,
                &update.effects.overlays,
                &update.effects.input_audio_strips,
            )
        }
        DurableChange::ScriptRan(update) => {
            scripts::validate_run(update, state)?;
            (
                update.effects.selection,
                update.effects.manual_transition,
                None,
                &update.effects.overlays,
                &update.effects.input_audio_strips,
            )
        }
        DurableChange::CueRunChanged(update) => {
            cue_lists::validate_run(update, state)?;
            (
                update.selection,
                update.manual_transition,
                None,
                &update.overlays,
                &update.input_audio_strips,
            )
        }
        DurableChange::DesiredSwitcher {
            selection,
            manual_transition,
            overlays,
            input_audio_strips,
            ..
        } => (
            *selection,
            *manual_transition,
            None,
            overlays,
            input_audio_strips,
        ),
        DurableChange::StingerSlotsChanged {
            selection,
            manual_transition,
            stingers,
            overlays,
            input_audio_strips,
            ..
        } => (
            *selection,
            *manual_transition,
            Some(stingers.as_slice()),
            overlays,
            input_audio_strips,
        ),
        DurableChange::StreamsChanged { streams }
        | DurableChange::StreamTargetChanged { streams, .. } => {
            validate_streams(streams)?;
            return Ok(());
        }
        DurableChange::RecordingChanged { .. }
        | DurableChange::FadeToBlackCurveChanged { .. }
        | DurableChange::EditLockChanged {
            target: EditLockTarget::Project,
            ..
        } => return Ok(()),
        DurableChange::PageNavigated { input } => {
            return if state.inputs.contains(input) {
                Ok(())
            } else {
                Err(ModelError::InvalidMediaChange(*input))
            };
        }
        DurableChange::MediaChanged { status } => {
            let input = status.input.to_domain();
            return if state.inputs.contains(&input) {
                Ok(())
            } else {
                Err(ModelError::InvalidMediaChange(input))
            };
        }
        DurableChange::TransitionFavoriteChanged { slot, .. } => {
            return if (1..=TRANSITION_FAVORITE_COUNT).contains(&usize::from(*slot)) {
                Ok(())
            } else {
                Err(ModelError::InvalidTransitionFavorite(*slot))
            };
        }
        DurableChange::NetworkStreamChanged { input, .. }
        | DurableChange::CaptureSettingsChanged { input, .. }
        | DurableChange::EditLockChanged {
            target: EditLockTarget::Input(input),
            ..
        } => {
            return if state.inputs.contains(input) {
                Ok(())
            } else {
                Err(ModelError::UnknownInput(*input))
            };
        }
    };
    let inputs = &state.inputs;
    for input in [selection.program, selection.preview] {
        if !inputs.contains(&input) {
            return Err(ModelError::UnknownInput(input));
        }
    }
    let input_set = inputs.iter().copied().collect();
    validate_manual_transition(manual_transition, selection, &input_set)?;
    if let Some(stingers) = stingers {
        validate_stingers(stingers, &input_set)?;
    }
    let output_set = state.outputs.iter().map(|output| output.output).collect();
    validate_overlays(overlays, &input_set, &output_set)?;
    validate_input_audio_strips(input_audio_strips, &input_set)?;
    Ok(())
}

/// Mirrors the server's removal rule over the replicated desired state:
/// Program, Preview, a manual transition endpoint, a Stinger slot's media,
/// an overlay queue entry, or the source of an overlay that is on air.
fn desired_input_in_use(state: &ProjectState, input: InputId) -> bool {
    let switcher = &state.switcher;
    let manual = match switcher.desired_manual_transition {
        ManualTransitionStatus::Active(transition) => {
            transition.from == input || transition.to == input
        }
        ManualTransitionStatus::Inactive => false,
    };
    switcher.desired.program == input
        || switcher.desired.preview == input
        || manual
        || state
            .stingers
            .iter()
            .any(|stinger| stinger.media_input == input)
        || state.desired_overlays.iter().any(|overlay| {
            overlay.queued_sources.contains(&input)
                || (overlay.source == Some(input) && (overlay.active || overlay.opacity > 0))
        })
}

fn validate_optimistic(change: OptimisticChange, inputs: &[InputId]) -> Result<(), ModelError> {
    for input in change.inputs().into_iter().flatten() {
        if !inputs.contains(&input) {
            return Err(ModelError::UnknownInput(input));
        }
    }
    Ok(())
}

/// Validates a live input addition or removal against the current catalogue.
fn validate_input_catalogue_change(
    change: &DurableChange,
    state: &ProjectState,
) -> Result<(), ModelError> {
    match change {
        DurableChange::InputAdded { input, name, .. } => {
            if state.inputs.contains(input) {
                return Err(ModelError::DuplicateInput(*input));
            }
            if !valid_input_name(name) || state.input_names.contains(name) {
                return Err(ModelError::InvalidInputNames);
            }
        }
        DurableChange::InputRemoved { input } => {
            if !state.inputs.contains(input) {
                return Err(ModelError::UnknownInput(*input));
            }
            if desired_input_in_use(state, *input)
                || state
                    .cue_lists
                    .iter()
                    .any(|list| cue_lists::list_inputs(list).any(|named| named == *input))
            {
                return Err(ModelError::InputInUse(*input));
            }
        }
        _ => unreachable!("only input catalogue changes are validated here"),
    }
    Ok(())
}

/// Appends an added input with a default strip.
fn apply_input_added(state: &mut ProjectState, input: InputId, name: String) {
    state.inputs.push(input);
    state.input_names.push(name);
    // Strips are kept in identifier order, exactly as snapshots and switcher
    // events carry them.
    let index = state
        .input_audio_strips
        .partition_point(|strip| strip.input < input);
    state.input_audio_strips.insert(
        index,
        InputAudioStripStatus {
            input,
            gain_millidb: 0,
            balance_basis_points: 0,
            pan_basis_points: 0,
            group: None,
            muted: false,
            soloed: false,
            follow_video: true,
            delay_samples: 0,
        },
    );
}

/// Drops a removed input with its name, strip, and lock.
fn apply_input_removed(state: &mut ProjectState, input: InputId) {
    let index = state
        .inputs
        .iter()
        .position(|candidate| *candidate == input)
        .expect("validated removed input must exist");
    state.inputs.remove(index);
    state.input_names.remove(index);
    // Its source entries leave with it, however it is removed (one input,
    // a batch, or an undone duplicate), so an input later added under the
    // same id is not shown as one.
    state.network_streams.remove(&input);
    state.capture_inputs.remove(&input);
    state.system_audio_inputs.remove(&input);
    state.still_inputs.remove(&input);
    state
        .input_audio_strips
        .retain(|strip| strip.input != input);
    state.audio_strip_dsp.remove(&input);
    state.edit_locks.set(EditLockTarget::Input(input), false);
    state.titles.retain(|title| title.input != input);
    state.scenes.retain(|scene| scene.input != input);
    state.grading.remove_input(input);
    state.media.remove(&input);
    state.media_lists.remove(&input);
    state.input_lifecycle.remove(&input);
    state
        .audio_mixer
        .apply(&fm_protocol::AudioMixerOperation::SetLabel {
            input: fm_protocol::WireInputId::from_domain(input),
            label: String::new(),
        });
    // A settled-off overlay forgets a removed input it retained.
    for overlay in state
        .desired_overlays
        .iter_mut()
        .chain(state.realized_overlays.iter_mut())
    {
        if overlay.source == Some(input) {
            overlay.source = None;
        }
    }
}

#[allow(clippy::too_many_lines)] // one match over every durable change
fn apply_change(state: &mut ProjectState, change: DurableChange) {
    match change {
        DurableChange::InputOrderChanged { inputs } => {
            let names = inputs
                .iter()
                .map(|input| {
                    let index = state
                        .inputs
                        .iter()
                        .position(|candidate| candidate == input)
                        .expect("validated input order must exist");
                    state.input_names[index].clone()
                })
                .collect();
            state.inputs = inputs;
            state.input_names = names;
        }
        DurableChange::InputRenamed { input, name } => {
            let index = state
                .inputs
                .iter()
                .position(|candidate| *candidate == input)
                .expect("validated rename input must exist");
            state.input_names[index] = name;
        }
        DurableChange::InputAdded {
            input,
            name,
            network_stream,
            capture,
            system_audio,
            still,
        } => {
            if let Some(source) = still {
                state.still_inputs.insert(
                    input,
                    StillInputStatus {
                        source: *source,
                        status: fm_protocol::SlideshowPageStatus::default(),
                        listing: None,
                    },
                );
            }
            if let Some(config) = system_audio {
                state.system_audio_inputs.insert(
                    input,
                    fm_protocol::SystemAudioEntry {
                        input: fm_protocol::WireInputId::from_domain(input),
                        config: *config,
                        status: fm_protocol::SystemAudioStatus::default(),
                    },
                );
            }
            if let Some(config) = capture {
                state.capture_inputs.insert(
                    input,
                    fm_protocol::CaptureInputEntry {
                        input: fm_protocol::WireInputId::from_domain(input),
                        config: *config,
                        status: fm_protocol::CaptureStatus::default(),
                    },
                );
            }
            if let Some(config) = network_stream {
                state.network_streams.insert(
                    input,
                    fm_protocol::NetworkStreamEntry {
                        input: fm_protocol::WireInputId::from_domain(input),
                        config: *config,
                        status: fm_protocol::NetworkStreamStatus::default(),
                    },
                );
            }
            apply_input_added(state, input, name);
        }
        DurableChange::InputRemoved { input } => {
            apply_input_removed(state, input);
        }
        DurableChange::CaptureSettingsChanged { input, settings } => {
            if let Some(entry) = state.capture_inputs.get_mut(&input) {
                entry.config.settings = *settings;
            }
        }
        DurableChange::NetworkStreamChanged { input, config } => {
            let entry = state.network_streams.entry(input).or_insert_with(|| {
                fm_protocol::NetworkStreamEntry {
                    input: fm_protocol::WireInputId::from_domain(input),
                    config: (*config).clone(),
                    status: fm_protocol::NetworkStreamStatus::default(),
                }
            });
            entry.config = *config;
        }
        DurableChange::InputLifecycleChanged { operation, added } => {
            if let fm_protocol::InputLifecycleOperation::ReplaceSource { input, source } =
                &operation
            {
                let input = input.to_domain();
                match source {
                    fm_protocol::WireInputSource::Network { endpoint } => {
                        state.network_streams.insert(
                            input,
                            fm_protocol::NetworkStreamEntry {
                                input: fm_protocol::WireInputId::from_domain(input),
                                config: fm_protocol::NetworkStreamConfig::new(endpoint.clone()),
                                status: fm_protocol::NetworkStreamStatus::default(),
                            },
                        );
                    }
                    _ => {
                        state.network_streams.remove(&input);
                    }
                }
                // A device keeps the settings of a device it replaces, as
                // the engine does (IN-001).
                match source {
                    fm_protocol::WireInputSource::Device { stable_key } => {
                        let entry = state.capture_inputs.entry(input).or_insert_with(|| {
                            fm_protocol::CaptureInputEntry {
                                input: fm_protocol::WireInputId::from_domain(input),
                                config: fm_protocol::CaptureInputConfig::new(stable_key.clone()),
                                status: fm_protocol::CaptureStatus::default(),
                            }
                        });
                        entry.config.stable_key.clone_from(stable_key);
                    }
                    _ => {
                        state.capture_inputs.remove(&input);
                    }
                }
                let still = match source {
                    fm_protocol::WireInputSource::Image(config) => {
                        Some(fm_protocol::WireStillSource::Image(config.clone()))
                    }
                    fm_protocol::WireInputSource::Slideshow(config) => {
                        Some(fm_protocol::WireStillSource::Slideshow((**config).clone()))
                    }
                    _ => None,
                };
                match still {
                    Some(source) => {
                        // A replaced slideshow keeps reporting its slide
                        // until the next `slideshow_page`.
                        let entry =
                            state
                                .still_inputs
                                .entry(input)
                                .or_insert_with(|| StillInputStatus {
                                    source: source.clone(),
                                    status: fm_protocol::SlideshowPageStatus::default(),
                                    listing: None,
                                });
                        entry.source = source;
                        entry.listing = None;
                    }
                    None => {
                        state.still_inputs.remove(&input);
                    }
                }
                match source {
                    fm_protocol::WireInputSource::SystemAudio(config) => {
                        state.system_audio_inputs.insert(
                            input,
                            fm_protocol::SystemAudioEntry {
                                input: fm_protocol::WireInputId::from_domain(input),
                                config: config.clone(),
                                status: fm_protocol::SystemAudioStatus::default(),
                            },
                        );
                    }
                    _ => {
                        state.system_audio_inputs.remove(&input);
                    }
                }
            }
            if let fm_protocol::InputLifecycleOperation::Duplicate { source, input, .. } =
                &operation
                && let Some(original) = state.still_inputs.get(&source.to_domain())
            {
                let copy = StillInputStatus {
                    source: original.source.clone(),
                    status: fm_protocol::SlideshowPageStatus::default(),
                    listing: None,
                };
                state.still_inputs.insert(input.to_domain(), copy);
            }
            if let fm_protocol::InputLifecycleOperation::Duplicate { source, input, .. } =
                &operation
                && let Some(original) = state.system_audio_inputs.get(&source.to_domain())
            {
                let copy = fm_protocol::SystemAudioEntry {
                    input: *input,
                    config: original.config.clone(),
                    status: fm_protocol::SystemAudioStatus::default(),
                };
                state.system_audio_inputs.insert(input.to_domain(), copy);
            }
            if let fm_protocol::InputLifecycleOperation::Duplicate { source, input, .. } =
                &operation
                && let Some(original) = state.capture_inputs.get(&source.to_domain())
            {
                let copy = fm_protocol::CaptureInputEntry {
                    input: *input,
                    config: original.config.clone(),
                    status: fm_protocol::CaptureStatus::default(),
                };
                state.capture_inputs.insert(input.to_domain(), copy);
            }
            input_lifecycle::apply(state, operation, added);
        }
        DurableChange::AudioMixerChanged(operation) => {
            if let fm_protocol::AudioMixerOperation::SetGroup { input, group } = &operation
                && let Some(strip) = state
                    .input_audio_strips
                    .iter_mut()
                    .find(|strip| strip.input == input.to_domain())
            {
                strip.group = *group;
            }
            if let fm_protocol::AudioMixerOperation::SetStripDsp {
                target: fm_protocol::AudioDspTarget::Input(input),
                chain,
                ..
            } = &operation
            {
                if chain.is_default() {
                    state.audio_strip_dsp.remove(&input.to_domain());
                } else {
                    state.audio_strip_dsp.insert(input.to_domain(), **chain);
                }
            }
            state.audio_mixer.apply(&operation);
        }
        DurableChange::DisplayOutputChanged { slot, config } => match config {
            Some(config) => {
                state.display_outputs.insert(slot, *config);
            }
            None => {
                state.display_outputs.remove(&slot);
            }
        },
        DurableChange::LiveLanChanged(entry) => {
            // Disabled, it forgets its last report, as the server does.
            if !entry.enabled {
                state.live_lan_status = fm_protocol::LiveLanStatus::default();
            }
            state.live_lan = entry;
        }
        DurableChange::FadeToBlackOutputChanged { output, affected } => {
            state
                .fade_to_black_excluded_outputs
                .retain(|candidate| *candidate != output);
            if !affected {
                let index = state
                    .fade_to_black_excluded_outputs
                    .partition_point(|candidate| candidate.get() < output.get());
                state.fade_to_black_excluded_outputs.insert(index, output);
            }
        }
        DurableChange::FadeToBlackOutputDurationChanged {
            output,
            duration_frames,
        } => {
            state
                .fade_to_black_output_durations
                .retain(|(candidate, _)| *candidate != output);
            if let Some(frames) = duration_frames {
                let index = state
                    .fade_to_black_output_durations
                    .partition_point(|(candidate, _)| candidate.get() < output.get());
                state
                    .fade_to_black_output_durations
                    .insert(index, (output, frames));
            }
        }
        DurableChange::FadeToBlackCurveChanged { curve } => {
            state.fade_to_black_curve = curve;
        }
        DurableChange::DesiredSwitcher {
            selection,
            manual_transition,
            fade_to_black,
            overlays,
            input_audio_strips,
        } => {
            state.switcher.desired = selection;
            state.switcher.desired_manual_transition = manual_transition;
            state.switcher.desired_fade_to_black = fade_to_black;
            state.desired_overlays = overlays;
            state.input_audio_strips = input_audio_strips;
        }
        DurableChange::StingerSlotsChanged {
            selection,
            manual_transition,
            fade_to_black,
            stingers,
            overlays,
            input_audio_strips,
        } => {
            state.switcher.desired = selection;
            state.switcher.desired_manual_transition = manual_transition;
            state.switcher.desired_fade_to_black = fade_to_black;
            state.stingers = stingers;
            state.desired_overlays = overlays;
            state.input_audio_strips = input_audio_strips;
        }
        DurableChange::StreamsChanged { streams } => {
            state.streams = streams;
        }
        DurableChange::StreamTargetChanged {
            target,
            entry,
            streams,
        } => {
            state.streams = streams;
            match entry {
                Some(entry) => match state
                    .stream_targets
                    .iter_mut()
                    .find(|existing| existing.target.get() == target)
                {
                    Some(existing) => *existing = *entry,
                    None => state.stream_targets.push(*entry),
                },
                None => state
                    .stream_targets
                    .retain(|existing| existing.target.get() != target),
            }
            // The roster carries the authored order: an undone removal (or a
            // positioned edit) puts its destination back at its place, not
            // at the end where a new entry is appended.
            let place = |target: NonZeroU128| {
                state
                    .streams
                    .iter()
                    .position(|stream| stream.target == target)
                    .unwrap_or(usize::MAX)
            };
            state
                .stream_targets
                .sort_by_key(|entry| place(entry.target.get()));
        }

        DurableChange::RecordingChanged { recorder, active } => {
            let status = state
                .recordings
                .iter_mut()
                .find(|status| status.recorder == recorder)
                .expect("validated recorder roster contains every slot");
            status.desired_active = active;
        }
        DurableChange::EditLockChanged { target, locked } => state.edit_locks.set(target, locked),
        DurableChange::TitleChanged {
            input,
            field_revision,
            fields,
            overlays,
        } => {
            let title = state
                .titles
                .iter_mut()
                .find(|title| title.input == input)
                .expect("validated title change names a title");
            title.field_revision = field_revision;
            for (current, update) in title.fields.iter_mut().zip(fields) {
                current.value = update.value;
            }
            state.desired_overlays = overlays;
        }
        DurableChange::PlaylistChanged {
            selection,
            playlist,
        } => {
            state.switcher.desired = selection;
            state.playlist = Some(playlist);
        }
        DurableChange::CueListChanged(list) => cue_lists::apply_list(state, *list),
        DurableChange::CueListRemoved { list } => {
            state.cue_lists.retain(|candidate| candidate.id != list);
        }
        DurableChange::CueRunChanged(update) => cue_lists::apply_run(state, *update),
        DurableChange::ScriptChanged(script) => {
            // An accepted edit clears the edited script's fault, whose line
            // named the old source (Protocol 2.62).
            if matches!(
                state.script_run_states.get(&script.id),
                Some(fm_protocol::ScriptRunStatus::Faulted { .. })
            ) {
                state.script_run_states.remove(&script.id);
            }
            scripts::apply_script(state, *script);
        }
        DurableChange::ScriptRemoved { script } => {
            state.scripts.retain(|candidate| candidate.id != script);
            state.script_runs.retain(|run| run.script != script);
            state.script_run_states.remove(&script);
        }
        DurableChange::ScriptRan(update) => scripts::apply_run(state, *update),
        DurableChange::ScriptRunChanged { script, run, .. } => {
            scripts::set_run_status(state, script, run);
        }
        DurableChange::ScriptRunsStopped { .. } => state.script_run_states.clear(),
        DurableChange::ShortcutChanged(shortcut) => shortcuts::apply_shortcut(state, *shortcut),
        DurableChange::ShortcutRemoved { shortcut } => {
            state.shortcuts.retain(|candidate| candidate.id != shortcut);
        }
        DurableChange::ShortcutFired(update) => shortcuts::apply_fire(state, *update),
        DurableChange::MediaListChanged {
            created, status, ..
        } => media_lists::apply_change(state, created, *status),
        DurableChange::GradeChanged(change) => state.grading.apply(change),
        DurableChange::AnimationPresetChanged(change) => state.animation_presets.apply(change),
        DurableChange::TransitionFavoriteChanged { slot, favorite } => {
            state.transition_favorites[usize::from(slot) - 1] = favorite;
        }
        DurableChange::MediaChanged { status } => {
            state.media.insert(status.input.to_domain(), *status);
        }
        DurableChange::PageNavigated { .. } => {}
        DurableChange::TitleAnimationChanged { inputs, animation } => {
            for title in &mut state.titles {
                if inputs.contains(&title.input) {
                    title.animation = Some(animation.clone());
                }
            }
        }
        DurableChange::SceneAnimationChanged { inputs, animation } => {
            for scene in &mut state.scenes {
                if inputs.contains(&scene.input) {
                    scene.animation = Some(animation.clone());
                }
            }
        }
        DurableChange::SceneLayoutChanged { scene, layout, .. } => {
            for entry in &mut state.scene_layouts {
                if entry.scene == scene {
                    entry.layout = Some(Arc::clone(&layout));
                }
            }
            for shown in &mut state.scenes {
                if shown.scene == Some(scene) {
                    shown.layout = Some(Arc::clone(&layout));
                }
            }
        }
    }
}

fn protocol_stingers(stingers: Vec<fm_protocol::StingerStatus>) -> Vec<StingerStatus> {
    stingers
        .into_iter()
        .map(|status| StingerStatus {
            slot: status.slot.number(),
            media_input: status.media_input.to_domain(),
            preload: status.preload,
            cut_point_frames: status.cut_point_frames,
            audio_policy: status.audio_policy,
            missing_media_fallback: status.missing_media_fallback,
            readiness: status.readiness,
        })
        .collect()
}

/// Validates a replicated mixer operation against the current project.
fn validate_audio_mixer_change(
    operation: &fm_protocol::AudioMixerOperation,
    state: &ProjectState,
) -> Result<(), ModelError> {
    let known = |input: &fm_protocol::WireInputId| state.inputs.contains(&input.to_domain());
    let valid = match operation {
        fm_protocol::AudioMixerOperation::SetLabel { input, label } => {
            known(input) && fm_protocol::audio_label_is_valid(label, true)
        }
        fm_protocol::AudioMixerOperation::SetGroup { input, group } => {
            known(input) && group.is_none_or(fm_protocol::audio_group_is_valid)
        }
        fm_protocol::AudioMixerOperation::RenameGroup { group, name } => {
            fm_protocol::audio_group_is_valid(*group)
                && fm_protocol::audio_label_is_valid(name, false)
        }
        fm_protocol::AudioMixerOperation::SetGroupLevel { group, .. }
        | fm_protocol::AudioMixerOperation::SetGroupSolo { group, .. } => {
            fm_protocol::audio_group_is_valid(*group)
        }
        fm_protocol::AudioMixerOperation::SetGroupParent { group, parent } => {
            fm_protocol::audio_group_is_valid(*group)
                && parent.is_none_or(fm_protocol::audio_group_is_valid)
                && state.audio_mixer.parent_is_acyclic(*group, *parent)
        }
        fm_protocol::AudioMixerOperation::SetSoloMode { .. } => true,
        fm_protocol::AudioMixerOperation::SetMonitorDevice { device } => device
            .as_deref()
            .is_none_or(fm_protocol::audio_monitor_device_is_valid),
        fm_protocol::AudioMixerOperation::SetMonitorLevel { gain_millidb, .. } => {
            (-96_000..=24_000).contains(gain_millidb)
        }
        fm_protocol::AudioMixerOperation::SetStripDsp { target, chain, .. } => {
            chain.validate().is_ok()
                && match target {
                    fm_protocol::AudioDspTarget::Master => true,
                    fm_protocol::AudioDspTarget::Input(input) => known(input),
                }
        }
    };
    if valid {
        Ok(())
    } else {
        Err(ModelError::InvalidAudioMixerChange)
    }
}

fn protocol_input_audio_strips(
    strips: Vec<fm_protocol::InputAudioStripStatus>,
) -> Vec<InputAudioStripStatus> {
    strips
        .into_iter()
        .map(|status| InputAudioStripStatus {
            input: status.input.to_domain(),
            gain_millidb: status.gain_millidb,
            balance_basis_points: status.balance_basis_points,
            pan_basis_points: status.pan_basis_points,
            group: status.group,
            muted: status.muted,
            soloed: status.soloed,
            follow_video: status.follow_video,
            delay_samples: status.delay_samples,
        })
        .collect()
}

fn protocol_overlays(overlays: Vec<fm_protocol::OverlayStatus>) -> Vec<OverlayStatus> {
    overlays
        .into_iter()
        .map(|status| OverlayStatus {
            channel: status.channel.number(),
            source: status.source.map(fm_protocol::WireInputId::to_domain),
            active: status.active,
            opacity: status.opacity,
            transition: status.transition,
            duration_frames: status.duration_frames,
            position: status.position,
            border: status.border,
            queued_sources: status
                .queued_sources
                .into_iter()
                .map(fm_protocol::WireInputId::to_domain)
                .collect(),
            included_outputs: status
                .included_outputs
                .into_iter()
                .map(fm_protocol::WireOutputId::to_domain)
                .collect(),
        })
        .collect()
}

fn protocol_streams(streams: Vec<fm_protocol::StreamStatus>) -> Vec<StreamStatus> {
    streams
        .into_iter()
        .map(|status| StreamStatus {
            target: status.target.get(),
            name: status.name,
            desired_running: status.desired_running,
            realized: status.realized,
            detail: status.detail,
        })
        .collect()
}

fn manual_status_from_protocol(status: ProtocolManualTransitionStatus) -> ManualTransitionStatus {
    ManualTransitionStatus::from_protocol(status)
}

fn fade_to_black_from_protocol(state: FadeToBlackState) -> FadeToBlackState {
    state
}

fn validate_manual_transition(
    status: ManualTransitionStatus,
    selection: BusSelection,
    inputs: &HashSet<InputId>,
) -> Result<(), ModelError> {
    let ManualTransitionStatus::Active(state) = status else {
        return Ok(());
    };
    for input in [state.from, state.to] {
        if !inputs.contains(&input) {
            return Err(ModelError::UnknownInput(input));
        }
    }
    if state.from != selection.program || state.to != selection.preview {
        return Err(ModelError::InvalidManualTransitionRouting);
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// An image or slideshow input's replicated source and, for a slideshow,
/// the slide on air and the part of its listing fetched so far (IN-008,
/// IN-009).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StillInputStatus {
    pub source: fm_protocol::WireStillSource,
    pub status: fm_protocol::SlideshowPageStatus,
    /// The listing windows fetched at the current generation; `None` until
    /// a `slideshow_slides` reply for it arrives.
    pub listing: Option<SlideshowListing>,
}

/// The fetched windows of a slideshow's listing at one generation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SlideshowListing {
    /// How many slides the listing holds.
    pub total: u32,
    /// The fetched slides by position.
    pub slides: BTreeMap<u32, fm_protocol::SlideshowSlide>,
}

impl SlideshowListing {
    fn apply(&mut self, message: &fm_protocol::SlideshowSlidesMessage) {
        self.total = message.total;
        self.slides.retain(|index, _| *index < message.total);
        for (offset, slide) in (message.offset..).zip(&message.slides) {
            self.slides.insert(offset, slide.clone());
        }
    }

    /// The first position not fetched yet, if any.
    #[must_use]
    pub fn next_missing(&self) -> Option<u32> {
        (0..self.total).find(|index| !self.slides.contains_key(index))
    }
}
