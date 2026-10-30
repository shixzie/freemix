use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use fm_clock::{Clock, ClockDomainId, ClockTime, ManualClock};
use fm_command::{
    ApplyOutcome, CommandEnvelope, CommandReceipt, CommandState, EventSequence, FieldIssue,
    IdempotencyKey, Mutation, RefusalReason, Rejection, RejectionCode, Revision, RuntimeGeneration,
    StateEpoch,
};
use fm_scheduler::{FrameNumber, FrameScheduler, PlanGeneration};
use fm_switcher::{
    FadeToBlackCurve, FadeToBlackFrame, FadeToBlackPosition, InputUsage, OVERLAY_CHANNEL_COUNT,
    OverlayBorderPreset, OverlayChannelId, OverlayChannelState, OverlayPositionPreset,
    OverlayTransitionKind, ProgramFrame, RecorderSlot, StingerDescriptor, StingerSlotId,
    StreamTargetId, SwitcherCommand, SwitcherError, SwitcherEvent, SwitcherState, TBarPosition,
    TransitionKind,
};
use fm_titles::fm_animation::Phase;
use fm_titles::{FieldId, FieldValue, TitleAir, TitleAirFrame, TitleAirPhase, TitleAirTiming};
use fm_types::{FrameRate, InputId, InputOrderError, OutputId, RenameInputError, TargetBindings};

use crate::history::{EditHistory, HistoryStep, ResolvedCommand, check_edit_locks};
use crate::playlist::{
    EnginePlaylistOperation, PlaylistCue, PlaylistStep, apply_desired_cue, cue_transition_kind,
    is_playlist_command, playlist_due_frame, playlist_step, realize_cue,
};
use crate::{
    EditHistoryEntry, EditLockScope, EngineError, EngineGradeEdit, EngineInputAudioStripState,
    EngineInputSource, InputCatalogueError, ShowState, SnapshotError, TitleRejection,
};

type RuntimeScheduler = FrameScheduler<(), (), EngineCommand, ()>;
const MAX_TRANSITION_DURATION_FRAMES: u32 = 3_600;
/// Maximum idempotency receipts an engine retains in memory.
///
/// Retention keeps the newest receipts by the revision they were recorded at;
/// a rejection recorded at revision `r` ranks just after the accepted command
/// that produced `r`, and ties break by key. The receipt a command just
/// produced is never evicted. Resubmitting an evicted key executes it as a new
/// command. Persistence stores at most the same bound, so snapshots and
/// restores round-trip the retained window.
pub const MAX_RETAINED_RECEIPTS: usize = 1_024;
const RECEIPT_LIMIT: NonZeroUsize = NonZeroUsize::new(MAX_RETAINED_RECEIPTS).unwrap();
pub const MAX_INPUT_AUDIO_DELAY_SAMPLES: u32 = 48_000;
pub const MIN_INPUT_AUDIO_GAIN_MILLIDB: i32 = -96_000;
pub const MAX_INPUT_AUDIO_GAIN_MILLIDB: i32 = 24_000;
pub const MIN_INPUT_AUDIO_BALANCE_BASIS_POINTS: i32 = -10_000;
pub const MAX_INPUT_AUDIO_BALANCE_BASIS_POINTS: i32 = 10_000;

/// The kind of a manual (T-bar) transition: every timed mix, or a Stinger
/// scrubbing its clip (SW-004).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineManualTransitionKind {
    Fade,
    Wipe,
    AlphaFade,
    Slide,
    VerticalWipe,
    VerticalSlide,
    Zoom,
    Fly,
    FlyRotate,
    Cube,
    CubeZoom,
    CrossZoom,
    Merge,
    /// Stinger `slot` scrubbing a `duration_frames` clip: the position names
    /// the clip frame and the base cuts at the slot's cut point.
    Stinger {
        slot: StingerSlotId,
        duration_frames: u32,
    },
}

impl EngineManualTransitionKind {
    /// Every mix kind, in operator order.
    pub const MIXES: [Self; 13] = [
        Self::Fade,
        Self::AlphaFade,
        Self::Wipe,
        Self::VerticalWipe,
        Self::Slide,
        Self::VerticalSlide,
        Self::Zoom,
        Self::Fly,
        Self::FlyRotate,
        Self::Cube,
        Self::CubeZoom,
        Self::CrossZoom,
        Self::Merge,
    ];

    /// The switcher kind this manual transition draws with.
    #[must_use]
    pub const fn transition_kind(self) -> TransitionKind {
        match self {
            Self::Fade => TransitionKind::Fade,
            Self::Wipe => TransitionKind::Wipe,
            Self::AlphaFade => TransitionKind::AlphaFade,
            Self::Slide => TransitionKind::Slide,
            Self::VerticalWipe => TransitionKind::VerticalWipe,
            Self::VerticalSlide => TransitionKind::VerticalSlide,
            Self::Zoom => TransitionKind::Zoom,
            Self::Fly => TransitionKind::Fly,
            Self::FlyRotate => TransitionKind::FlyRotate,
            Self::Cube => TransitionKind::Cube,
            Self::CubeZoom => TransitionKind::CubeZoom,
            Self::CrossZoom => TransitionKind::CrossZoom,
            Self::Merge => TransitionKind::Merge,
            Self::Stinger { slot, .. } => TransitionKind::Stinger(slot),
        }
    }

    /// The manual kind of a switcher T-bar state.
    #[must_use]
    pub const fn from_t_bar(state: fm_switcher::TBarState) -> Self {
        match state.kind() {
            TransitionKind::Fade => Self::Fade,
            TransitionKind::Wipe => Self::Wipe,
            TransitionKind::AlphaFade => Self::AlphaFade,
            TransitionKind::Slide => Self::Slide,
            TransitionKind::VerticalWipe => Self::VerticalWipe,
            TransitionKind::VerticalSlide => Self::VerticalSlide,
            TransitionKind::Zoom => Self::Zoom,
            TransitionKind::Fly => Self::Fly,
            TransitionKind::FlyRotate => Self::FlyRotate,
            TransitionKind::Cube => Self::Cube,
            TransitionKind::CubeZoom => Self::CubeZoom,
            TransitionKind::CrossZoom => Self::CrossZoom,
            TransitionKind::Merge => Self::Merge,
            TransitionKind::Stinger(slot) => Self::Stinger {
                slot,
                duration_frames: state.clip_frames(),
            },
        }
    }

    /// The switcher T-bar state this kind starts between `from` and `to`.
    #[must_use]
    pub const fn t_bar_state(
        self,
        from: InputId,
        to: InputId,
        interval_start: EngineManualTransitionPosition,
        position: EngineManualTransitionPosition,
    ) -> fm_switcher::TBarState {
        let state = fm_switcher::TBarState::restore(
            self.transition_kind(),
            from,
            to,
            interval_start.domain(),
            position.domain(),
        );
        match self {
            Self::Stinger {
                duration_frames, ..
            } => state.with_clip_frames(duration_frames),
            _ => state,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EngineManualTransitionPosition(TBarPosition);

impl EngineManualTransitionPosition {
    pub const MAX: u16 = TBarPosition::MAX;
    pub const START: Self = Self(TBarPosition::START);
    pub const END: Self = Self(TBarPosition::END);

    #[must_use]
    pub const fn new(basis_points: u16) -> Option<Self> {
        match TBarPosition::new(basis_points) {
            Some(position) => Some(Self(position)),
            None => None,
        }
    }

    #[must_use]
    pub const fn basis_points(self) -> u16 {
        self.0.basis_points()
    }

    pub(crate) const fn domain(self) -> TBarPosition {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineManualTransitionState {
    pub kind: EngineManualTransitionKind,
    pub from: InputId,
    pub to: InputId,
    pub interval_start: EngineManualTransitionPosition,
    pub position: EngineManualTransitionPosition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineFadeToBlackState {
    pub active: bool,
    pub position: FadeToBlackPosition,
}

/// Whether a title field edit requires its title to be off air or on air.
///
/// A title is on air while it is on Program, an endpoint of an automatic or
/// manual transition, the media of a Stinger transition in flight, or the
/// source of an overlay channel that is shown or still fading, in either the
/// desired or the realized switcher. An overlay
/// fading out therefore keeps its title on air until the fade completes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleAirRequirement {
    /// Prepare (save) a title that is not on air.
    OffAir,
    /// Update a title live while it is on air.
    OnAir,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineCommand {
    ReorderInputs {
        inputs: Vec<InputId>,
    },
    RenameInput {
        input: InputId,
        name: String,
    },
    /// Appends a new input with a default Master strip and no padlock.
    ///
    /// The source is carried for durability and redo; the engine itself
    /// renders nothing.
    AddInput {
        input: InputId,
        name: String,
        source: EngineInputSource,
    },
    /// Removes an input that nothing on air or configured references.
    RemoveInput {
        input: InputId,
    },
    SetInputAudioStrip {
        input: InputId,
        state: EngineInputAudioStripState,
    },
    SelectPreview(InputId),
    Cut,
    Fade {
        duration_frames: u32,
    },
    AlphaFade {
        duration_frames: u32,
    },
    Slide {
        duration_frames: u32,
    },
    Zoom {
        duration_frames: u32,
    },
    /// Any automatic mix transition by kind (SW-002): the whole family,
    /// including the kinds without their own command. A Stinger kind is
    /// refused; Stingers take [`Self::Stinger`].
    Transition {
        kind: TransitionKind,
        duration_frames: u32,
    },
    Stinger {
        slot: StingerSlotId,
        duration_frames: u32,
    },
    ConfigureStinger {
        slot: StingerSlotId,
        descriptor: StingerDescriptor,
    },
    RemoveStinger {
        slot: StingerSlotId,
    },
    TakeOverlay {
        channel: OverlayChannelId,
        source: InputId,
    },
    UpdateOverlay {
        channel: OverlayChannelId,
        source: InputId,
    },
    OverlayOff {
        channel: OverlayChannelId,
    },
    ConfigureOverlayTransition {
        channel: OverlayChannelId,
        transition: OverlayTransitionKind,
        duration_frames: u32,
    },
    ConfigureOverlayAppearance {
        channel: OverlayChannelId,
        position: OverlayPositionPreset,
        border: OverlayBorderPreset,
    },
    QueueOverlay {
        channel: OverlayChannelId,
        source: InputId,
    },
    TakeNextOverlay {
        channel: OverlayChannelId,
    },
    SetOverlayOutputInclusion {
        channel: OverlayChannelId,
        output: OutputId,
        included: bool,
    },
    Wipe {
        duration_frames: u32,
    },
    FadeToBlack {
        active: bool,
        duration_frames: u32,
    },
    StartManualTransition {
        kind: EngineManualTransitionKind,
    },
    SetManualTransitionPosition {
        position: EngineManualTransitionPosition,
    },
    CommitManualTransition,
    CancelManualTransition,
    StreamStart {
        target: StreamTargetId,
    },
    StreamStop {
        target: StreamTargetId,
    },
    /// Starts or stops every destination at once, or reconnects one now
    /// (OR-006). A live operation like [`Self::StreamStart`]; never affects
    /// recording.
    StreamControl(EngineStreamControl),
    /// Adds or reconfigures (`Some`) or removes (`None`) stream destination
    /// `target` (OR-006). A project edit honouring the project padlock,
    /// undone by restoring the previous configuration at its previous
    /// `position` in the authored list; refused while the destination is
    /// desired running. `position: None` keeps an existing destination in
    /// place and appends a new one.
    SetStreamTarget {
        target: StreamTargetId,
        spec: Option<Box<crate::EngineStreamTargetSpec>>,
        position: Option<usize>,
    },
    RecordStart {
        recorder: RecorderSlot,
    },
    RecordStop {
        recorder: RecorderSlot,
    },
    /// Reverts the most recent project edit in the shared history.
    ///
    /// `expected_edit` names the [`EditHistoryEntry::edit`] the caller means
    /// to revert; a different top entry is rejected as a history conflict.
    Undo {
        expected_edit: Option<Revision>,
    },
    /// Reapplies the most recently undone project edit.
    ///
    /// `expected_edit` names the [`EditHistoryEntry::edit`] the caller means
    /// to reapply; a different top entry is rejected as a history conflict.
    Redo {
        expected_edit: Option<Revision>,
    },
    /// Engages or releases an edit padlock. Live operations are never locked.
    SetEditLock {
        scope: EditLockScope,
        locked: bool,
    },
    /// Atomically sets title field values when `base_revision` is the title's
    /// current field revision and the title's air state meets `require`.
    ///
    /// An empty field list is accepted without advancing the revision.
    SetTitleFields {
        input: InputId,
        base_revision: u64,
        require: TitleAirRequirement,
        fields: Vec<(FieldId, FieldValue)>,
    },
    /// Applies field values to an off-air title and takes it onto an overlay
    /// channel. The fields change at acceptance, before the overlay realizes,
    /// so the first frame on air already shows them.
    TakeTitle {
        input: InputId,
        channel: OverlayChannelId,
        base_revision: u64,
        fields: Vec<(FieldId, FieldValue)>,
    },
    /// Takes a title off the single overlay channel that shows it.
    TitleOffAir {
        input: InputId,
    },
    /// Replaces the playlist's entries and loop mode while it is stopped or
    /// finished. Durations are fixed and, like transition durations, in
    /// frames. A project edit without an undo entry.
    SetPlaylist {
        entries: Vec<fm_automation::PlaylistEntry>,
        loop_mode: fm_automation::LoopMode,
    },
    /// Controls the playlist on the command's target frame. An operation
    /// that puts an entry on air also takes its input to Program.
    PlaylistControl(EnginePlaylistOperation),
    /// The playlist clock: processes every playlist boundary up to `frame`
    /// and takes the entry then on air to Program on exactly that frame.
    ///
    /// `frame` must equal [`Engine::playlist_advance_frame`]: the due
    /// boundary, or the next frame when the boundary has already passed. The
    /// command is accepted ahead of `frame` and realized on it, and no other
    /// command is accepted until it has been realized.
    PlaylistAdvance {
        frame: u64,
    },
    /// The runtime action of an accepted playlist command. Never accepted
    /// as a command.
    RealizePlaylistCue(Option<PlaylistCue>),
    /// The runtime action of an accepted [`Self::TakeTitle`] or
    /// [`Self::TitleOffAir`]. Never accepted as a command.
    RealizeTitle(TitleCue),
    /// Edits the project grading (see [`EngineGradeEdit`]). A project edit.
    Grade(EngineGradeEdit),
    /// Edits a title template's animation (see
    /// [`crate::EngineTitleAnimationEdit`]). A project edit.
    TitleAnimation(crate::EngineTitleAnimationEdit),
    /// Shows or hides one title template field in the browser title editor
    /// (see [`crate::EngineTitleExposureEdit`]). A project edit that is not
    /// undoable.
    TitleExposure(crate::EngineTitleExposureEdit),
    /// Edits a scene's layer animation (see
    /// [`crate::EngineSceneAnimationEdit`]). A project edit.
    SceneAnimation(crate::EngineSceneAnimationEdit),
    /// Edits the animation preset library (see
    /// [`crate::EngineAnimationPresetEdit`]). A project edit.
    AnimationPreset(crate::EngineAnimationPresetEdit),
    /// Assigns one of the project's favourite transition buttons (SW-002).
    /// A project edit, undone by restoring the previous favourite.
    SetTransitionFavorite {
        /// One-based slot, 1 through [`fm_model::TRANSITION_FAVORITE_COUNT`].
        slot: u8,
        favorite: fm_model::TransitionFavorite,
    },
    /// Controls a media-file input (IN-003). Transport operations are live
    /// operations; settings changes are project edits (padlocked, without
    /// an undo entry). Realized on the target frame (see [`crate::media`]).
    Media(crate::EngineMediaCommand),
    /// Moves a paged input to another page: a slideshow (IN-009), realized
    /// on the target frame (see [`crate::slideshow`]), or a document
    /// (IN-014), whose durable page moves at once, resolved against the page
    /// count the media plane reported (see [`crate::paged`]). A live
    /// operation.
    PageNavigate {
        input: InputId,
        navigation: fm_types::paged::PagedNavigation,
    },
    /// Creates, edits, or steps a media list input (IN-006; see
    /// [`crate::media_list`]). Creating and editing are project edits;
    /// navigation and advances are live operations.
    MediaList(crate::EngineMediaListCommand),
    /// Edits an input's lifecycle or the input catalogue (IN-026; see
    /// [`crate::EngineInputLifecycleEdit`]). A project edit.
    InputLifecycle(crate::EngineInputLifecycleEdit),
    /// Edits one layer of a scene live (SW-006; see
    /// [`crate::EngineSceneLayerEdit`]). A project edit.
    SceneLayer(crate::EngineSceneLayerEdit),
    /// Labels a mixer strip, assigns it to a group, renames a group (project
    /// edits), or moves a group fader or mute (a live operation) (AU-001;
    /// see [`crate::EngineAudioMixerEdit`]).
    AudioMixer(crate::EngineAudioMixerEdit),
    /// Chooses whether Fade to Black fades `output` (SW-004). A project
    /// edit, undone by restoring the previous choice.
    SetFadeToBlackOutput {
        output: OutputId,
        affected: bool,
    },
    /// Gives `output` its own Fade-to-Black duration (`Some`, 1 through
    /// 3600 frames) or returns it to the requested one (`None`) (SW-004). A
    /// project edit, undone by restoring the previous duration.
    SetFadeToBlackOutputDuration {
        output: OutputId,
        duration_frames: Option<u32>,
    },
    /// Chooses how automatic Fade-to-Black moves ease (SW-004). A project
    /// edit, undone by restoring the previous curve.
    SetFadeToBlackCurve {
        curve: FadeToBlackCurve,
    },
    /// Configures (`Some`) or clears (`None`) one fullscreen display output
    /// slot (OR-011). A project edit, undone by restoring the slot's
    /// previous configuration; the daemon realizes it live.
    SetDisplayOutput {
        slot: fm_types::display::DisplayOutputSlot,
        config: Option<Box<fm_types::display::DisplayOutputConfig>>,
    },
    /// Replaces the `LiveLAN` viewer stream's configuration (OR-017), its PIN
    /// already resolved to the exact one it will hold. A project edit,
    /// undone by restoring the previous configuration; the daemon realizes
    /// it live.
    SetLiveLan(Box<fm_types::live_lan::LiveLanConfig>),
    /// Sets one health alert rule's thresholds, hold, and switch for the
    /// show (RC-015); the rule's default removes the show's own setting. A
    /// project edit, undone by restoring the rule's previous setting; the
    /// daemon's alert engine follows it live.
    SetAlertRule(fm_types::alert_rules::AlertRuleSetting),
    /// One replay command (OR-012): a settings change is a project edit,
    /// everything else a live operation realized on its target frame
    /// (see [`crate::EngineReplayOperation`]).
    Replay(crate::EngineReplayOperation),
    /// One highlight reel command (OR-013): a reel edit is a project edit,
    /// export control a live operation (see [`crate::EngineReelOperation`]).
    Reel(crate::EngineReelOperation),
    /// Reconfigures network stream input `input` (IN-015). A project edit
    /// that honours the input's padlock, undone by restoring the previous
    /// configuration; the daemon reconnects the stream live.
    SetNetworkStream {
        input: InputId,
        config: Box<fm_types::network_stream::NetworkStreamConfig>,
    },
    /// Replaces capture input `input`'s settings (IN-001). A project edit
    /// that honours the input's padlock, undone by restoring the previous
    /// settings; the daemon reopens the device live.
    SetCaptureSettings {
        input: InputId,
        settings: Box<fm_types::capture::CaptureSettings>,
    },
    /// Creates or replaces (`Some`) or removes (`None`) cue list `list`'s
    /// definition (RC-017). A project edit honouring the project padlock,
    /// undone by restoring the previous definition; the run state carries
    /// across (see [`crate::cue`]).
    SetCueList {
        list: u64,
        definition: Option<crate::EngineCueListDefinition>,
        /// `Some(index)` places it at that index of the project's list
        /// (clamped to its end): an undone removal returns to its old
        /// place. `None` keeps an existing one where it is and appends a
        /// new one; a removal ignores it.
        position: Option<usize>,
    },
    /// Operates cue list `list` on the command's target frame (RC-017): a
    /// GO runs every action due on that frame together. A live operation.
    CueControl {
        list: u64,
        operation: crate::EngineCueOperation,
    },
    /// The cue clock: runs list `list`'s waiting actions on `frame`, which
    /// must equal [`Engine::cue_advance_frame`]. Accepted ahead of `frame`
    /// and realized on it; no other command is accepted until then. List
    /// `0` is the waiting script run (RC-011, see [`crate::script`]): the
    /// advance runs its next part.
    CueAdvance {
        list: u64,
        frame: u64,
    },
    /// The runtime action of an accepted cue GO or advance: the realized
    /// form of every action due on the frame, in order. Never accepted as a
    /// command.
    RealizeCueActions(Vec<EngineCommand>),
    /// Creates or replaces (`Some`) or removes (`None`) script `script`
    /// (RC-011). A project edit honouring the project padlock, undone by
    /// restoring the previous definition (see [`crate::script`]).
    SetScript {
        script: u64,
        definition: Option<crate::EngineScriptDefinition>,
        /// `Some(index)` places it at that index of the project's list
        /// (clamped to its end): an undone removal returns to its old
        /// place. `None` keeps an existing one where it is and appends a
        /// new one; a removal ignores it.
        position: Option<usize>,
    },
    /// Runs script `script` in the sandbox on the command's target frame
    /// (RC-011): every action it performs applies together, or the run is
    /// refused whole. A live operation, realized like a cue GO.
    RunScript {
        script: u64,
    },
    /// Ends waiting script run `run`, or clears that run's fault, on the
    /// command's target frame (RC-011, schema 56); with no `run`, ends every
    /// waiting run and clears every fault (schema 60). A live operation.
    StopScript {
        run: Option<u64>,
    },
    /// Creates or replaces (`Some`) or removes (`None`) user shortcut
    /// `shortcut` (RC-004). A project edit honouring the project padlock,
    /// undone by restoring the previous definition (see
    /// [`crate::shortcut`]).
    SetShortcut {
        shortcut: u64,
        definition: Option<Box<fm_model::Shortcut>>,
        /// `Some(index)` places it at that index of the project's list
        /// (clamped to its end): an undone removal returns to its old
        /// place. `None` keeps an existing one where it is and appends a
        /// new one; a removal ignores it.
        position: Option<usize>,
    },
    /// Fires user shortcut `shortcut` on the command's target frame
    /// (RC-004): resolved into the cue list operation or script run it
    /// stands for, or, for cue actions, planned and realized like a cue
    /// GO's first frame. A live operation. Its dynamic targets resolve
    /// under `targets`; a role left unbound there is read from the show
    /// (Preview and Program) when the fire is planned. An action taking
    /// its value from the firing control reads `value` (RC-004 dynamic
    /// values); a cue or script shortcut ignores it.
    FireShortcut {
        shortcut: u64,
        targets: fm_model::TargetBindings,
        value: Option<fm_model::FireValue>,
    },
}

/// What an accepted title command does on air.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleCueAction {
    /// Take the title onto its overlay channel and play its In phase.
    Take,
    /// Play the title's Out phase, then take its overlay channel off.
    OffAir,
}

/// The runtime action of an accepted title take or off-air: the overlay
/// channel it acts on and the animation phase lengths it plays.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TitleCue {
    pub input: InputId,
    pub channel: OverlayChannelId,
    pub action: TitleCueAction,
    pub timing: TitleAirTiming,
}

/// A title on air through [`EngineCommand::TakeTitle`] at one realized
/// frame: its overlay channel and where it is in its In/Hold/Out life.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineTitleAir {
    pub input: InputId,
    pub channel: OverlayChannelId,
    /// The title's current In or Out phase: where it started and how long
    /// it plays, so a renderer sampling on its own clock (the simulated
    /// monitor) can play the phase the engine realized at once.
    pub air: TitleAir,
    pub frame: TitleAirFrame,
}

/// The on-air animations one realized runtime action changed: what the
/// control service watches to report the action realized once the motion
/// it started has played (Protocol 2.86).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EngineActionMotion {
    /// Each title whose on-air animation the action started, replaced, or
    /// ended, ascending by input, with its animation after the action
    /// (`None` once the title is no longer animated on air).
    pub titles: Vec<(InputId, Option<TitleAir>)>,
    /// Each overlay channel whose realized picture the action changed (its
    /// source, on-air state, opacity, or timed transition, not its
    /// configuration), ascending.
    pub channels: Vec<OverlayChannelId>,
}

impl EngineActionMotion {
    /// Records the titles whose animation differs between `before` and
    /// `after`, keeping the latest animation of a title already recorded.
    fn record_channels(
        &mut self,
        before: &[OverlayChannelState; OVERLAY_CHANNEL_COUNT],
        after: &[OverlayChannelState; OVERLAY_CHANNEL_COUNT],
    ) {
        for ((channel, before), after) in OverlayChannelId::ALL.into_iter().zip(before).zip(after) {
            if !before.shows_like(after)
                && let Err(index) = self.channels.binary_search(&channel)
            {
                self.channels.insert(index, channel);
            }
        }
    }

    fn record_titles(
        &mut self,
        before: &BTreeMap<InputId, (OverlayChannelId, TitleAir)>,
        after: &BTreeMap<InputId, (OverlayChannelId, TitleAir)>,
    ) {
        for input in before.keys().chain(after.keys()) {
            let now = after.get(input);
            if before.get(input) == now {
                continue;
            }
            let air = now.map(|(_, air)| *air);
            match self.titles.binary_search_by_key(input, |(title, _)| *title) {
                Ok(index) => self.titles[index].1 = air,
                Err(index) => self.titles.insert(index, (*input, air)),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineAcceptance {
    pub target_frame: FrameNumber,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineEvent {
    DesiredSwitcherChanged(EngineCommand),
    InputRenamed {
        input: InputId,
        name: String,
    },
    InputOrderChanged {
        inputs: Vec<InputId>,
    },
    InputAdded {
        input: InputId,
        name: String,
        source: EngineInputSource,
    },
    InputRemoved {
        input: InputId,
    },
    StreamsChanged {
        streams: Vec<EngineStreamStatus>,
    },
    /// A stream destination was added, reconfigured, or removed (or the
    /// edit was undone or redone) (OR-006), with the resulting roster.
    StreamTargetChanged {
        target: StreamTargetId,
        config: Option<Box<fm_model::StreamTarget>>,
        streams: Vec<EngineStreamStatus>,
    },
    RecordingChanged {
        recorder: RecorderSlot,
        active: bool,
    },
    EditLockChanged {
        scope: EditLockScope,
        locked: bool,
    },
    /// A [`EngineCommand::SetTitleFields`] was accepted; `revision` is the
    /// title's field revision afterwards (unchanged for an empty edit).
    ///
    /// [`EngineCommand::TakeTitle`] and [`EngineCommand::TitleOffAir`] report
    /// [`EngineEvent::DesiredSwitcherChanged`] instead; the taken title's
    /// field revision is read from [`ShowState::title`].
    TitleFieldsChanged {
        input: InputId,
        revision: u64,
    },
    /// A playlist command was accepted for `frame`. `cue` is the entry it
    /// put on air, whose input is taken to Program on `frame`; the playlist
    /// itself is read from [`ShowState::playlist`].
    PlaylistChanged {
        frame: u64,
        cue: Option<PlaylistCue>,
    },
    /// A grade edit was accepted; the grading afterwards is read from
    /// [`ShowState::grading`].
    GradingChanged(EngineGradeEdit),
    /// A title animation edit (or its undo or redo) was accepted for
    /// `input`'s template; the animation afterwards is read from
    /// [`ShowState::title`].
    TitleAnimationChanged {
        input: InputId,
    },
    /// A title template field was shown or hidden in the browser title
    /// editor for `input`'s template; the exposure afterwards is read from
    /// [`ShowState::title`].
    TitleExposureChanged {
        input: InputId,
    },
    /// A scene animation edit (or its undo or redo) was accepted for
    /// `input`'s scene; the animation afterwards is read from
    /// [`ShowState::scene_input`].
    SceneAnimationChanged {
        input: InputId,
    },
    /// An animation preset library edit (or its undo or redo) was accepted;
    /// the library afterwards is read from [`ShowState::animation_presets`].
    AnimationPresetsChanged(crate::EngineAnimationPresetEdit),
    /// A favourite transition button was assigned (or the assignment undone
    /// or redone); the favourites are read from
    /// [`ShowState::transition_favorites`].
    TransitionFavoriteChanged {
        slot: u8,
    },
    /// A media command was accepted for `frame`; the input's settings
    /// afterwards are read from [`ShowState::media_settings`] and its
    /// projected transport from [`Engine::media_status`].
    MediaChanged {
        input: InputId,
        frame: u64,
        operation: crate::EngineMediaOperation,
    },
    /// A page navigation was accepted for `frame` (IN-009); the page it
    /// lands on is reported by that frame's [`FrameResult::slideshows`].
    PageNavigated {
        input: InputId,
        frame: u64,
        navigation: fm_types::paged::PagedNavigation,
    },
    /// A media list command was accepted for `frame` (IN-006); the list
    /// afterwards is read from [`ShowState::media_list`].
    MediaListChanged {
        input: InputId,
        frame: u64,
        operation: crate::EngineMediaListOperation,
    },
    /// A page step of a document input was accepted (IN-014); the page
    /// afterwards is read from [`ShowState::paged_page`].
    PagedChanged {
        input: InputId,
        navigation: fm_types::paged::PagedNavigation,
    },
    /// An input lifecycle edit (or its undo or redo) was accepted
    /// (IN-026). `added` lists the inputs a duplicate or import appended,
    /// with their names; `refused` the import items it refused, by request
    /// index. Lifecycle, sources, and categories afterwards are read from
    /// [`ShowState`].
    InputLifecycleChanged {
        edit: crate::EngineInputLifecycleEdit,
        added: Vec<(InputId, String)>,
        refused: Vec<(usize, fm_model::BatchImportRefusal)>,
    },
    /// A live scene layer edit (or its undo or redo) was accepted (SW-006);
    /// the scene afterwards is read from [`ShowState::scene`].
    SceneLayoutChanged(crate::EngineSceneLayerEdit),
    /// A mixer label, group, or group level edit (or its undo or redo) was
    /// accepted (AU-001); the mixer afterwards is read from [`ShowState`].
    AudioMixerChanged(crate::EngineAudioMixerEdit),
    /// Fade to Black now fades `output` or leaves it live (SW-004); the
    /// selection is read from the desired switcher.
    FadeToBlackOutputChanged {
        output: OutputId,
        affected: bool,
    },
    /// `output` now fades over its own duration, or over the requested one
    /// with `None` (SW-004).
    FadeToBlackOutputDurationChanged {
        output: OutputId,
        duration_frames: Option<u32>,
    },
    /// Automatic Fade-to-Black moves now ease along `curve` (SW-004).
    FadeToBlackCurveChanged {
        curve: FadeToBlackCurve,
    },
    /// A display output slot was configured or cleared (or the edit was
    /// undone or redone) (OR-011); the slot afterwards is read from
    /// [`ShowState::display_output`].
    DisplayOutputChanged {
        slot: fm_types::display::DisplayOutputSlot,
        config: Option<Box<fm_types::display::DisplayOutputConfig>>,
    },
    /// The `LiveLAN` configuration changed (or the edit was undone or redone)
    /// (OR-017); it is read from [`ShowState::live_lan`].
    LiveLanChanged(Box<fm_types::live_lan::LiveLanConfig>),
    /// The show's health alert rule settings after an edit (or its undo or
    /// redo) (RC-015).
    AlertRulesChanged(Vec<fm_types::alert_rules::AlertRuleSetting>),
    /// A replay command was accepted for frame `frame` (OR-012); settings
    /// are read from [`ShowState::replay`].
    ReplayChanged {
        frame: u64,
        operation: crate::EngineReplayOperation,
    },
    /// A highlight reel was created, replaced, or removed (or the edit was
    /// undone or redone) (OR-013); the reels afterwards are read from
    /// [`ShowState::reels`]. Export progress is reported by
    /// [`Engine::reel_status`], not an event.
    ReelChanged {
        operation: crate::EngineReelOperation,
    },
    /// A network stream input was reconfigured (or the edit was undone or
    /// redone) (IN-015).
    NetworkStreamChanged {
        input: InputId,
        config: Box<fm_types::network_stream::NetworkStreamConfig>,
    },
    /// A capture input's settings changed (or the edit was undone or
    /// redone) (IN-001).
    CaptureSettingsChanged {
        input: InputId,
        settings: Box<fm_types::capture::CaptureSettings>,
    },
    /// A cue list command was accepted for `frame` (RC-017): the list
    /// afterwards is read from [`ShowState::cue_list`] (`removed` when it no
    /// longer exists). `fired` lists the cues a GO or advance fired and
    /// `title_texts` every title text or number (schema 59) it set, in
    /// order. `applied` lists
    /// the runtime actions the frame's cue actions realize, in order (a
    /// record, stream, or media command appears as itself), so a runtime
    /// that realizes more than the show (recorders, stream workers) acts on
    /// exactly what the cue did. `scripts` lists every script step the
    /// frame ran, in order (at most [`fm_model::MAX_CUE_LIST_SCRIPT_STEPS`]).
    /// `edit` marks a
    /// definition edit (or its undo or redo) rather than a live operation.
    CueListChanged {
        list: u64,
        frame: u64,
        edit: bool,
        removed: bool,
        fired: Vec<u64>,
        title_texts: Vec<(InputId, FieldId, FieldValue)>,
        applied: Vec<EngineCommand>,
        scripts: Vec<CueScriptRun>,
    },
    /// A script was created, replaced, or removed (or the edit undone or
    /// redone) (RC-011); it is read from [`ShowState::script`].
    ScriptChanged {
        script: u64,
    },
    /// A script run was accepted for `frame` (RC-011): it performed
    /// `actions` actions in `instructions` instructions. `title_texts` and
    /// `applied` are as for [`Self::CueListChanged`].
    ScriptRan {
        script: u64,
        frame: u64,
        actions: u32,
        instructions: u32,
        title_texts: Vec<(InputId, FieldId, FieldValue)>,
        applied: Vec<EngineCommand>,
    },
    /// Script `script`'s run changed on `frame` without a part running
    /// (RC-011, schema 56): its run was stopped, its fault cleared, or a
    /// part due on `frame` failed (the script then holds the fault). The
    /// runs are read from [`ShowState::script_run`].
    ScriptRunChanged {
        script: u64,
        frame: u64,
    },
    /// Every script run was stopped and every fault cleared on `frame`
    /// (RC-011, schema 60): `scripts` (ascending) are the scripts that were
    /// waiting or faulted, all idle now.
    ScriptRunsStopped {
        scripts: Vec<u64>,
        frame: u64,
    },
    /// A user shortcut was created, replaced, or removed (or the edit
    /// undone or redone) (RC-004); it is read from [`ShowState::shortcut`].
    ShortcutChanged {
        shortcut: u64,
    },
    /// A shortcut's cue actions were accepted for `frame` (RC-004).
    /// `title_texts`, `applied`, and `scripts` are as for
    /// [`Self::CueListChanged`].
    ShortcutFired {
        shortcut: u64,
        frame: u64,
        actions: u32,
        title_texts: Vec<(InputId, FieldId, FieldValue)>,
        applied: Vec<EngineCommand>,
        scripts: Vec<CueScriptRun>,
    },
}

/// What one script step of a cue frame did (RC-011/RC-017): the counts a
/// [`EngineEvent::ScriptRan`] of the same script on the same show reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CueScriptRun {
    pub script: u64,
    /// Actions the run performed.
    pub actions: u32,
    /// Instructions the run used.
    pub instructions: u32,
}

/// A group or recovery operation on the stream destinations (OR-006).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineStreamControl {
    /// Every destination becomes desired running.
    StartAll,
    /// Every destination becomes desired stopped.
    StopAll,
    /// Destination `target`, which must be desired running, reconnects now
    /// with a fresh reconnect budget. Desired state is unchanged; the media
    /// plane realizes the retry.
    Retry { target: StreamTargetId },
}

/// Engine-level desired status of one stream target; realization is owned by
/// the media plane and always reads as `Stopped` at this layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineStreamStatus {
    pub target: StreamTargetId,
    pub name: String,
    pub desired_running: bool,
}

pub type EngineCommandOutcome = ApplyOutcome<EngineAcceptance, EngineEvent>;

/// The result of preparing an engine command without changing the live engine.
#[derive(Debug)]
pub enum EnginePrepareOutcome {
    /// The idempotency key was already durable, so there is nothing to commit.
    Replayed(EngineCommandOutcome),
    /// A new accepted or rejected receipt is staged and ready to commit.
    Prepared(Box<PreparedEngineExecution>),
}

impl EnginePrepareOutcome {
    #[must_use]
    pub fn prepared(self) -> Option<PreparedEngineExecution> {
        match self {
            Self::Prepared(prepared) => Some(*prepared),
            Self::Replayed(_) => None,
        }
    }

    #[must_use]
    pub fn replayed(self) -> Option<EngineCommandOutcome> {
        match self {
            Self::Prepared(_) => None,
            Self::Replayed(outcome) => Some(outcome),
        }
    }
}

/// An isolated post-command engine ready for projection or atomic installation.
#[derive(Debug)]
pub struct PreparedEngineExecution {
    staged: Engine,
    outcome: EngineCommandOutcome,
    base: EnginePreparationBase,
}

impl PreparedEngineExecution {
    #[must_use]
    pub const fn outcome(&self) -> &EngineCommandOutcome {
        &self.outcome
    }

    /// Returns the staged post-command engine without exposing mutable access.
    #[must_use]
    pub const fn staged_engine(&self) -> &Engine {
        &self.staged
    }

    /// Projects the staged engine by an exact number of deterministic ticks.
    ///
    /// The returned snapshot includes the projected runtime state and receipt.
    ///
    /// # Errors
    ///
    /// Returns a tick error if projection cannot advance, or
    /// [`SnapshotError::WorkInFlight`] if `ticks` does not reach an idle frame
    /// boundary.
    pub fn project(&self, ticks: u64) -> Result<EngineSnapshot, EngineError> {
        let mut projected = self.staged.clone();
        for _ in 0..ticks {
            projected.tick()?;
        }
        projected.snapshot().map_err(EngineError::from)
    }

    /// Consumes this preparation without changing its originating engine.
    pub fn abort(self) {}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EnginePreparationBase {
    state_epoch: StateEpoch,
    revision: Revision,
    event_sequence: EventSequence,
    receipts_recorded: u64,
    frame_cursor: FrameNumber,
    clock_time: ClockTime,
    runtime_generation: RuntimeGeneration,
}

/// Persisted engine coordinates needed to resume an idle runtime exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineRestoreState {
    pub state_epoch: StateEpoch,
    pub revision: Revision,
    pub event_sequence: EventSequence,
    pub runtime_generation: RuntimeGeneration,
    pub clock_time: ClockTime,
    pub frame_cursor: FrameNumber,
    pub receipts: Vec<(IdempotencyKey, CommandReceipt<EngineAcceptance>)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameResult {
    pub frame: FrameNumber,
    pub deadline: ClockTime,
    pub program: ProgramFrame,
    pub fade_to_black: FadeToBlackFrame,
    pub overlays: [OverlayChannelState; OVERLAY_CHANNEL_COUNT],
    pub events: Vec<SwitcherEvent>,
    pub input_audio_strip_updates: Vec<(InputId, EngineInputAudioStripState)>,
    /// Strip group membership and group level changes realized this frame,
    /// in acceptance order (AU-001).
    pub audio_mixer_updates: Vec<crate::EngineAudioMixerUpdate>,
    /// The outputs Fade to Black leaves live this frame, ascending (SW-004);
    /// see [`Self::fade_to_black_for`].
    pub fade_to_black_excluded_outputs: Vec<OutputId>,
    /// This frame's interval for every faded output with its own duration
    /// whose interval differs from [`Self::fade_to_black`], ascending
    /// (SW-004); see [`Self::fade_to_black_for`].
    pub fade_to_black_overrides: Vec<(OutputId, FadeToBlackFrame)>,
    /// Whether [`Self::program`] is a manual (T-bar) transition, whose
    /// position the operator moves rather than the clock (SW-004).
    pub manual_transition: bool,
    /// Every title taken with [`EngineCommand::TakeTitle`] that this frame
    /// shows on an overlay channel, ascending by input, with the pose to
    /// render it in. A title on air any other way (Program, a transition, a
    /// plain overlay take) is absent and shows its Hold pose.
    pub titles: Vec<EngineTitleAir>,
    /// Every Scene input on Program or Preview this frame, ascending by
    /// input, with the pose to render it in (GX-011 stage 5). A scene
    /// absent here (shown any other way, or restored) holds.
    pub scenes: Vec<crate::EngineSceneAir>,
    /// Every title input this frame shows on Program, an overlay channel,
    /// or Preview, ascending by input, with the frame its timeline started
    /// and its pose on Program or Preview (GX-001).
    pub titles_shown: Vec<crate::EngineTitleShown>,
    /// Every media input with an open source, a realized command, or a
    /// fault this frame, ascending by input (IN-003).
    pub media: Vec<crate::EngineMediaFrame>,
    /// Every slideshow input this frame, ascending by input (IN-009).
    pub slideshows: Vec<crate::EngineSlideshowFrame>,
    /// What every Replay input shows this frame, ascending by input
    /// (OR-012).
    pub replay: Vec<crate::EngineReplayFrame>,
    pub revision: Revision,
    pub runtime_generation: RuntimeGeneration,
}

impl FrameResult {
    /// This frame's Fade-to-Black interval as `output` shows it: the shared
    /// interval when Fade to Black affects the output, otherwise live.
    #[must_use]
    pub fn fade_to_black_for(&self, output: OutputId) -> FadeToBlackFrame {
        if self.fade_to_black_excluded_outputs.contains(&output) {
            FadeToBlackFrame::LIVE
        } else {
            self.fade_to_black_overrides
                .iter()
                .find(|(candidate, _)| *candidate == output)
                .map_or(self.fade_to_black, |(_, frame)| *frame)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineSnapshot {
    show: ShowState,
    realized_switcher: SwitcherState,
    state_epoch: StateEpoch,
    revision: Revision,
    event_sequence: EventSequence,
    runtime_generation: RuntimeGeneration,
    frame_rate: FrameRate,
    clock_domain: ClockDomainId,
    clock_time: ClockTime,
    frames_rendered: u64,
    receipts: Vec<(IdempotencyKey, CommandReceipt<EngineAcceptance>)>,
    title_timelines: BTreeMap<InputId, fm_model::TitleTimeline>,
}

impl EngineSnapshot {
    /// Where each shown title input's timeline started (GX-001): runtime
    /// state a project persists so a restored show continues it.
    #[must_use]
    pub const fn title_timelines(&self) -> &BTreeMap<InputId, fm_model::TitleTimeline> {
        &self.title_timelines
    }

    #[must_use]
    pub const fn show(&self) -> &ShowState {
        &self.show
    }

    #[must_use]
    pub const fn realized_switcher(&self) -> &SwitcherState {
        &self.realized_switcher
    }

    #[must_use]
    pub fn desired_manual_transition(&self) -> Option<EngineManualTransitionState> {
        self.show()
            .desired_switcher()
            .t_bar()
            .map(engine_manual_state)
    }

    #[must_use]
    pub const fn desired_fade_to_black(&self) -> EngineFadeToBlackState {
        engine_fade_to_black_state(self.show.desired_switcher())
    }

    #[must_use]
    pub const fn realized_fade_to_black(&self) -> EngineFadeToBlackState {
        engine_fade_to_black_state(&self.realized_switcher)
    }

    #[must_use]
    pub fn realized_manual_transition(&self) -> Option<EngineManualTransitionState> {
        self.realized_switcher.t_bar().map(engine_manual_state)
    }

    #[must_use]
    pub const fn state_epoch(&self) -> StateEpoch {
        self.state_epoch
    }

    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    #[must_use]
    pub const fn event_sequence(&self) -> EventSequence {
        self.event_sequence
    }

    #[must_use]
    pub const fn runtime_generation(&self) -> RuntimeGeneration {
        self.runtime_generation
    }

    #[must_use]
    pub const fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }

    #[must_use]
    pub const fn clock_domain(&self) -> ClockDomainId {
        self.clock_domain
    }

    #[must_use]
    pub const fn clock_time(&self) -> ClockTime {
        self.clock_time
    }

    #[must_use]
    pub const fn frames_rendered(&self) -> u64 {
        self.frames_rendered
    }

    #[must_use]
    pub fn receipts(&self) -> &[(IdempotencyKey, CommandReceipt<EngineAcceptance>)] {
        &self.receipts
    }
}

#[derive(Clone, Debug)]
pub struct Engine {
    commands: CommandState<ShowState, EngineAcceptance>,
    /// Monotonic count of receipts recorded, so preparations go stale after
    /// any committed receipt even once retention holds the count constant.
    receipts_recorded: u64,
    realized_switcher: SwitcherState,
    scheduler: RuntimeScheduler,
    clock: ManualClock,
    frame_rate: FrameRate,
    runtime_generation: RuntimeGeneration,
    pending_actions: usize,
    transition_in_flight: Option<TransitionKind>,
    /// Session-scoped undo/redo history; never snapshotted or restored.
    history: EditHistory,
    /// The future frame an accepted playlist advance is realized on. No
    /// other command is accepted before it.
    playlist_hold: Option<FrameNumber>,
    /// The future frame an accepted cue advance is realized on (RC-017). No
    /// other command is accepted before it.
    cue_hold: Option<FrameNumber>,
    /// Realized title animations: every title a realized
    /// [`EngineCommand::TakeTitle`] or [`EngineCommand::TitleOffAir`] put
    /// in motion, while its overlay channel still shows it. Runtime state:
    /// never snapshotted, so a restored title holds.
    title_air: BTreeMap<InputId, (OverlayChannelId, TitleAir)>,
    /// What each runtime action the last tick realized changed of the
    /// on-air animations, in realization order. Runtime state.
    action_motions: Vec<EngineActionMotion>,
    /// Whether a title clip can be carried to clients (see
    /// [`Self::set_title_clip_bound`]); `None` bounds nothing.
    title_clip_bound: Option<crate::TitleClipBound>,
    /// Realized scene animations: every Scene input on Program or Preview
    /// (see [`crate::EngineSceneAnimationEdit`]). Runtime state: never
    /// snapshotted, so a restored scene holds.
    scene_air: BTreeMap<InputId, crate::scene_animation::SceneAir>,
    /// Every title input on Program, an overlay channel, or Preview, with
    /// the frame its timeline started (GX-001). Snapshotted, so a restored
    /// title's tickers and clocks continue.
    title_shown: BTreeMap<InputId, crate::title_timeline::TitleShown>,
    /// Whether a scene clip can be carried to clients (see
    /// [`Self::set_scene_clip_bound`]); `None` bounds nothing.
    scene_clip_bound: Option<crate::SceneClipBound>,
    /// Media-file transports by input (IN-003). Runtime state: never
    /// snapshotted, so a restored source is cued at its start frame.
    media: BTreeMap<InputId, crate::media::MediaRuntime>,
    slideshows: BTreeMap<InputId, crate::slideshow::SlideshowRuntime>,
    /// Every paged input's page count as the media plane reported it
    /// (IN-014). Runtime only: never snapshotted.
    paged_counts: BTreeMap<InputId, u32>,
    /// The replay recording, events, and players (OR-012). Runtime state:
    /// never snapshotted, so a restored engine starts a new recording.
    replay: crate::replay::ReplayRuntime,
}

impl Engine {
    #[must_use]
    pub fn new(show: ShowState, frame_rate: FrameRate, clock_domain: ClockDomainId) -> Self {
        let realized_switcher = show.desired_switcher().clone();
        Self {
            commands: CommandState::new(show, StateEpoch::new(1)).with_receipt_limit(RECEIPT_LIMIT),
            receipts_recorded: 0,
            realized_switcher,
            scheduler: RuntimeScheduler::new(frame_rate, 0, PlanGeneration::new(0), Arc::new(())),
            clock: ManualClock::new(clock_domain, ClockTime::ZERO),
            frame_rate,
            runtime_generation: RuntimeGeneration::default(),
            pending_actions: 0,
            transition_in_flight: None,
            history: EditHistory::default(),
            playlist_hold: None,
            cue_hold: None,
            title_air: BTreeMap::new(),
            action_motions: Vec::new(),
            title_clip_bound: None,
            scene_air: BTreeMap::new(),
            title_shown: BTreeMap::new(),
            scene_clip_bound: None,
            media: BTreeMap::new(),
            slideshows: BTreeMap::new(),
            paged_counts: BTreeMap::new(),
            replay: crate::replay::ReplayRuntime::new(frame_rate),
        }
    }

    /// Opens (`Some`) or closes (`None`) media input `input`'s source, cued
    /// at its start frame and stopped, clearing any fault.
    ///
    /// # Errors
    ///
    /// Returns [`crate::EngineMediaError::NotMediaInput`] for an input that
    /// is not a media input of the show, and
    /// [`crate::EngineMediaError::Playback`] when the source's rates cannot
    /// drive a transport (the input is faulted as unsupported).
    pub fn set_media_source(
        &mut self,
        input: InputId,
        source: Option<crate::EngineMediaSource>,
    ) -> Result<(), crate::EngineMediaError> {
        let settings = *self
            .show()
            .media_settings(input)
            .ok_or(crate::EngineMediaError::NotMediaInput(input))?;
        let opened_for = source
            .is_some()
            .then(|| self.show().input_source(input).cloned())
            .flatten();
        let runtime = self.media.entry(input).or_default();
        runtime.opened_for = opened_for;
        if let Err(error) = runtime.open(source, &settings, self.frame_rate) {
            runtime.fault(crate::EngineMediaFault::Unsupported);
            return Err(crate::EngineMediaError::Playback(error));
        }
        Ok(())
    }

    /// Records paged input `input`'s page count as the media plane reads
    /// it (IN-014): `Some` once its document is known (a count of zero is
    /// treated as unknown), `None` while it is not. Navigation resolves
    /// against it. Returns whether `input` is a paged input of the show.
    pub fn set_paged_count(&mut self, input: InputId, count: Option<u32>) -> bool {
        if self.show().paged_page(input).is_none() {
            self.paged_counts.remove(&input);
            return false;
        }
        match count.filter(|count| *count > 0) {
            Some(count) => {
                self.paged_counts
                    .insert(input, count.min(fm_types::paged::MAX_PAGED_PAGES));
            }
            None => {
                self.paged_counts.remove(&input);
            }
        }
        true
    }

    /// Paged input `input`'s page and reported count, or `None` for an
    /// input that is not paged (IN-014).
    #[must_use]
    pub fn paged_status(&self, input: InputId) -> Option<crate::EnginePagedStatus> {
        let page = self.show().paged_page(input)?;
        Some(crate::EnginePagedStatus {
            input,
            page,
            count: self.paged_counts.get(&input).copied(),
        })
    }

    /// Every paged input's status, ascending by input (IN-014).
    #[must_use]
    pub fn paged_statuses(&self) -> Vec<crate::EnginePagedStatus> {
        self.show()
            .paged_inputs()
            .keys()
            .filter_map(|input| self.paged_status(*input))
            .collect()
    }

    /// Whether media input `input` plays from an open source (IN-026): a
    /// source is open, the input is not closed, and its source has not
    /// been replaced since the source opened. The media plane opens the
    /// source of any media input for which this is `false`, unless it is
    /// closed.
    #[must_use]
    pub fn media_source_open(&self, input: InputId) -> bool {
        let show = self.show();
        self.media.get(&input).is_some_and(|runtime| {
            runtime.is_open()
                && !show.input_lifecycle(input).closed
                && runtime.opened_for.as_ref() == show.input_source(input)
        })
    }

    /// Reports an external source's current frame count and completion. A
    /// shrinking source faults as truncated; a completed source that
    /// changes faults as changed.
    ///
    /// # Errors
    ///
    /// Returns [`crate::EngineMediaError::NotMediaInput`] for an input that
    /// is not a media input of the show.
    pub fn update_media_source(
        &mut self,
        input: InputId,
        frames: u64,
        complete: bool,
    ) -> Result<(), crate::EngineMediaError> {
        if self.show().media_settings(input).is_none() {
            return Err(crate::EngineMediaError::NotMediaInput(input));
        }
        self.media
            .entry(input)
            .or_default()
            .update_source(frames, complete);
        Ok(())
    }

    /// Closes media input `input`'s source with `fault`; the next frame
    /// reports it once.
    ///
    /// # Errors
    ///
    /// Returns [`crate::EngineMediaError::NotMediaInput`] for an input that
    /// is not a media input of the show.
    pub fn set_media_fault(
        &mut self,
        input: InputId,
        fault: crate::EngineMediaFault,
    ) -> Result<(), crate::EngineMediaError> {
        if self.show().media_settings(input).is_none() {
            return Err(crate::EngineMediaError::NotMediaInput(input));
        }
        self.media.entry(input).or_default().fault(fault);
        Ok(())
    }

    /// Media input `input`'s status once every accepted media command is
    /// realized, or `None` for an input that is not a media input.
    #[must_use]
    pub fn media_status(&self, input: InputId) -> Option<crate::EngineMediaStatus> {
        let settings = self.show().media_settings(input)?;
        Some(self.media.get(&input).map_or_else(
            || crate::media::MediaRuntime::default().status(input, settings),
            |runtime| runtime.status(input, settings),
        ))
    }

    /// Every media input's status (see [`Self::media_status`]), ascending
    /// by input.
    #[must_use]
    pub fn media_statuses(&self) -> Vec<crate::EngineMediaStatus> {
        self.show()
            .media_inputs()
            .keys()
            .filter_map(|input| self.media_status(*input))
            .collect()
    }

    /// Whether media list `input`'s transport plays (or waits for data at a
    /// growing file's live edge), or is about to as soon as its new file
    /// opens (IN-006).
    fn media_list_playing(&self, input: InputId) -> bool {
        let opening = self
            .media
            .get(&input)
            .is_some_and(|runtime| runtime.play_on_open && !self.media_source_open(input));
        opening
            || self.media_status(input).is_some_and(|status| {
                matches!(
                    status.state,
                    fm_playback::MediaState::Playing | fm_playback::MediaState::Waiting
                )
            })
    }

    /// Media list input `input`'s replicated state (IN-006): its list, the
    /// play order of the current cycle, and the active item's position in
    /// it; `None` for an input that is not a list.
    #[must_use]
    pub fn media_list_status(&self, input: InputId) -> Option<crate::EngineMediaListStatus> {
        let list = self.show().media_list(input)?;
        Some(crate::EngineMediaListStatus {
            input,
            list: list.clone(),
            order: list.play_order(),
            position: list.position().unwrap_or(0),
        })
    }

    /// Every media list's status, ascending by input.
    #[must_use]
    pub fn media_list_statuses(&self) -> Vec<crate::EngineMediaListStatus> {
        self.show()
            .media_lists()
            .keys()
            .filter_map(|input| self.media_list_status(*input))
            .collect()
    }

    /// Whether any media transport changes on the next frame absent new
    /// commands: one is playing or waiting, a synthetic source is still
    /// growing, or a command or fault is still to be realized.
    #[must_use]
    pub fn media_advancing(&self) -> bool {
        let show = self.show();
        self.media.iter().any(|(input, runtime)| {
            let lifecycle = show.input_lifecycle(*input);
            runtime.advancing(lifecycle.paused || lifecycle.closed)
        })
    }

    /// Whether any slideshow shows something new on the next frame absent
    /// new commands: one is advancing on its own, in a transition or a Ken
    /// Burns move, or has a navigation still to realize.
    #[must_use]
    pub fn slideshows_advancing(&self) -> bool {
        let show = self.show();
        // A slideshow not yet ticked starts on the next frame.
        let unstarted = show.input_sources().iter().any(|(input, source)| {
            matches!(source, fm_model::InputSourceSpec::Slideshow(_))
                && !self.slideshows.contains_key(input)
        });
        unstarted
            || self.slideshows.iter().any(|(input, runtime)| {
                let lifecycle = show.input_lifecycle(*input);
                runtime.advancing(lifecycle.paused || lifecycle.closed)
            })
    }

    /// Installs the pictures the server found in a folder slideshow's
    /// folder (IN-009), in any order: the engine sorts them by file name
    /// (natural order). Runtime state, never journaled: the slide on air
    /// stays until it leaves, new pictures join, and the listing
    /// generation increases when anything changed.
    ///
    /// # Errors
    ///
    /// Refuses an input that is not a folder slideshow.
    pub fn set_slideshow_listing(
        &mut self,
        input: InputId,
        listing: Vec<crate::EngineSlide>,
    ) -> Result<(), crate::EngineSlideshowError> {
        let show = self.commands.state();
        let config = crate::slideshow::paged_config(show, input)
            .map_err(|_| crate::EngineSlideshowError::NotSlideshowInput(input))?;
        if !matches!(
            config.source,
            fm_types::still::SlideshowSource::Folder { .. }
        ) {
            return Err(crate::EngineSlideshowError::NotFolderSource(input));
        }
        let config = config.clone();
        // The server may list a folder in any order; the engine shows it
        // in one, so a replay lands navigations on the same slides.
        let mut listing = listing;
        crate::slideshow::sort_folder_listing(&mut listing);
        let runtime = self.slideshows.entry(input).or_default();
        runtime.follow(&config);
        runtime.set_listing(listing);
        Ok(())
    }

    /// The pictures a slideshow input lists now, in listing order.
    #[must_use]
    pub fn slideshow_listing(&self, input: InputId) -> &[crate::EngineSlide] {
        self.slideshows
            .get(&input)
            .map_or(&[], crate::slideshow::SlideshowRuntime::listing)
    }

    /// What slideshow input `input` showed on the last realized frame: its
    /// incoming and outgoing slide views and the incoming mix in basis
    /// points. `None` before its first frame or with nothing listed.
    #[must_use]
    pub fn slideshow_shown(
        &self,
        input: InputId,
    ) -> Option<(
        Option<crate::EngineSlideView>,
        Option<crate::EngineSlideView>,
        u16,
    )> {
        self.slideshows.get(&input)?.shown()
    }

    /// A slideshow input's settings, page, label, and listing generation.
    #[must_use]
    pub fn slideshow_status(&self, input: InputId) -> Option<crate::EngineSlideshowStatus> {
        let config = crate::slideshow::paged_config(self.show(), input).ok()?;
        Some(match self.slideshows.get(&input) {
            Some(runtime) => runtime.status(input, config),
            None => crate::slideshow::SlideshowRuntime::default().status(input, config),
        })
    }

    /// Every slideshow input's status, ascending by input.
    #[must_use]
    pub fn slideshow_statuses(&self) -> Vec<crate::EngineSlideshowStatus> {
        self.show()
            .inputs()
            .iter()
            .filter_map(|input| self.slideshow_status(*input))
            .collect()
    }

    /// Refuses every title animation edit (undo and redo included) whose
    /// resulting clip `bound` rejects: a clip the protocol cannot carry
    /// would make every event and snapshot record naming it unencodable.
    pub fn set_title_clip_bound(&mut self, bound: crate::TitleClipBound) {
        self.title_clip_bound = Some(bound);
    }

    /// Refuses every scene animation edit (undo and redo included) whose
    /// resulting clip `bound` rejects, like [`Self::set_title_clip_bound`].
    pub fn set_scene_clip_bound(&mut self, bound: crate::SceneClipBound) {
        self.scene_clip_bound = Some(bound);
    }

    /// Every Scene input on Program or Preview as the next frame (the frame
    /// cursor) shows it absent new commands, ascending by input. A scene
    /// not listed holds.
    #[must_use]
    pub fn scenes_on_air(&self) -> Vec<crate::EngineSceneAir> {
        crate::scene_animation::scene_air_frames(&self.scene_air, self.frame_cursor().get())
    }

    /// The pose Scene input `input` shows on the next frame: its In or Out
    /// phase position while one plays on Program or Preview, otherwise
    /// [`fm_model::fm_animation::ClipState::Hold`].
    #[must_use]
    pub fn scene_pose(&self, input: InputId) -> fm_model::fm_animation::ClipState {
        self.scenes_on_air()
            .into_iter()
            .find(|scene| scene.input == input)
            .map_or(fm_model::fm_animation::ClipState::Hold, |scene| {
                scene.frame.sample
            })
    }

    /// Whether any Scene input is playing an In or Out phase on the next
    /// frame: the picture changes every frame until it settles.
    #[must_use]
    pub fn scenes_in_motion(&self) -> bool {
        crate::scene_animation::scenes_in_motion(&self.scene_air, self.frame_cursor().get())
    }

    /// Every title input shown on Program, an overlay channel, or Preview
    /// as the next frame (the frame cursor) shows it absent new commands,
    /// ascending by input, with the frame its timeline started (GX-001).
    #[must_use]
    pub fn titles_shown(&self) -> Vec<crate::EngineTitleShown> {
        crate::title_timeline::shown_frames(
            &self.title_shown,
            self.commands.state(),
            self.frame_cursor().get(),
            self.frame_rate,
        )
    }

    /// Where each shown title input's timeline started (GX-001).
    #[must_use]
    pub fn title_timelines(&self) -> BTreeMap<InputId, fm_model::TitleTimeline> {
        crate::title_timeline::saved(&self.title_shown)
    }

    /// Whether any title is playing an In or Out phase on Program or
    /// Preview on the next frame.
    #[must_use]
    pub fn titles_in_motion(&self) -> bool {
        self.titles_shown()
            .iter()
            .any(crate::EngineTitleShown::in_motion)
    }

    /// Resumes the title timelines a snapshot or project saved (GX-001), on
    /// a freshly restored engine.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::TitleTimelineOutOfReach`] when a timeline
    /// names an input that is not a title or starts at or after the frame
    /// cursor; nothing changes then.
    pub fn restore_title_timelines(
        &mut self,
        timelines: &BTreeMap<InputId, fm_model::TitleTimeline>,
    ) -> Result<(), SnapshotError> {
        let cursor = self.frame_cursor().get();
        if let Some((input, timeline)) = timelines.iter().find(|(input, timeline)| {
            self.show().title(**input).is_none() || timeline.since_frame >= cursor
        }) {
            return Err(SnapshotError::TitleTimelineOutOfReach {
                input: *input,
                since_frame: timeline.since_frame,
                frame_cursor: cursor,
            });
        }
        self.title_shown = crate::title_timeline::restored(timelines, &self.realized_switcher);
        Ok(())
    }

    /// The length of `phase` of Scene input `input`'s animation in frames of
    /// this engine, or zero when `input` is not a Scene input.
    #[must_use]
    pub fn scene_phase_frames(&self, input: InputId, phase: Phase) -> u64 {
        self.show().scene_input(input).map_or(0, |scene| {
            scene.timing().output_frames(phase, self.frame_rate)
        })
    }

    #[must_use]
    pub const fn show(&self) -> &ShowState {
        self.commands.state()
    }

    #[must_use]
    pub const fn realized_switcher(&self) -> &SwitcherState {
        &self.realized_switcher
    }

    #[must_use]
    pub fn desired_manual_transition(&self) -> Option<EngineManualTransitionState> {
        self.show()
            .desired_switcher()
            .t_bar()
            .map(engine_manual_state)
    }

    #[must_use]
    pub fn realized_manual_transition(&self) -> Option<EngineManualTransitionState> {
        self.realized_switcher.t_bar().map(engine_manual_state)
    }

    #[must_use]
    pub const fn desired_fade_to_black(&self) -> EngineFadeToBlackState {
        engine_fade_to_black_state(self.show().desired_switcher())
    }

    #[must_use]
    pub const fn realized_fade_to_black(&self) -> EngineFadeToBlackState {
        engine_fade_to_black_state(&self.realized_switcher)
    }

    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.commands.revision()
    }

    #[must_use]
    pub const fn state_epoch(&self) -> StateEpoch {
        self.commands.state_epoch()
    }

    #[must_use]
    pub const fn event_sequence(&self) -> EventSequence {
        self.commands.event_sequence()
    }

    #[must_use]
    pub const fn runtime_generation(&self) -> RuntimeGeneration {
        self.runtime_generation
    }

    /// The engine's video frame rate; frame numbers count these frames.
    #[must_use]
    pub const fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }

    #[must_use]
    pub const fn frame_cursor(&self) -> FrameNumber {
        self.scheduler.pacer().next_frame()
    }

    #[must_use]
    pub fn clock_time(&self) -> ClockTime {
        self.clock.now()
    }

    /// Returns the next deterministic frame deadline without advancing state.
    ///
    /// # Errors
    ///
    /// Returns a pacing error when the next deadline exceeds the media clock.
    pub fn next_frame_deadline(&self) -> Result<ClockTime, EngineError> {
        self.scheduler
            .pacer()
            .next_deadline()
            .map(|deadline| ClockTime::from_nanos(deadline.at_ns))
            .map_err(|error| EngineError::Tick(fm_scheduler::TickError::Pacing(error)))
    }

    /// Returns the durable receipt recorded for an idempotency key, if any.
    ///
    /// This is a read-only lookup: it neither validates nor stages a command.
    #[must_use]
    pub fn receipt(&self, key: &IdempotencyKey) -> Option<&CommandReceipt<EngineAcceptance>> {
        self.commands.receipt(key)
    }

    /// Returns undoable project edits, oldest first; the last is undone next.
    ///
    /// At most [`crate::MAX_EDIT_HISTORY`] entries are retained.
    #[must_use]
    pub fn undo_entries(&self) -> impl ExactSizeIterator<Item = &EditHistoryEntry> {
        self.history.undo_entries()
    }

    /// Returns undone project edits, oldest first; the last is redone next.
    #[must_use]
    pub fn redo_entries(&self) -> impl ExactSizeIterator<Item = &EditHistoryEntry> {
        self.history.redo_entries()
    }

    /// Returns the only frame a [`EngineCommand::PlaylistAdvance`] is
    /// accepted for: the playlist's next boundary, or the next frame when
    /// that boundary has already passed. `None` while no boundary is pending.
    #[must_use]
    pub fn playlist_advance_frame(&self) -> Option<u64> {
        playlist_due_frame(self.show().playlist())
            .map(|due| due.max(self.scheduler.pacer().next_frame().get()))
    }

    /// Every title [`EngineCommand::TakeTitle`] put on an overlay channel,
    /// as the next frame (the frame cursor) shows it absent new commands,
    /// ascending by input.
    #[must_use]
    pub fn titles_on_air(&self) -> Vec<EngineTitleAir> {
        title_air_frames(&self.title_air, self.frame_cursor().get())
    }

    /// The length of `phase` of title `input`'s animation in frames of this
    /// engine, or zero when `input` is not a title.
    #[must_use]
    pub fn title_phase_frames(&self, input: InputId, phase: Phase) -> u64 {
        self.show().title(input).map_or(0, |title| {
            title.timing.output_frames(phase, self.frame_rate)
        })
    }

    /// Returns the only `(list, frame)` a [`EngineCommand::CueAdvance`] is
    /// accepted for (RC-017): the earliest waiting cue's due frame, or the
    /// next frame once that has passed. `None` while no cue waits. The
    /// waiting script runs (RC-011) are list `0`, due on the earliest
    /// run's resume frame (see [`fm_model::ScriptRunState::next_due`]); a
    /// cue list due on the same frame goes first.
    #[must_use]
    pub fn cue_advance_frame(&self) -> Option<(u64, u64)> {
        let cue = crate::cue::cue_due(self.show().cue_lists());
        let script = self
            .show()
            .script_run()
            .next_due()
            .map(|run| (0, run.due_frame));
        let earliest = match (cue, script) {
            (Some(cue), Some(script)) if script.1 < cue.1 => Some(script),
            (cue, script) => cue.or(script),
        };
        let next = self.scheduler.pacer().next_frame().get();
        // A replay auto return is due at once (OR-012).
        let replay = self
            .replay
            .return_due(self.show())
            .map(|_| (crate::replay::REPLAY_RETURN_LIST, next));
        let earliest = match (earliest, replay) {
            (Some(cue), Some(replay)) if cue.1 <= next => Some(cue).or(Some(replay)),
            (Some(_), Some(replay)) => Some(replay),
            (cue, replay) => cue.or(replay),
        };
        earliest.map(|(list, due)| (list, due.max(next)))
    }

    /// The replay auto return due now (OR-012), if any: the finished Replay
    /// input on Program, the source to go back to, and the mix's frames.
    pub(crate) fn replay_return_due(&self) -> Option<(InputId, InputId, u32)> {
        self.replay.return_due(self.show())
    }

    /// The command a fire of user shortcut `shortcut` stands for on the
    /// current show (RC-004): the cue list operation or script run it
    /// resolves to, or [`EngineCommand::FireShortcut`] itself when it runs
    /// cue actions (read them from [`ShowState::shortcut`]).
    ///
    /// # Errors
    ///
    /// Refuses an unknown or disabled shortcut, as a fire would.
    pub fn shortcut_command(&self, shortcut: u64) -> Result<EngineCommand, Rejection> {
        crate::shortcut::resolve(self.show(), shortcut, TargetBindings::default(), None)
    }

    /// Whether script `script`'s source holds a `wait`, so a run of it may
    /// span frames (RC-011, schema 56); `None` for an unknown script.
    #[must_use]
    pub fn script_waits(&self, script: u64) -> Option<bool> {
        crate::script::waits(self.show(), script)
    }

    /// Every show action running script `script` now would ask for, in
    /// order (RC-011): the script runs on a copy of the show exactly as an
    /// [`EngineCommand::RunScript`] submitted now plans it, so the list
    /// covers the branches it takes on the current show. Nothing changes.
    /// A run that would stop early lists the actions it asked for up to and
    /// including the refused one; an unknown script or a syntax error lists
    /// none.
    #[must_use]
    pub fn script_run_actions(&self, script: u64) -> Vec<fm_model::CueAction> {
        crate::script::requested_actions(self, script, self.scheduler.pacer().next_frame())
    }

    /// Every action verb script `script` holds, in any branch or loop
    /// (RC-011): whatever show a run of it meets, it can only ask for
    /// actions of these verbs. This bounds what a cue's
    /// [`fm_model::CueAction::RunScript`] step can do on a later frame,
    /// where the show (and so the branches taken) may differ from now.
    /// Empty for a source that does not parse; `None` for an unknown script.
    #[must_use]
    pub fn script_verbs(&self, script: u64) -> Option<Vec<fm_script::Verb>> {
        crate::script::verbs(self.show(), script)
    }

    /// Steps a cue operation or advance on the current show for
    /// `target_frame`: the list's next run state and, for a GO or advance,
    /// every due action applied in order to a copy of the show.
    fn cue_firing(
        &self,
        command: &EngineCommand,
        frame: u64,
    ) -> Result<(u64, fm_automation::CueFiring<fm_model::CueAction>), Rejection> {
        let (list_id, operation) = match command {
            EngineCommand::CueAdvance { list, frame: at } => {
                if self.cue_advance_frame() != Some((*list, *at)) {
                    let message = self.cue_advance_frame().map_or_else(
                        || format!("no cue waits for frame {at}"),
                        |(due_list, due)| {
                            format!(
                                "cue list {due_list} advances at frame {due}, not cue list \
                                 {list} at frame {at}"
                            )
                        },
                    );
                    return Err(Rejection::new(RejectionCode::Conflict, message));
                }
                (*list, None)
            }
            EngineCommand::CueControl { list, operation } => (*list, Some(*operation)),
            _ => unreachable!("only cue runtime commands are planned"),
        };
        let list = self
            .show()
            .cue_list(list_id)
            .ok_or_else(|| crate::cue::unknown_list(list_id))?;
        let stack = crate::cue::stack(list, self.frame_rate);
        let state = crate::cue::stack_state(&list.state);
        let refused = |error| crate::cue::stack_rejection(list_id, error);
        let quiet = |state| fm_automation::CueFiring {
            actions: Vec::new(),
            fired: Vec::new(),
            state,
        };
        let firing = match operation {
            None => stack.advance(&state, frame).map_err(refused)?,
            Some(crate::EngineCueOperation::Go { cue, .. }) => {
                stack.go(&state, cue, frame).map_err(refused)?
            }
            Some(crate::EngineCueOperation::Standby { cue }) => {
                quiet(stack.standby(&state, cue).map_err(refused)?)
            }
            Some(crate::EngineCueOperation::Back) => quiet(stack.back(&state)),
            Some(crate::EngineCueOperation::Skip) => quiet(stack.skip(&state)),
            Some(crate::EngineCueOperation::Stop) => quiet(stack.stop(&state)),
            Some(crate::EngineCueOperation::Arm { cue, in_millis, .. }) => {
                if in_millis == 0 || in_millis > fm_model::MAX_CUE_ARM_MS {
                    return Err(crate::cue::arm_delay_out_of_range());
                }
                let due = frame
                    .checked_add(u64::from(fm_model::delay_frames(
                        in_millis,
                        self.frame_rate,
                    )))
                    .ok_or_else(|| refused(fm_automation::CueStackError::FrameOverflow))?;
                quiet(stack.arm(&state, cue, due).map_err(refused)?)
            }
            Some(crate::EngineCueOperation::Disarm) => quiet(stack.disarm(&state)),
            Some(crate::EngineCueOperation::Reset) => quiet(stack.reset()),
        };
        Ok((list_id, firing))
    }

    /// Plans a cue operation or advance (see [`crate::cue`]).
    fn cue_plan(
        &self,
        command: &EngineCommand,
        target_frame: FrameNumber,
    ) -> Result<CuePlan, Rejection> {
        let frame = target_frame.get();
        let (list_id, firing) = self.cue_firing(command, frame)?;
        let targets = self.cue_targets(command, list_id);
        self.check_cue_selection(command, list_id, &firing, targets)?;
        let mut show = self.show().clone();
        let mut runtime = Vec::new();
        let mut transition = None;
        let mut title_texts = Vec::new();
        let mut scripts = Vec::new();
        let applied = self.apply_cue_actions(
            &mut show,
            &firing.actions,
            target_frame,
            &mut CueFrame {
                runtime: &mut runtime,
                transition: &mut transition,
                title_texts: &mut title_texts,
                scripts: Some(&mut scripts),
                takes: 0,
                targets: targets.frame,
            },
        );
        let mut fault = None;
        let (state, fired) = match applied {
            Ok(()) => (firing.state, firing.fired),
            Err((cue, step, rejection)) if !matches!(command, EngineCommand::CueAdvance { .. }) => {
                let message = format!("cue {cue} action {}: {}", step + 1, rejection.message);
                return Err(Rejection {
                    message,
                    ..rejection
                });
            }
            Err((cue, step, rejection)) => {
                // A refused deferred action stops its cue: the clock must not
                // retry it, so the advance is accepted with the fault set.
                show = self.show().clone();
                runtime.clear();
                transition = None;
                title_texts.clear();
                scripts.clear();
                let list = self
                    .show()
                    .cue_list(list_id)
                    .expect("the planned list exists");
                let state = crate::cue::stack_state(&list.state);
                // What the advance ran: the armed GO or the waiting cue.
                let due = fm_automation::CueStack::<fm_model::CueAction>::due(&state)
                    .map_or(fm_automation::CueDue::Wait, |(_, due)| due);
                fault = Some(crate::cue::fault(cue, step, &rejection));
                (
                    crate::cue::stack(list, self.frame_rate).fail(&state, due, cue, step),
                    Vec::new(),
                )
            }
        };
        let index = show
            .cue_lists()
            .iter()
            .position(|candidate| candidate.id == list_id)
            .expect("the planned list exists");
        let previous = &self.show().cue_lists()[index].state;
        let mut state = crate::cue::run_state(state, previous);
        if fault.is_some() {
            state.fault = fault;
        }
        state.targets = targets.run;
        if let Some(armed) = &mut state.armed {
            armed.targets = targets.armed;
        }
        show.cue_lists_mut()[index].state = state;
        Ok(CuePlan {
            show: Box::new(show),
            event: EngineEvent::CueListChanged {
                list: list_id,
                frame,
                edit: false,
                removed: false,
                fired,
                title_texts,
                applied: runtime.clone(),
                scripts,
            },
            runtime,
            transition,
        })
    }

    /// Binds the roles `targets` leaves open to the show's Preview and
    /// Program (RC-004): a fire's dynamic targets resolve against the
    /// show the fire is planned on, unless the command (a journal replay)
    /// carries them.
    pub(crate) fn bind_targets(&self, targets: TargetBindings) -> TargetBindings {
        let switcher = self.show().desired_switcher();
        TargetBindings {
            preview: targets.preview.or(Some(switcher.preview())),
            program: targets.program.or(Some(switcher.program())),
            selected: targets.selected,
        }
    }

    /// The targets a cue command's actions resolve under on this frame,
    /// and the targets the list's run and armed GO keep after it (RC-004,
    /// schema 73): a GO binds its run's targets, an arm binds its armed
    /// GO's, and an advance runs under the targets of the run it resumes
    /// or of the armed GO it fires. A list naming no dynamic target keeps
    /// none.
    /// Refuses a GO or arm whose cue run needs a selection the fire lacks
    /// (RC-004, see [`crate::cue::check_selection`]).
    fn check_cue_selection(
        &self,
        command: &EngineCommand,
        list_id: u64,
        firing: &fm_automation::CueFiring<fm_model::CueAction>,
        targets: CueTargets,
    ) -> Result<(), Rejection> {
        let start = match command {
            EngineCommand::CueControl {
                operation: crate::EngineCueOperation::Go { .. },
                ..
            } => firing.fired.first().map(|cue| (*cue, targets.frame)),
            EngineCommand::CueControl {
                operation: crate::EngineCueOperation::Arm { .. },
                ..
            } => firing
                .state
                .armed
                .map(|armed| (armed.cue, targets.armed.unwrap_or_default())),
            _ => None,
        };
        match (start, self.show().cue_list(list_id)) {
            (Some((cue, bound)), Some(list)) => crate::cue::check_selection(list, cue, &bound),
            _ => Ok(()),
        }
    }

    fn cue_targets(&self, command: &EngineCommand, list_id: u64) -> CueTargets {
        let Some(list) = self.show().cue_list(list_id) else {
            return CueTargets::default();
        };
        if !list
            .cues
            .iter()
            .flat_map(|cue| &cue.actions)
            .any(fm_model::CueAction::is_dynamic)
        {
            return CueTargets::default();
        }
        let run = list.state.targets;
        let armed = list.state.armed.and_then(|armed| armed.targets);
        match command {
            EngineCommand::CueControl {
                operation: crate::EngineCueOperation::Go { targets, .. },
                ..
            } => {
                let bound = self.bind_targets(*targets);
                CueTargets {
                    frame: bound,
                    run: Some(bound),
                    armed,
                }
            }
            EngineCommand::CueControl {
                operation: crate::EngineCueOperation::Arm { targets, .. },
                ..
            } => CueTargets {
                frame: TargetBindings::default(),
                run,
                armed: Some(self.bind_targets(*targets)),
            },
            EngineCommand::CueControl {
                operation: crate::EngineCueOperation::Reset,
                ..
            } => CueTargets::default(),
            EngineCommand::CueAdvance { .. } => {
                let due = fm_automation::CueStack::<fm_model::CueAction>::due(
                    &crate::cue::stack_state(&list.state),
                )
                .map(|(_, due)| due);
                // A run (or armed GO) started while the list named no
                // dynamic target bound none; an edit since may have added
                // one, which resolves Preview and Program as the show has
                // them on this frame and keeps them for the rest of the
                // run. Only a selection stays unbound.
                let bind = |bound: Option<TargetBindings>| {
                    bound.unwrap_or_else(|| self.bind_targets(TargetBindings::default()))
                };
                if due == Some(fm_automation::CueDue::Armed) {
                    let bound = bind(armed);
                    CueTargets {
                        frame: bound,
                        run: Some(bound),
                        armed: None,
                    }
                } else {
                    let bound = bind(run);
                    CueTargets {
                        frame: bound,
                        run: Some(bound),
                        armed,
                    }
                }
            }
            _ => CueTargets {
                frame: TargetBindings::default(),
                run,
                armed,
            },
        }
    }

    /// Runs a cue's script step on `show` (schema 55), collecting what its
    /// actions realize and what it did into `frame`; its own actions come
    /// back through [`Self::apply_cue_actions`], never another script.
    fn run_script_step(
        &self,
        script: u64,
        target_frame: FrameNumber,
        show: &mut ShowState,
        frame: &mut CueFrame<'_>,
    ) -> Result<(), Rejection> {
        let mut nested = CueFrame {
            runtime: &mut *frame.runtime,
            transition: &mut *frame.transition,
            title_texts: &mut *frame.title_texts,
            scripts: None,
            takes: frame.takes,
            targets: frame.targets,
        };
        let outcome = crate::script::run_on(
            self,
            script,
            crate::script::Entry::Step,
            target_frame,
            show,
            &mut nested,
        )?;
        frame.takes = nested.takes;
        let ran = CueScriptRun {
            script,
            actions: outcome.report.actions,
            instructions: outcome.report.instructions,
        };
        if let Some(rejection) = outcome.failure(script) {
            return Err(rejection);
        }
        if let Some(scripts) = frame.scripts.as_deref_mut() {
            record_script_step(scripts, ran)?;
        }
        Ok(())
    }

    /// Applies fired cue actions in order to `show`, collecting their runtime
    /// actions, the Program transition they start, and the title texts they
    /// set into `frame`. Returns the first refused action as
    /// `(cue, step, refusal)`.
    #[allow(clippy::result_large_err)] // a refusal carries the failing cue and action
    #[allow(clippy::too_many_lines)] // one pass over the fired actions
    pub(crate) fn apply_cue_actions(
        &self,
        show: &mut ShowState,
        actions: &[fm_automation::FiredCueAction<fm_model::CueAction>],
        target_frame: FrameNumber,
        frame: &mut CueFrame<'_>,
    ) -> Result<(), (u64, usize, Rejection)> {
        let CueFrame {
            runtime,
            transition,
            title_texts,
            scripts,
            takes,
            targets,
        } = frame;
        for fired in actions {
            let refuse = |rejection: Rejection| (fired.cue, fired.step, rejection);
            // A script step runs the script on this frame's show (schema 55);
            // its own actions come back through here, never another script.
            if let fm_model::CueAction::RunScript { script } = &fired.action {
                let mut step = CueFrame {
                    runtime,
                    transition,
                    title_texts,
                    scripts: scripts.as_deref_mut(),
                    takes: *takes,
                    targets: *targets,
                };
                self.run_script_step(*script, target_frame, show, &mut step)
                    .map_err(refuse)?;
                *takes = step.takes;
                continue;
            }
            // A dynamic target resolves under the fire's targets (RC-004)
            // and is then checked like a fixed one at definition time.
            let action =
                crate::cue::resolve_action(show, &fired.action, targets).map_err(refuse)?;
            let realized = &self.realized_switcher;
            let commands = crate::cue::expand(show, &action, |input| {
                realized.on_air_usage(input).is_some()
            })
            .map_err(refuse)?;
            record_title_values(title_texts, &action, &commands).map_err(refuse)?;
            for sub in commands {
                if let Some(kind) = self.transition_in_flight.or(**transition)
                    && refused_during_transition(&sub)
                    && !matches!(&sub, EngineCommand::SetInputAudioStrip { .. })
                {
                    let name = transition_kind_name(kind);
                    return Err(refuse(Rejection::new(
                        RejectionCode::Conflict,
                        format!("a {name} transition is already in flight"),
                    )));
                }
                if crate::cue::takes_program(&sub) {
                    *takes += 1;
                    if *takes > 1 {
                        return Err(refuse(Rejection::new(
                            RejectionCode::Conflict,
                            "the actions take Program more than once on one frame",
                        )));
                    }
                }
                let playlist = is_playlist_command(&sub)
                    .then(|| playlist_step(show, &sub, target_frame.get()));
                let action = match &playlist {
                    Some(Ok(step)) => EngineCommand::RealizePlaylistCue(step.cue),
                    _ => runtime_action(show, &sub),
                };
                let kind = runtime_transition_kind(show, &sub).or(match &playlist {
                    Some(Ok(PlaylistStep { cue: Some(cue), .. })) => {
                        cue_transition_kind(show.desired_switcher(), cue)
                    }
                    _ => None,
                });
                let realized_on_air = match &sub {
                    EngineCommand::TakeTitle { input, channel, .. }
                        if self.title_leaving_only(*input, *channel) =>
                    {
                        None
                    }
                    EngineCommand::SetTitleFields { input, .. }
                    | EngineCommand::TakeTitle { input, .. }
                    | EngineCommand::TitleOffAir { input } => {
                        self.realized_switcher.on_air_usage(*input)
                    }
                    _ => None,
                };
                let paged_count = match &sub {
                    EngineCommand::PageNavigate { input, .. } => {
                        self.paged_counts.get(input).copied()
                    }
                    _ => None,
                };
                let mutation = EngineMutation {
                    command: sub,
                    target_frame,
                    realized_on_air,
                    playlist,
                    title_clip_bound: None,
                    scene_clip_bound: None,
                    media_tracks: None,
                    media_list: None,
                    paged_count,
                    cue: None,
                    reel: None,
                };
                mutation.apply(show, &mut Vec::new()).map_err(refuse)?;
                **transition = transition.or(kind);
                runtime.push(action);
            }
        }
        Ok(())
    }

    fn playlist_step(
        &self,
        command: &EngineCommand,
        target_frame: FrameNumber,
    ) -> Result<PlaylistStep, Rejection> {
        if let EngineCommand::PlaylistAdvance { frame } = command {
            let expected = self.playlist_advance_frame();
            if expected != Some(*frame) {
                let message = expected.map_or_else(
                    || format!("no playlist boundary is pending at frame {frame}"),
                    |expected| {
                        format!("the playlist advances at frame {expected}, not at frame {frame}")
                    },
                );
                return Err(Rejection::new(RejectionCode::Conflict, message));
            }
        }
        playlist_step(self.show(), command, target_frame.get())
    }

    /// Validates one resolved reel command against the show and the
    /// recorded media before the command is accepted (OR-013).
    fn reel_mutation(
        &self,
        operation: &crate::EngineReelOperation,
    ) -> Result<crate::reel::ReelMutation, Rejection> {
        use crate::EngineReelOperation;
        match operation {
            EngineReelOperation::Set(_) | EngineReelOperation::Delete { .. } => {
                Ok(crate::reel::ReelMutation::Edit)
            }
            EngineReelOperation::Export {
                job,
                reel,
                destination,
            } => self
                .replay
                .plan_export(self.commands.state(), *job, *reel, destination)
                .map(crate::reel::ReelMutation::Export),
            EngineReelOperation::CancelExport { job } => self
                .replay
                .check_cancel_export(*job)
                .map(|()| crate::reel::ReelMutation::Cancel { job: *job })
                .map_err(|error| crate::reel::control_rejection(&error)),
            EngineReelOperation::ForgetExport { job } => self
                .replay
                .check_forget_export(*job)
                .map(|()| crate::reel::ReelMutation::Forget { job: *job })
                .map_err(|error| crate::reel::control_rejection(&error)),
        }
    }

    /// Returns how many idempotency receipts are retained, at most
    /// [`MAX_RETAINED_RECEIPTS`].
    #[must_use]
    pub fn receipt_count(&self) -> usize {
        self.commands.receipt_count()
    }

    /// Prepares a durable command and its runtime action on an isolated clone.
    ///
    /// Duplicate idempotency keys return their durable receipt without creating
    /// a staged execution.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Schedule`] if the staged runtime action sequence
    /// is exhausted.
    pub fn prepare_execute(
        &self,
        envelope: CommandEnvelope<EngineCommand>,
        now_millis: u64,
    ) -> Result<EnginePrepareOutcome, EngineError> {
        if let Some(receipt) = self.commands.receipt(&envelope.idempotency_key) {
            return Ok(EnginePrepareOutcome::Replayed(EngineCommandOutcome {
                receipt: receipt.clone(),
                events: Vec::new(),
                replayed: true,
            }));
        }

        let base = self.preparation_base();
        let mut staged = self.clone();
        let outcome = staged.execute_immediate(envelope, now_millis)?;
        Ok(EnginePrepareOutcome::Prepared(Box::new(
            PreparedEngineExecution {
                staged,
                outcome,
                base,
            },
        )))
    }

    /// Atomically installs a prepared engine if its live base is still current.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::StalePreparation`] if durable authority, receipt
    /// history, or the runtime cursor changed after preparation.
    pub fn commit_execute(
        &mut self,
        prepared: PreparedEngineExecution,
    ) -> Result<EngineCommandOutcome, EngineError> {
        if self.preparation_base() != prepared.base {
            return Err(EngineError::StalePreparation);
        }

        let PreparedEngineExecution {
            staged, outcome, ..
        } = prepared;
        *self = staged;
        Ok(outcome)
    }

    /// Accepts a durable command and schedules its runtime action.
    ///
    /// Duplicate idempotency keys replay the original receipt without adding
    /// another runtime action.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Schedule`] if the runtime action sequence is
    /// exhausted.
    pub fn execute(
        &mut self,
        envelope: CommandEnvelope<EngineCommand>,
        now_millis: u64,
    ) -> Result<EngineCommandOutcome, EngineError> {
        match self.prepare_execute(envelope, now_millis)? {
            EnginePrepareOutcome::Replayed(outcome) => Ok(outcome),
            EnginePrepareOutcome::Prepared(prepared) => self.commit_execute(*prepared),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn execute_immediate(
        &mut self,
        envelope: CommandEnvelope<EngineCommand>,
        now_millis: u64,
    ) -> Result<EngineCommandOutcome, EngineError> {
        let key = envelope.idempotency_key.clone();
        let command_id = envelope.id.clone();
        let next_frame = self.scheduler.pacer().next_frame();
        // A playlist advance is accepted ahead of its boundary and realized
        // on it; every other command targets the next frame.
        let target_frame = match &envelope.command {
            EngineCommand::PlaylistAdvance { frame } | EngineCommand::CueAdvance { frame, .. }
                if *frame > next_frame.get() =>
            {
                FrameNumber::new(*frame)
            }
            _ => next_frame,
        };
        let held_until = self.playlist_hold.filter(|frame| *frame >= next_frame);
        let cue_held_until = self.cue_hold.filter(|frame| *frame >= next_frame);
        let already_seen = self.commands.receipt(&key).is_some();
        // Undo and redo resolve to the stored project edit before scheduling,
        // so the runtime only ever realizes ordinary edit commands.
        let resolution = self
            .history
            .resolve(self.commands.state(), &envelope.command);
        // A playlist command is stepped once, here, against the pre-command
        // show: the mutation installs the result and the runtime realizes
        // its cue.
        let playlist = match resolution.as_ref().map(|resolved| &resolved.command) {
            Ok(command) if is_playlist_command(command) => {
                Some(self.playlist_step(command, target_frame))
            }
            _ => None,
        };
        let playlist_transition = match &playlist {
            Some(Ok(PlaylistStep { cue: Some(cue), .. })) => {
                cue_transition_kind(self.commands.state().desired_switcher(), cue)
            }
            _ => None,
        };
        // A cue GO or advance is planned once, here, like a playlist step:
        // every due action is applied to a copy of the show, which the
        // mutation installs, and realized by one runtime action.
        let cue = match resolution.as_ref().map(|resolved| &resolved.command) {
            // The cue clock's list 0 resumes the waiting script run.
            Ok(EngineCommand::CueAdvance { list: 0, frame }) => {
                Some(crate::script::plan_resume(self, *frame, target_frame))
            }
            // Its reserved list takes Program back from a finished Replay
            // input (OR-012 auto return).
            Ok(EngineCommand::CueAdvance {
                list: crate::replay::REPLAY_RETURN_LIST,
                frame,
            }) => Some(crate::replay::plan_return(self, *frame, target_frame)),
            Ok(command @ (EngineCommand::CueControl { .. } | EngineCommand::CueAdvance { .. })) => {
                Some(self.cue_plan(command, target_frame))
            }
            // A script run is planned the same way (RC-011).
            Ok(EngineCommand::RunScript { script }) => {
                Some(crate::script::plan(self, *script, target_frame))
            }
            Ok(EngineCommand::StopScript { run }) => {
                Some(crate::script::plan_stop(self, *run, target_frame))
            }
            // A shortcut's cue actions are planned the same way (RC-004).
            Ok(EngineCommand::FireShortcut {
                shortcut,
                targets,
                value,
            }) => Some(crate::shortcut::plan(
                self,
                *shortcut,
                *targets,
                *value,
                target_frame,
            )),
            _ => None,
        };
        let cue_transition = match &cue {
            Some(Ok(plan)) => plan.transition,
            _ => None,
        };
        // A media list step is planned once, here, against the pre-command
        // show and the list's transport (IN-006).
        let media_list = match resolution.as_ref().map(|resolved| &resolved.command) {
            Ok(EngineCommand::MediaList(command)) => {
                let playing = self.media_list_playing(command.input);
                Some(crate::media_list::plan(
                    self.commands.state(),
                    command,
                    playing,
                ))
            }
            _ => None,
        };
        let media_list_operations = match &media_list {
            Some(Ok(plan)) => crate::media_list::transport_operations(plan),
            _ => Vec::new(),
        };
        // A reel command is validated against the recorded media once, here:
        // the mutation installs a reel edit, and an accepted export is
        // submitted after the receipt is committed (OR-013).
        let reel = match resolution.as_ref().map(|resolved| &resolved.command) {
            Ok(EngineCommand::Reel(operation)) => Some(self.reel_mutation(operation)),
            _ => None,
        };
        let scheduled = match (&resolution, already_seen) {
            (Ok(resolved), false) => Some(
                self.scheduler
                    .schedule_action(
                        target_frame,
                        match (&playlist, &cue) {
                            (Some(Ok(step)), _) => EngineCommand::RealizePlaylistCue(step.cue),
                            (_, Some(Ok(plan))) => {
                                EngineCommand::RealizeCueActions(plan.runtime.clone())
                            }
                            _ if !media_list_operations.is_empty() => {
                                media_list_runtime(&resolved.command, &media_list_operations)
                            }
                            _ => runtime_action(self.commands.state(), &resolved.command),
                        },
                    )
                    .map_err(EngineError::Schedule)?,
            ),
            _ => None,
        };
        let runtime_busy = self.transition_in_flight;
        // The realized switcher lags the desired one by up to a frame and
        // keeps fading overlays on air, so a removal must also be free of
        // every realized reference or its scheduled action could not apply.
        let realized_usage = match resolution.as_ref().map(|resolved| &resolved.command) {
            Ok(EngineCommand::RemoveInput { input }) => self
                .realized_switcher
                .input_usage(*input)
                .map(|usage| (*input, usage)),
            Ok(EngineCommand::InputLifecycle(crate::EngineInputLifecycleEdit::RemoveInputs {
                inputs,
            })) => inputs.iter().find_map(|input| {
                self.realized_switcher
                    .input_usage(*input)
                    .map(|usage| (*input, usage))
            }),
            Ok(EngineCommand::InputLifecycle(crate::EngineInputLifecycleEdit::SetClosed {
                input,
                closed: true,
            })) => self
                .realized_switcher
                .on_air_usage(*input)
                .map(|usage| (*input, usage)),
            _ => None,
        };
        // Title air checks span both switchers for the same reason: a title
        // fading out of an overlay is still on air.
        // A title playing its Out phase may be taken again on the channel it
        // is leaving: that restarts its In phase.
        let realized_on_air = match resolution.as_ref().map(|resolved| &resolved.command) {
            Ok(EngineCommand::TakeTitle { input, channel, .. })
                if self.title_leaving_only(*input, *channel) =>
            {
                None
            }
            Ok(
                EngineCommand::SetTitleFields { input, .. }
                | EngineCommand::TakeTitle { input, .. }
                | EngineCommand::TitleOffAir { input },
            ) => self.realized_switcher.on_air_usage(*input),
            _ => None,
        };
        let paged_count = match resolution.as_ref().map(|resolved| &resolved.command) {
            Ok(EngineCommand::PageNavigate { input, .. }) => self.paged_counts.get(input).copied(),
            _ => None,
        };
        let media_tracks = match resolution.as_ref().map(|resolved| &resolved.command) {
            Ok(EngineCommand::Media(command)) => self
                .media
                .get(&command.input)
                .and_then(crate::media::MediaRuntime::track_indexes),
            _ => None,
        };
        // A cue's media actions queue on their transports exactly like a
        // media command once the GO or advance is accepted.
        let cue_media: Vec<crate::EngineMediaCommand> = match &cue {
            Some(Ok(plan)) => plan
                .runtime
                .iter()
                .filter_map(|action| match action {
                    EngineCommand::Media(media) => Some(*media),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let cue_pages: Vec<(InputId, fm_types::paged::PagedNavigation)> = match &cue {
            Some(Ok(plan)) => plan
                .runtime
                .iter()
                .filter_map(|action| match action {
                    EngineCommand::PageNavigate { input, navigation } => {
                        Some((*input, *navigation))
                    }
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let staged_resolution = resolution.clone();
        let staged_media_list = media_list.clone();
        let staged_reel = reel.clone();
        let title_clip_bound = self.title_clip_bound;
        let scene_clip_bound = self.scene_clip_bound;
        let outcome = self.commands.apply(envelope, now_millis, move |_, _| {
            let ResolvedCommand { command, .. } = staged_resolution?;
            if let Some(frame) = held_until {
                return Err(Rejection::new(
                    RejectionCode::Conflict,
                    format!("a playlist take is scheduled for frame {}", frame.get()),
                )
                .with_reason(RefusalReason::PlaylistTakeScheduled { frame: frame.get() }));
            }
            if let Some((input, usage)) = realized_usage {
                return Err(input_catalogue_rejection(InputCatalogueError::InUse {
                    input,
                    usage,
                }));
            }
            // Another script run due on the held frame joins it (schema
            // 60): it is planned on the show the held advance left, and
            // realized after it on the same frame, so runs due together
            // resume together.
            if let Some(frame) = cue_held_until
                && !(matches!(&command, EngineCommand::CueAdvance { list: 0, .. })
                    && frame == target_frame)
            {
                return Err(Rejection::new(
                    RejectionCode::Conflict,
                    format!("cue actions are scheduled for frame {}", frame.get()),
                )
                .with_reason(RefusalReason::CueScheduled { frame: frame.get() }));
            }
            // A cue GO checks each of its actions against the transition in
            // flight while it is planned.
            if let Some(kind) = runtime_busy
                && refused_during_transition(&command)
                && !matches!(
                    &command,
                    EngineCommand::CueControl { .. }
                        | EngineCommand::CueAdvance { .. }
                        | EngineCommand::SetCueList { .. }
                        | EngineCommand::RunScript { .. }
                        | EngineCommand::StopScript { .. }
                        | EngineCommand::SetScript { .. }
                        | EngineCommand::FireShortcut { .. }
                        | EngineCommand::SetShortcut { .. }
                )
            {
                let name = transition_kind_name(kind);
                return Err(Rejection::new(
                    RejectionCode::Conflict,
                    format!("a {name} transition is already in flight"),
                ));
            }
            Ok(EngineMutation {
                command,
                target_frame,
                realized_on_air,
                playlist,
                title_clip_bound,
                scene_clip_bound,
                media_tracks,
                media_list: staged_media_list,
                paged_count,
                cue,
                reel: staged_reel,
            })
        });

        if !outcome.replayed {
            self.receipts_recorded = self.receipts_recorded.wrapping_add(1);
        }
        let accepted_revision = outcome
            .receipt
            .accepted()
            .filter(|_| !outcome.replayed)
            .map(|accepted| accepted.revision);
        if let (Some(revision), Ok(resolved)) = (accepted_revision, resolution) {
            let ResolvedCommand { command, step } = resolved;
            self.pending_actions += 1;
            if let Some(kind) = runtime_transition_kind(self.commands.state(), &command)
                .or(playlist_transition)
                .or(cue_transition)
            {
                self.transition_in_flight = Some(kind);
            }
            if target_frame > next_frame {
                if matches!(&command, EngineCommand::CueAdvance { .. }) {
                    self.cue_hold = Some(target_frame);
                } else {
                    self.playlist_hold = Some(target_frame);
                }
            }
            if let EngineCommand::Media(media) = &command {
                self.media
                    .entry(media.input)
                    .or_default()
                    .accept(media.operation);
            }
            // A document's page moved with the show; a slideshow steps on
            // its target frame.
            let show = self.commands.state();
            if let EngineCommand::PageNavigate { input, navigation } = &command
                && crate::slideshow::paged_config(show, *input).is_ok()
            {
                self.slideshows
                    .entry(*input)
                    .or_default()
                    .accept(*navigation);
            }
            for (input, navigation) in cue_pages {
                if crate::slideshow::paged_config(show, input).is_ok() {
                    self.slideshows.entry(input).or_default().accept(navigation);
                }
            }
            for media in cue_media {
                self.media
                    .entry(media.input)
                    .or_default()
                    .accept(media.operation);
            }
            if let Some(Ok(plan)) = &media_list {
                let runtime = self.media.entry(plan.input).or_default();
                for operation in &media_list_operations {
                    runtime.accept(*operation);
                }
                // A new file opens stopped, or playing when the step plays.
                if let Some(activation) = &plan.activation
                    && !activation.same_source
                {
                    runtime.play_on_open = activation.play;
                }
                // A playing step blends the two items when its transition
                // does: the transport as the step finds it plays out. Any
                // other step is a cut; an edit leaving the active item
                // keeps a step not yet realized.
                if let Some(activation) = &plan.activation {
                    runtime.pending_transition = Some(if activation.play {
                        plan.list.options.transition
                    } else {
                        fm_model::MediaListTransition::CUT
                    });
                }
            }
            match reel {
                Some(Ok(crate::reel::ReelMutation::Export(plan))) => {
                    assert!(
                        self.replay.submit_export(plan),
                        "a planned reel export submits"
                    );
                }
                Some(Ok(crate::reel::ReelMutation::Cancel { job })) => {
                    self.replay
                        .cancel_export(job)
                        .expect("a planned reel export cancels");
                }
                Some(Ok(crate::reel::ReelMutation::Forget { job })) => {
                    self.replay
                        .forget_export(job)
                        .expect("a planned reel export is forgotten");
                }
                Some(Ok(crate::reel::ReelMutation::Edit)) => self.replay.touch(),
                Some(Err(_)) | None => {}
            }
            if !matches!(step, HistoryStep::None) {
                self.history.commit(step, command, command_id, revision);
            }
        } else if let Some(action) = scheduled {
            self.scheduler.cancel_action(action);
        }
        Ok(outcome)
    }

    /// Whether the only realized on-air use of title `input` is `channel`
    /// holding it while its Out phase plays.
    fn title_leaving_only(&self, input: InputId, channel: OverlayChannelId) -> bool {
        self.title_air
            .get(&input)
            .is_some_and(|(shown_on, air)| *shown_on == channel && air.is_leaving())
            && self.realized_switcher.on_air_usage(input) == Some(InputUsage::Overlay(channel))
            && self
                .realized_switcher
                .overlay_channels_showing(input)
                .eq([channel])
    }

    fn preparation_base(&self) -> EnginePreparationBase {
        EnginePreparationBase {
            state_epoch: self.commands.state_epoch(),
            revision: self.commands.revision(),
            event_sequence: self.commands.event_sequence(),
            receipts_recorded: self.receipts_recorded,
            frame_cursor: self.scheduler.pacer().next_frame(),
            clock_time: self.clock.now(),
            runtime_generation: self.runtime_generation,
        }
    }

    /// Advances every media input's transport one frame; `operations`
    /// counts the media commands realized on this frame per input.
    fn tick_media(
        &mut self,
        operations: &BTreeMap<InputId, usize>,
    ) -> Vec<crate::EngineMediaFrame> {
        let show = self.commands.state();
        self.media
            .retain(|input, _| show.media_settings(*input).is_some());
        self.media
            .iter_mut()
            .filter_map(|(input, runtime)| {
                let settings = show.media_settings(*input)?;
                let lifecycle = show.input_lifecycle(*input);
                // A closed input's source is released, as is a replaced
                // one; a paused one holds. A release reports one frame
                // without a tick, so clients see the transport go.
                let released = runtime.is_open()
                    && (lifecycle.closed
                        || runtime.opened_for.as_ref() != show.input_source(*input));
                if released {
                    runtime.release(!lifecycle.closed);
                }
                runtime
                    .tick(
                        *input,
                        operations.get(input).copied().unwrap_or(0),
                        settings,
                        lifecycle.paused || lifecycle.closed,
                    )
                    .or_else(|| {
                        released.then(|| crate::EngineMediaFrame {
                            input: *input,
                            tick: None,
                            operations: Vec::new(),
                            faulted: None,
                            completion_requested: false,
                            outgoing: None,
                        })
                    })
            })
            .collect()
    }

    /// Advances every slideshow input one frame; `operations` counts the
    /// page navigations realized on this frame per input.
    fn tick_slideshows(
        &mut self,
        operations: &BTreeMap<InputId, usize>,
    ) -> Vec<crate::EngineSlideshowFrame> {
        let show = self.commands.state();
        self.slideshows
            .retain(|input, _| crate::slideshow::paged_config(show, *input).is_ok());
        for input in show.inputs() {
            if crate::slideshow::paged_config(show, *input).is_ok() {
                self.slideshows.entry(*input).or_default();
            }
        }
        self.slideshows
            .iter_mut()
            .filter_map(|(input, runtime)| {
                let config = crate::slideshow::paged_config(show, *input).ok()?;
                let lifecycle = show.input_lifecycle(*input);
                Some(runtime.tick(
                    *input,
                    config,
                    operations.get(input).copied().unwrap_or(0),
                    lifecycle.paused || lifecycle.closed,
                ))
            })
            .collect()
    }

    /// Renders the next frame at its exact simulated deadline.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the clock, scheduler, switcher, or runtime
    /// generation cannot advance.
    #[allow(clippy::too_many_lines)]
    pub fn tick(&mut self) -> Result<FrameResult, EngineError> {
        let deadline = self
            .scheduler
            .pacer()
            .next_deadline()
            .map_err(|error| EngineError::Tick(fm_scheduler::TickError::Pacing(error)))?;
        self.clock.set(ClockTime::from_nanos(deadline.at_ns))?;
        let tick = self.scheduler.tick(deadline.at_ns)?;
        self.pending_actions = self.pending_actions.saturating_sub(tick.actions.len());
        if self
            .playlist_hold
            .is_some_and(|frame| frame <= tick.deadline.frame)
        {
            self.playlist_hold = None;
        }
        if self
            .cue_hold
            .is_some_and(|frame| frame <= tick.deadline.frame)
        {
            self.cue_hold = None;
        }

        let mut events = Vec::new();
        let mut input_audio_strip_updates = Vec::new();
        let mut audio_mixer_updates = Vec::new();
        let mut media_operations = BTreeMap::<InputId, usize>::new();
        let mut page_operations = BTreeMap::<InputId, usize>::new();
        let frame = tick.deadline.frame.get();
        self.action_motions.clear();
        for scheduled in tick.actions {
            let mut motion = EngineActionMotion::default();
            let mut realized = RealizedActions {
                events: &mut events,
                input_audio_strip_updates: &mut input_audio_strip_updates,
                audio_mixer_updates: &mut audio_mixer_updates,
                media_operations: &mut media_operations,
                page_operations: &mut page_operations,
                motion: &mut motion,
            };
            self.realize_action(scheduled.action, frame, &mut realized)?;
            self.action_motions.push(motion);
            self.runtime_generation = self.runtime_generation.checked_next()?;
        }

        let program = self.realized_switcher.program_frame();
        let fade_to_black = self.realized_switcher.fade_to_black_frame();
        let fade_to_black_excluded_outputs = self
            .realized_switcher
            .fade_to_black_excluded_outputs()
            .iter()
            .copied()
            .collect();
        let fade_to_black_overrides = self.realized_switcher.fade_to_black_override_frames();
        let manual_transition = self.realized_switcher.transition().is_none()
            && self.realized_switcher.t_bar().is_some();
        let overlays = self.realized_switcher.overlays().clone();
        // A plain overlay command that replaced or removed a title's source
        // ends its animation.
        let realized = &self.realized_switcher;
        self.title_air.retain(|input, (channel, _)| {
            realized
                .overlay_channels_showing(*input)
                .any(|shown_on| shown_on == *channel)
        });
        let titles = title_air_frames(&self.title_air, frame);
        crate::scene_animation::step_scene_air(
            &mut self.scene_air,
            self.commands.state(),
            &program,
            self.realized_switcher.preview(),
            frame,
            self.frame_rate,
        );
        let scenes = crate::scene_animation::scene_air_frames(&self.scene_air, frame);
        crate::title_timeline::step_titles_shown(
            &mut self.title_shown,
            self.commands.state(),
            &program,
            &self.realized_switcher,
            &self.title_air,
            frame,
        );
        let titles_shown = crate::title_timeline::shown_frames(
            &self.title_shown,
            self.commands.state(),
            frame,
            self.frame_rate,
        );
        for event in self.realized_switcher.advance_frame_events() {
            if matches!(event, SwitcherEvent::ProgramChanged { .. }) {
                self.transition_in_flight = None;
            }
            events.push(event);
        }
        let next = frame.saturating_add(1);
        // A title's channel goes off when its Out phase ends, even if a plain
        // overlay off replaced the hold with a longer fade.
        for (input, (channel, air)) in &self.title_air {
            if air.is_leaving()
                && air.at(next).is_none()
                && self
                    .realized_switcher
                    .overlay_channels_showing(*input)
                    .any(|shown_on| shown_on == *channel)
            {
                events.extend(
                    self.realized_switcher
                        .request_overlay_off_after(*channel, 0),
                );
            }
        }
        self.title_air.retain(|_, (_, air)| air.at(next).is_some());

        Ok(FrameResult {
            frame: tick.deadline.frame,
            deadline: ClockTime::from_nanos(tick.deadline.at_ns),
            program,
            fade_to_black,
            overlays,
            events,
            input_audio_strip_updates,
            audio_mixer_updates,
            fade_to_black_excluded_outputs,
            fade_to_black_overrides,
            manual_transition,
            titles,
            scenes,
            titles_shown,
            media: self.tick_media(&media_operations),
            slideshows: self.tick_slideshows(&page_operations),
            replay: self.replay.step(self.commands.state(), frame),
            revision: self.commands.revision(),
            runtime_generation: self.runtime_generation,
        })
    }

    /// Realizes one scheduled runtime action on the realized switcher at
    /// `frame`; a cue's actions realize in order, as one action.
    fn realize_action(
        &mut self,
        action: EngineCommand,
        frame: u64,
        realized: &mut RealizedActions<'_>,
    ) -> Result<(), EngineError> {
        if let EngineCommand::RealizeCueActions(actions) = action {
            for action in actions {
                self.realize_action(action, frame, realized)?;
            }
            return Ok(());
        }
        let titles_before = self.title_air.clone();
        let overlays_before = self.realized_switcher.overlays().clone();
        if let EngineCommand::SetInputAudioStrip { input, state } = action {
            realized.input_audio_strip_updates.push((input, state));
        }
        if let EngineCommand::AudioMixer(edit) = &action
            && let Some(update) = crate::EngineAudioMixerUpdate::of(edit)
        {
            realized.audio_mixer_updates.push(update);
        }
        let command_events = if let EngineCommand::Replay(operation) = &action {
            self.replay.realize(self.commands.state(), operation, frame);
            Vec::new()
        } else if let EngineCommand::Media(command) = action {
            *realized.media_operations.entry(command.input).or_default() += 1;
            Vec::new()
        } else if let EngineCommand::PageNavigate { input, .. } = action {
            *realized.page_operations.entry(input).or_default() += 1;
            Vec::new()
        } else if let EngineCommand::RealizeTitle(cue) = action {
            realize_title(
                &mut self.realized_switcher,
                &mut self.title_air,
                cue,
                frame,
                self.frame_rate,
            )?
        } else {
            // A plain take on a title's channel ends its animation, even
            // when it takes the same title: it is on air the plain way.
            if let EngineCommand::TakeOverlay { channel, .. }
            | EngineCommand::TakeNextOverlay { channel } = &action
            {
                self.title_air
                    .retain(|_, (shown_on, _)| shown_on != channel);
            }
            apply_runtime(&mut self.realized_switcher, action)?
        };
        realized.events.extend(command_events);
        realized
            .motion
            .record_titles(&titles_before, &self.title_air);
        realized
            .motion
            .record_channels(&overlays_before, self.realized_switcher.overlays());
        Ok(())
    }

    /// What each runtime action the last [`Self::tick`] realized changed of
    /// the on-air animations, one entry per action in realization order
    /// (the order of their runtime generations).
    #[must_use]
    pub fn action_motions(&self) -> &[EngineActionMotion] {
        &self.action_motions
    }

    /// Whether accepted work is still being realized frame by frame: a
    /// pending action, a timed transition or Fade to Black, an overlay in
    /// motion, or a title on air mid In or Out phase. A snapshot is taken
    /// only once none is (see [`Self::snapshot`]).
    #[must_use]
    pub fn work_in_flight(&self) -> bool {
        self.pending_actions != 0
            || self.transition_in_flight.is_some()
            || self.realized_switcher.transition().is_some()
            || self
                .commands
                .state()
                .desired_switcher()
                .fade_to_black_is_automatic()
            || self.realized_switcher.fade_to_black_is_automatic()
            || self.realized_switcher.overlays_in_motion()
            || self
                .titles_on_air()
                .iter()
                .any(|title| title.frame.phase != TitleAirPhase::Hold)
    }

    /// How the replay recording is fed (OR-012).
    #[must_use]
    pub const fn replay_feed(&self) -> crate::EngineReplayFeed {
        self.replay.feed()
    }

    /// Feeds the replay recording synthetically or from a media plane; a
    /// change starts a new recording (OR-012).
    pub fn set_replay_feed(&mut self, feed: crate::EngineReplayFeed) {
        self.replay.set_feed(feed);
    }

    /// The storage root replay uses when the show's settings name none; a
    /// change starts a new recording (OR-012).
    pub fn set_replay_default_root(&mut self, root: std::path::PathBuf) {
        self.replay.set_default_root(root);
    }

    /// The storage root replay records into (OR-012).
    #[must_use]
    pub fn replay_root(&self) -> &std::path::Path {
        self.replay.root()
    }

    /// Records the free bytes on the replay storage root, as the media
    /// plane last sampled them, or `None` when it cannot sample them
    /// (OR-014).
    pub fn set_replay_storage_free(&mut self, free: Option<u64>) {
        self.replay.set_storage_free(free);
    }

    /// Reports that an external replay recording recorded engine frame
    /// `frame` of source `input` as stream frame `stream_frame` (OR-012).
    /// Returns whether it was taken: the input is recorded and the frame
    /// follows the previous one.
    pub fn replay_observe(&mut self, input: InputId, frame: u64, stream_frame: u64) -> bool {
        self.replay.observe(input, frame, stream_frame)
    }

    /// Catalogs a finished segment file of an external replay recording
    /// (OR-012): `frames` stream frames from `first_stream_frame`, the first
    /// recorded on engine frame `first_frame`. Returns whether it was
    /// taken.
    #[allow(clippy::too_many_arguments)]
    pub fn replay_segment_closed(
        &mut self,
        input: InputId,
        recorder_index: u64,
        path: std::path::PathBuf,
        first_frame: u64,
        first_stream_frame: u64,
        frames: u64,
        bytes: u64,
    ) -> bool {
        self.replay.segment_closed(
            input,
            recorder_index,
            path,
            first_frame,
            first_stream_frame,
            frames,
            bytes,
        )
    }

    /// Takes the segment files of an external recording that retention or
    /// a new recording dropped, to be deleted (OR-012).
    pub fn take_replay_pruned(&mut self) -> Vec<std::path::PathBuf> {
        self.replay.take_pruned()
    }

    /// The four-angle view of Replay input `input` at its current position,
    /// or `None` when nothing is cued on it (OR-014).
    #[must_use]
    pub fn replay_multiview(&self, input: InputId) -> Option<crate::EngineReplayMultiview> {
        self.replay.multiview(input)
    }

    /// The replay recording, events, and players as clients see them
    /// (OR-012).
    #[must_use]
    pub fn replay_status(&self) -> crate::EngineReplayStatus {
        self.replay.status(self.commands.state())
    }

    /// A counter that moves whenever [`Self::replay_status`] may have
    /// changed (OR-012).
    #[must_use]
    pub const fn replay_generation(&self) -> u64 {
        self.replay.generation()
    }

    /// The highlight reels and their export jobs as clients see them
    /// (OR-013).
    #[must_use]
    pub fn reel_status(&self) -> crate::EngineReelStatus {
        self.replay.reel_status(self.commands.state())
    }

    /// A counter that moves whenever [`Self::reel_status`] may have changed
    /// (OR-013).
    #[must_use]
    pub const fn reel_generation(&self) -> u64 {
        self.replay.generation()
    }

    /// The compiled output length of reel `reel` in frames (OR-013).
    ///
    /// # Errors
    ///
    /// Returns [`crate::EngineReelError::UnknownReel`] when the show holds
    /// no such reel, or the compile error when its clips do not resolve
    /// against the recorded events.
    pub fn reel_plan(&self, reel: u64) -> Result<u64, crate::EngineReelError> {
        self.replay.reel_plan(self.commands.state(), reel)
    }

    /// Resolves up to `max_frames` further output frames of export `job`,
    /// each layer naming its segment file and exact frame inside it
    /// (OR-013). The media plane needs no catalog access of its own.
    ///
    /// # Errors
    ///
    /// Returns [`crate::EngineReelError::UnknownExport`] for a job that does
    /// not exist, [`crate::EngineReelError::ExportNotActive`] for a terminal
    /// one, and [`crate::EngineReelError::Export`] when the planner refuses
    /// the chunk or the recorded media changed under it.
    pub fn reel_export_chunk(
        &mut self,
        job: u64,
        max_frames: u64,
    ) -> Result<crate::EngineReelChunk, crate::EngineReelError> {
        self.replay.advance_export(job, max_frames)
    }

    /// Reports that the media plane wrote export `job`'s file, `bytes`
    /// long (OR-013). The export must have resolved every output frame.
    ///
    /// # Errors
    ///
    /// Returns [`crate::EngineReelError::UnknownExport`] or
    /// [`crate::EngineReelError::ExportNotActive`] for a job that is not
    /// finalizing, and [`crate::EngineReelError::ExportNotFinalizing`] while
    /// frames remain.
    pub fn reel_export_finished(
        &mut self,
        job: u64,
        bytes: u64,
    ) -> Result<(), crate::EngineReelError> {
        self.replay.export_finished(job, bytes)
    }

    /// Reports that the media plane could not write export `job`'s file,
    /// releasing its retention protection (OR-013).
    ///
    /// # Errors
    ///
    /// Returns [`crate::EngineReelError::UnknownExport`] or
    /// [`crate::EngineReelError::ExportNotActive`] for a terminal job.
    pub fn reel_export_failed(
        &mut self,
        job: u64,
        reason: impl Into<String>,
    ) -> Result<(), crate::EngineReelError> {
        self.replay.export_failed(job, reason.into())
    }

    /// Captures durable and realized state at an idle frame boundary.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::WorkInFlight`] while an action or transition
    /// is pending.
    pub fn snapshot(&self) -> Result<EngineSnapshot, SnapshotError> {
        if self.work_in_flight() {
            return Err(SnapshotError::WorkInFlight);
        }
        let mut receipts: Vec<_> = self
            .commands
            .receipts()
            .map(|(key, receipt)| (key.clone(), receipt.clone()))
            .collect();
        receipts.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
        Ok(EngineSnapshot {
            show: self.commands.state().clone(),
            realized_switcher: self.realized_switcher.clone(),
            state_epoch: self.commands.state_epoch(),
            revision: self.commands.revision(),
            event_sequence: self.commands.event_sequence(),
            runtime_generation: self.runtime_generation,
            frame_rate: self.frame_rate,
            clock_domain: self.clock.domain(),
            clock_time: self.clock.now(),
            frames_rendered: self.scheduler.pacer().next_frame().get(),
            receipts,
            title_timelines: crate::title_timeline::saved(&self.title_shown),
        })
    }

    /// Restores an idle snapshot, including durable and runtime counters.
    ///
    /// # Errors
    ///
    /// Returns an error if the saved scheduler cursor cannot be reconstructed.
    pub fn restore(snapshot: EngineSnapshot) -> Result<Self, EngineError> {
        let restore_state = EngineRestoreState {
            state_epoch: snapshot.state_epoch,
            revision: snapshot.revision,
            event_sequence: snapshot.event_sequence,
            runtime_generation: snapshot.runtime_generation,
            clock_time: snapshot.clock_time,
            frame_cursor: FrameNumber::new(snapshot.frames_rendered),
            receipts: snapshot.receipts,
        };
        let mut engine = Self::restore_persisted(
            snapshot.show,
            snapshot.realized_switcher,
            snapshot.frame_rate,
            snapshot.clock_domain,
            restore_state,
        )?;
        engine.restore_title_timelines(&snapshot.title_timelines)?;
        Ok(engine)
    }

    /// Restores durable and idle runtime state supplied by persistence.
    ///
    /// # Errors
    ///
    /// Returns a [`SnapshotError`] if the persisted state is not a consistent
    /// idle frame boundary or its frame cursor cannot be reconstructed.
    pub fn restore_persisted(
        show: ShowState,
        realized_switcher: SwitcherState,
        frame_rate: FrameRate,
        clock_domain: ClockDomainId,
        restore_state: EngineRestoreState,
    ) -> Result<Self, SnapshotError> {
        validate_idle_restore(&show, &realized_switcher, &restore_state, frame_rate)?;
        let scheduler = RuntimeScheduler::restore(
            frame_rate,
            0,
            restore_state.frame_cursor,
            PlanGeneration::new(0),
            Arc::new(()),
        )
        .map_err(|_| SnapshotError::InvalidFrameCounter)?;
        Ok(Self {
            commands: CommandState::restore_with_receipts(
                show,
                restore_state.state_epoch,
                restore_state.revision,
                restore_state.event_sequence,
                restore_state.receipts,
            )
            .with_receipt_limit(RECEIPT_LIMIT),
            receipts_recorded: 0,
            realized_switcher,
            scheduler,
            clock: ManualClock::new(clock_domain, restore_state.clock_time),
            frame_rate,
            runtime_generation: restore_state.runtime_generation,
            pending_actions: 0,
            transition_in_flight: None,
            history: EditHistory::default(),
            playlist_hold: None,
            cue_hold: None,
            title_air: BTreeMap::new(),
            action_motions: Vec::new(),
            title_clip_bound: None,
            scene_air: BTreeMap::new(),
            title_shown: BTreeMap::new(),
            scene_clip_bound: None,
            media: BTreeMap::new(),
            slideshows: BTreeMap::new(),
            paged_counts: BTreeMap::new(),
            replay: crate::replay::ReplayRuntime::new(frame_rate),
        })
    }
}

/// A saved script run's last part was realized before the frame `cursor`,
/// and its due frame is that part's frame plus one wait, so it lies within
/// the run's waited frames of the cursor (RC-011, schema 56). A further one
/// (a hand-edited project) would hold the cue clock, and block a local CLI
/// run that plays the clock to its end, for ever.
fn validate_script_run_reach(show: &ShowState, cursor: u64) -> Result<(), SnapshotError> {
    match show.script_run().runs.iter().find(|run| {
        cursor
            .checked_add(u64::from(run.state.wait_frames))
            .is_none_or(|reach| run.due_frame > reach)
    }) {
        Some(run) => Err(SnapshotError::ScriptRunOutOfReach {
            due_frame: run.due_frame,
            frame_cursor: cursor,
        }),
        None => Ok(()),
    }
}

/// A waiting cue's pending step follows a wait (project validation), and
/// the cue stack schedules it on the frame that wait was reached plus the
/// wait's frames at the engine rate. That frame ran before the frame
/// `cursor`, and a waiting cue's actions cannot change, so its due frame
/// lies within that wait of the cursor (RC-017). A further one (a
/// hand-edited project) would hold its list, and block a local CLI run
/// that plays the clock to its end, for ever.
fn validate_cue_run_reach(
    show: &ShowState,
    cursor: u64,
    frame_rate: FrameRate,
) -> Result<(), SnapshotError> {
    // An armed GO (schema 58) fires at most the longest arm delay after
    // the frame it was armed on, which ran before `cursor`.
    let arm_reach = cursor.checked_add(u64::from(fm_model::delay_frames(
        fm_model::MAX_CUE_ARM_MS,
        frame_rate,
    )));
    for list in show.cue_lists() {
        if let Some(armed) = list.state.armed
            && arm_reach.is_none_or(|reach| armed.due_frame > reach)
        {
            return Err(SnapshotError::CueRunOutOfReach {
                list: list.id,
                due_frame: armed.due_frame,
                frame_cursor: cursor,
            });
        }
        let Some(pending) = list.state.pending else {
            continue;
        };
        let wait = list
            .cue(pending.cue)
            .and_then(|cue| cue.actions.get(pending.action.checked_sub(1)?))
            .and_then(|action| match action {
                fm_model::CueAction::Wait { frames } => Some(*frames),
                fm_model::CueAction::Delay { millis } => {
                    Some(fm_model::delay_frames(*millis, frame_rate))
                }
                _ => None,
            });
        let reach = wait.and_then(|wait| cursor.checked_add(u64::from(wait)));
        if reach.is_none_or(|reach| pending.due_frame > reach) {
            return Err(SnapshotError::CueRunOutOfReach {
                list: list.id,
                due_frame: pending.due_frame,
                frame_cursor: cursor,
            });
        }
    }
    Ok(())
}

fn validate_idle_restore(
    show: &ShowState,
    realized_switcher: &SwitcherState,
    restore_state: &EngineRestoreState,
    frame_rate: FrameRate,
) -> Result<(), SnapshotError> {
    if show.desired_switcher().transition().is_some() || realized_switcher.transition().is_some() {
        return Err(SnapshotError::WorkInFlight);
    }
    if show.desired_switcher().fade_to_black_is_automatic()
        || realized_switcher.fade_to_black_is_automatic()
    {
        return Err(SnapshotError::WorkInFlight);
    }
    if show.inputs() != realized_switcher.inputs() {
        return Err(SnapshotError::IncompatibleSwitcher);
    }
    if show.desired_switcher().program() != realized_switcher.program()
        || show.desired_switcher().preview() != realized_switcher.preview()
        || show.desired_switcher().fade_to_black() != realized_switcher.fade_to_black()
        || show.desired_switcher().fade_to_black_position()
            != realized_switcher.fade_to_black_position()
        || show.desired_switcher().fade_to_black_excluded_outputs()
            != realized_switcher.fade_to_black_excluded_outputs()
        || !show
            .desired_switcher()
            .fade_to_black_output_durations()
            .eq(realized_switcher.fade_to_black_output_durations())
        || show.desired_switcher().fade_to_black_curve() != realized_switcher.fade_to_black_curve()
        || show.desired_switcher().stingers() != realized_switcher.stingers()
        || show.desired_switcher().overlays() != realized_switcher.overlays()
        || show.desired_switcher().streams() != realized_switcher.streams()
        || show.desired_switcher().running_stream_targets()
            != realized_switcher.running_stream_targets()
        || RecorderSlot::ALL.into_iter().any(|slot| {
            show.desired_switcher().recording_desired(slot)
                != realized_switcher.recording_desired(slot)
        })
    {
        return Err(SnapshotError::MismatchedSwitcherRouting);
    }
    match (show.desired_switcher().t_bar(), realized_switcher.t_bar()) {
        (None, None) => {}
        (Some(desired), Some(realized))
            if desired.kind() == realized.kind()
                && desired.clip_frames() == realized.clip_frames()
                && desired.from() == realized.from()
                && desired.to() == realized.to()
                && desired.interval_start() == TBarPosition::START
                && desired.position() == realized.position()
                && realized.interval_start() == realized.position() => {}
        _ => return Err(SnapshotError::MismatchedManualTransition),
    }

    let cursor = restore_state.frame_cursor.get();
    let pacer = fm_scheduler::FramePacer::new(frame_rate, 0);
    let expected_clock_ns = if cursor == 0 {
        0
    } else {
        pacer
            .deadline_for(FrameNumber::new(cursor - 1))
            .map_err(|_| SnapshotError::InvalidFrameCounter)?
            .at_ns
    };
    if restore_state.clock_time.as_nanos() != expected_clock_ns {
        return Err(SnapshotError::ClockTimeMismatch {
            expected_ns: expected_clock_ns,
            actual_ns: restore_state.clock_time.as_nanos(),
        });
    }

    let mut accepted_commands = 0_u64;
    for (_, receipt) in &restore_state.receipts {
        if let Some(accepted) = receipt.accepted() {
            accepted_commands = accepted_commands
                .checked_add(1)
                .ok_or(SnapshotError::InvalidFrameCounter)?;
            if accepted.result.target_frame >= restore_state.frame_cursor {
                return Err(SnapshotError::UnrealizedAcceptedReceipt {
                    target_frame: accepted.result.target_frame.get(),
                    frame_cursor: cursor,
                });
            }
        }
    }

    validate_script_run_reach(show, cursor)?;
    validate_cue_run_reach(show, cursor, frame_rate)?;

    // Every accepted command advances all three counters together, so they
    // must agree. Persisted receipts are a bounded retention window over the
    // accepted commands, so they may cover fewer commands than the counters
    // record, but never more.
    if accepted_commands > restore_state.revision.get()
        || restore_state.event_sequence.get() != restore_state.revision.get()
        || restore_state.runtime_generation.get() != restore_state.revision.get()
    {
        return Err(SnapshotError::CounterMismatch {
            accepted_commands,
            revision: restore_state.revision.get(),
            event_sequence: restore_state.event_sequence.get(),
            runtime_generation: restore_state.runtime_generation.get(),
        });
    }
    Ok(())
}

#[derive(Debug)]
struct EngineMutation {
    command: EngineCommand,
    target_frame: FrameNumber,
    /// The realized switcher's on-air use of a title command's input.
    realized_on_air: Option<InputUsage>,
    /// The stepped playlist of a playlist command.
    playlist: Option<Result<PlaylistStep, Rejection>>,
    /// The engine's title clip bound, for an animation edit.
    title_clip_bound: Option<crate::TitleClipBound>,
    /// The engine's scene clip bound, for a scene animation edit.
    scene_clip_bound: Option<crate::SceneClipBound>,
    /// The open source's audio stream indexes, for a media command.
    media_tracks: Option<Vec<u32>>,
    /// The planned step of a media list command (IN-006).
    media_list: Option<Result<crate::media_list::MediaListPlan, Rejection>>,
    /// The input's reported page count, for a paged command.
    paged_count: Option<u32>,
    /// The planned cue GO, operation, or advance of a cue runtime command.
    cue: Option<Result<CuePlan, Rejection>>,
    /// The validated runtime work of a reel command (OR-013).
    reel: Option<Result<crate::reel::ReelMutation, Rejection>>,
}

/// Where one tick collects what its runtime actions realized.
struct RealizedActions<'a> {
    events: &'a mut Vec<SwitcherEvent>,
    input_audio_strip_updates: &'a mut Vec<(InputId, EngineInputAudioStripState)>,
    audio_mixer_updates: &'a mut Vec<crate::EngineAudioMixerUpdate>,
    media_operations: &'a mut BTreeMap<InputId, usize>,
    page_operations: &'a mut BTreeMap<InputId, usize>,
    /// What the action changed of the on-air animations.
    motion: &'a mut EngineActionMotion,
}

/// A planned cue command (see [`crate::cue`]): the show once every due
/// action ran, the event announcing it, the runtime actions realizing it,
/// and the Program transition it starts.
#[derive(Clone, Debug)]
pub(crate) struct CuePlan {
    pub(crate) show: Box<ShowState>,
    pub(crate) event: EngineEvent,
    pub(crate) runtime: Vec<EngineCommand>,
    pub(crate) transition: Option<TransitionKind>,
}

/// Most title text bytes the actions of one cue or script frame may set:
/// what one script run's whole action budget can set
/// ([`fm_script::DEFAULT_ACTIONS`] texts of [`fm_model::MAX_CUE_TEXT_BYTES`]).
pub(crate) const MAX_FRAME_TITLE_TEXT_BYTES: usize =
    fm_script::DEFAULT_ACTIONS as usize * fm_model::MAX_CUE_TEXT_BYTES;

/// Records the title values a title text or number action's `commands`
/// set (schema 59) for the frame's event, refusing a frame past the bound
/// of [`check_frame_title_texts`].
fn record_title_values(
    title_texts: &mut Vec<(InputId, FieldId, FieldValue)>,
    action: &fm_model::ResolvedCueAction,
    commands: &[EngineCommand],
) -> Result<(), Rejection> {
    if !matches!(
        action,
        fm_model::CueAction::TitleText { .. } | fm_model::CueAction::TitleNumber { .. }
    ) {
        return Ok(());
    }
    for command in commands {
        if let EngineCommand::SetTitleFields { input, fields, .. } = command {
            title_texts.extend(
                fields
                    .iter()
                    .map(|(field, value)| (*input, *field, value.clone())),
            );
        }
    }
    check_frame_title_texts(title_texts)
}

/// Refuses a frame whose actions set more title text than its event may
/// carry: a cue list and a lone script run stay far inside the bound, only
/// script steps on one cue frame can reach it.
fn check_frame_title_texts(
    title_texts: &[(InputId, FieldId, FieldValue)],
) -> Result<(), Rejection> {
    let bytes: usize = title_texts
        .iter()
        .map(|(_, _, value)| match value {
            FieldValue::Text(text) => text.len(),
            _ => 0,
        })
        .sum();
    if title_texts.len() > fm_model::MAX_CUE_LIST_ACTIONS || bytes > MAX_FRAME_TITLE_TEXT_BYTES {
        return Err(Rejection::new(
            RejectionCode::ResourceExhausted,
            format!(
                "the actions set more than {MAX_FRAME_TITLE_TEXT_BYTES} bytes of title text on \
                 one frame"
            ),
        )
        .with_reason(RefusalReason::bounds(
            "title_text",
            MAX_FRAME_TITLE_TEXT_BYTES as u64,
        )));
    }
    Ok(())
}

/// Records a cue frame's script step for its event. A frame runs one
/// list's steps, each at most once, so the list's script step bound holds
/// and the event can carry every step; the check keeps that true by
/// construction.
fn record_script_step(scripts: &mut Vec<CueScriptRun>, ran: CueScriptRun) -> Result<(), Rejection> {
    if scripts.len() >= fm_model::MAX_CUE_LIST_SCRIPT_STEPS {
        return Err(Rejection::new(
            RejectionCode::ResourceExhausted,
            format!(
                "the actions run more than {} scripts on one frame",
                fm_model::MAX_CUE_LIST_SCRIPT_STEPS
            ),
        )
        .with_reason(RefusalReason::bounds(
            "script_steps",
            fm_model::MAX_CUE_LIST_SCRIPT_STEPS as u64,
        )));
    }
    scripts.push(ran);
    Ok(())
}

/// What the actions applied so far on one frame collected: their runtime
/// actions, the Program transition they start, the title texts they set,
/// and how many took Program.
pub(crate) struct CueFrame<'a> {
    pub(crate) runtime: &'a mut Vec<EngineCommand>,
    pub(crate) transition: &'a mut Option<TransitionKind>,
    pub(crate) title_texts: &'a mut Vec<(InputId, FieldId, FieldValue)>,
    /// The script steps run so far, in order; `None` inside a script,
    /// whose actions never run a script.
    pub(crate) scripts: Option<&'a mut Vec<CueScriptRun>>,
    pub(crate) takes: usize,
    /// The inputs the frame's dynamic targets resolve to (RC-004).
    pub(crate) targets: TargetBindings,
}

/// The targets of one cue command (see `Engine::cue_targets`): what its
/// frame's actions resolve under, and what the list's run and armed GO
/// keep after it.
#[derive(Clone, Copy, Debug, Default)]
struct CueTargets {
    frame: TargetBindings,
    run: Option<TargetBindings>,
    armed: Option<TargetBindings>,
}

impl Mutation<ShowState, EngineEvent, EngineAcceptance> for EngineMutation {
    /// Applies the command, then refuses it if it brought a closed input to
    /// Preview or air (IN-026, see [`crate::input_lifecycle::ClosedInputGuard`]).
    fn apply(
        self,
        state: &mut ShowState,
        events: &mut Vec<EngineEvent>,
    ) -> Result<EngineAcceptance, Rejection> {
        let guard = crate::input_lifecycle::ClosedInputGuard::before(
            state,
            matches!(&self.command, EngineCommand::InputLifecycle(_)),
        );
        let acceptance = self.apply_command(state, events)?;
        guard.check(state)?;
        Ok(acceptance)
    }
}

impl EngineMutation {
    #[allow(clippy::too_many_lines)]
    fn apply_command(
        self,
        state: &mut ShowState,
        events: &mut Vec<EngineEvent>,
    ) -> Result<EngineAcceptance, Rejection> {
        if matches!(
            &self.command,
            EngineCommand::Undo { .. } | EngineCommand::Redo { .. }
        ) {
            return Err(Rejection::new(
                RejectionCode::Internal,
                "undo and redo must be resolved before mutation",
            ));
        }
        if let EngineCommand::SetEditLock { scope, locked } = self.command {
            return apply_edit_lock_mutation(state, events, scope, locked, self.target_frame);
        }
        check_edit_locks(state, &self.command).map_err(crate::EditRejection::into_rejection)?;
        if is_playlist_command(&self.command)
            || matches!(&self.command, EngineCommand::RealizePlaylistCue(_))
        {
            return apply_playlist_mutation(state, events, self);
        }
        if let EngineCommand::SetCueList {
            list,
            definition,
            position,
        } = &self.command
        {
            crate::cue::apply_set(state, *list, definition.as_ref(), *position)?;
            events.push(EngineEvent::CueListChanged {
                list: *list,
                frame: self.target_frame.get(),
                edit: true,
                removed: definition.is_none(),
                fired: Vec::new(),
                title_texts: Vec::new(),
                applied: Vec::new(),
                scripts: Vec::new(),
            });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetScript {
            script,
            definition,
            position,
        } = &self.command
        {
            crate::script::apply_set(state, *script, definition.as_ref(), *position)?;
            events.push(EngineEvent::ScriptChanged { script: *script });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetShortcut {
            shortcut,
            definition,
            position,
        } = &self.command
        {
            crate::shortcut::apply_set(state, *shortcut, definition.as_deref(), *position)?;
            events.push(EngineEvent::ShortcutChanged {
                shortcut: *shortcut,
            });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if matches!(
            &self.command,
            EngineCommand::CueControl { .. }
                | EngineCommand::CueAdvance { .. }
                | EngineCommand::RealizeCueActions(_)
                | EngineCommand::RunScript { .. }
                | EngineCommand::StopScript { .. }
                | EngineCommand::FireShortcut { .. }
        ) {
            let Some(plan) = self.cue else {
                return Err(Rejection::new(
                    RejectionCode::InvalidCommand,
                    "cue actions are realized by the engine and are not commands",
                ));
            };
            let plan = plan?;
            *state = *plan.show;
            events.push(plan.event);
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if matches!(&self.command, EngineCommand::RealizeTitle(_)) {
            return Err(Rejection::new(
                RejectionCode::InvalidCommand,
                "title cues are realized by the engine and are not commands",
            ));
        }
        if is_title_command(&self.command) {
            return apply_title_mutation(state, events, self);
        }
        if let EngineCommand::TitleAnimation(edit) = &self.command {
            crate::title_animation::apply(state, edit, self.title_clip_bound)?;
            events.push(EngineEvent::TitleAnimationChanged { input: edit.input });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::TitleExposure(edit) = &self.command {
            crate::title_animation::apply_exposure(state, edit)?;
            events.push(EngineEvent::TitleExposureChanged { input: edit.input });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SceneAnimation(edit) = &self.command {
            crate::scene_animation::apply(state, edit, self.scene_clip_bound)?;
            events.push(EngineEvent::SceneAnimationChanged { input: edit.input });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SceneLayer(edit) = self.command {
            crate::scene_layer::apply(state, &edit)?;
            events.push(EngineEvent::SceneLayoutChanged(edit));
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::AudioMixer(edit) = self.command {
            crate::audio_mixer::apply(state, &edit)?;
            events.push(EngineEvent::AudioMixerChanged(edit));
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetFadeToBlackOutput { output, affected } = self.command {
            if !state
                .outputs()
                .iter()
                .any(|(candidate, _)| *candidate == output)
            {
                return Err(Rejection::new(
                    RejectionCode::NotFound,
                    format!("show does not contain output {output}"),
                ));
            }
            let _ = state
                .desired_switcher_mut()
                .set_fade_to_black_output(output, affected);
            events.push(EngineEvent::FadeToBlackOutputChanged { output, affected });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetFadeToBlackOutputDuration {
            output,
            duration_frames,
        } = self.command
        {
            if !state
                .outputs()
                .iter()
                .any(|(candidate, _)| *candidate == output)
            {
                return Err(Rejection::new(
                    RejectionCode::NotFound,
                    format!("show does not contain output {output}"),
                ));
            }
            if let Some(duration) = duration_frames {
                validate_fade_to_black_duration(duration)?;
            }
            state
                .desired_switcher_mut()
                .set_fade_to_black_output_duration(output, duration_frames)
                .expect("validated FTB output duration");
            events.push(EngineEvent::FadeToBlackOutputDurationChanged {
                output,
                duration_frames,
            });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetFadeToBlackCurve { curve } = self.command {
            let _ = state.desired_switcher_mut().set_fade_to_black_curve(curve);
            events.push(EngineEvent::FadeToBlackCurveChanged { curve });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetDisplayOutput { slot, config } = self.command {
            state
                .set_display_output(slot, config.as_deref().cloned())
                .map_err(|error| match error {
                    crate::ShowError::InvalidDisplayOutput(_, code @ "unknown_output") => {
                        let rejection =
                            Rejection::new(RejectionCode::NotFound, format!("{error} ({code})"));
                        match config.as_deref().map(|config| config.source) {
                            Some(fm_types::display::DisplayOutputSource::Output(output)) => {
                                rejection.with_reason(RefusalReason::UnknownId {
                                    kind: fm_command::IdKind::Output,
                                    id: output.get().get(),
                                })
                            }
                            _ => rejection,
                        }
                    }
                    crate::ShowError::InvalidDisplayOutput(_, code @ "display_in_use") => {
                        // Two slots: the display is claimed by the other.
                        let other = fm_types::display::DisplayOutputSlot::ALL
                            .into_iter()
                            .find(|candidate| *candidate != slot)
                            .unwrap_or(slot);
                        Rejection::new(RejectionCode::Conflict, format!("{error} ({code})"))
                            .with_reason(RefusalReason::DisplayInUse { slot: other.get() })
                    }
                    error => Rejection::new(RejectionCode::InvalidCommand, error.to_string()),
                })?;
            events.push(EngineEvent::DisplayOutputChanged { slot, config });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetAlertRule(setting) = self.command {
            state.set_alert_rule(setting).map_err(|error| {
                Rejection::new(RejectionCode::InvalidCommand, error.to_string()).with_field(
                    fm_command::FieldIssue::new(
                        "alert_rule",
                        "invalid_alert_rule",
                        error.to_string(),
                    ),
                )
            })?;
            events.push(EngineEvent::AlertRulesChanged(state.alert_rules().to_vec()));
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetLiveLan(config) = self.command {
            state
                .set_live_lan((*config).clone())
                .map_err(|error| match error {
                    fm_types::live_lan::LiveLanConfigError::UnknownOutput(output) => {
                        Rejection::new(
                            RejectionCode::NotFound,
                            format!("{error} ({})", error.code()),
                        )
                        .with_reason(RefusalReason::UnknownId {
                            kind: fm_command::IdKind::Output,
                            id: output.get().get(),
                        })
                    }
                    fm_types::live_lan::LiveLanConfigError::MaxViewers => {
                        Rejection::new(RejectionCode::InvalidCommand, error.to_string()).with_field(
                            fm_command::FieldIssue::new(
                                "max_viewers",
                                error.code(),
                                error.to_string(),
                            ),
                        )
                    }
                })?;
            events.push(EngineEvent::LiveLanChanged(config));
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetNetworkStream { input, config } = self.command {
            if state.network_stream(input).is_none() {
                return Err(if state.inputs().contains(&input) {
                    Rejection::new(
                        RejectionCode::InvalidCommand,
                        format!("input {input} is not a network stream"),
                    )
                    .with_field(FieldIssue::new(
                        "input",
                        "not_network_stream",
                        "the input is not a network stream",
                    ))
                } else {
                    Rejection::new(
                        RejectionCode::NotFound,
                        format!("show does not contain input {input}"),
                    )
                    .with_reason(crate::error::unknown_input_reason(input))
                });
            }
            config.validate().map_err(network_stream_rejection)?;
            check_listen_port(state, input, &config)?;
            state.set_input_source_unchecked(
                input,
                fm_model::InputSourceSpec::Network((*config).clone()),
            );
            events.push(EngineEvent::NetworkStreamChanged { input, config });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetCaptureSettings { input, settings } = self.command {
            if state.capture_settings(input).is_none() {
                return Err(if state.inputs().contains(&input) {
                    Rejection::new(
                        RejectionCode::InvalidCommand,
                        format!("input {input} is not a capture input"),
                    )
                    .with_field(FieldIssue::new(
                        "input",
                        "not_capture_input",
                        "the input is not a capture input",
                    ))
                } else {
                    Rejection::new(
                        RejectionCode::NotFound,
                        format!("show does not contain input {input}"),
                    )
                    .with_reason(crate::error::unknown_input_reason(input))
                });
            }
            settings.validate().map_err(capture_rejection)?;
            state.set_capture_settings_unchecked(input, (*settings).clone());
            events.push(EngineEvent::CaptureSettingsChanged { input, settings });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::AnimationPreset(edit) = self.command {
            crate::animation_preset::apply(state.animation_presets_mut(), &edit)?;
            events.push(EngineEvent::AnimationPresetsChanged(edit));
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::InputLifecycle(edit) = &self.command {
            events.push(crate::input_lifecycle::apply(state, edit)?);
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::Replay(operation) = &self.command {
            crate::replay::accept(state, operation)?;
            events.push(EngineEvent::ReplayChanged {
                frame: self.target_frame.get(),
                operation: operation.clone(),
            });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::Reel(operation) = &self.command {
            let plan = self.reel.as_ref().ok_or_else(|| {
                Rejection::new(
                    RejectionCode::Internal,
                    "a reel command was not planned before mutation",
                )
            })?;
            plan.as_ref().map_err(Clone::clone)?;
            crate::reel::accept(state, operation)?;
            // Export control is runtime state, reported by `reel_status`;
            // a reel edit is a durable project edit with an event.
            if !operation.is_live() {
                events.push(EngineEvent::ReelChanged {
                    operation: operation.clone(),
                });
            }
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::Media(command) = &self.command {
            let settings = crate::media::accept(state, command, self.media_tracks.as_deref())?;
            state.set_media_settings(command.input, settings);
            if matches!(command.operation, crate::EngineMediaOperation::Trim { .. }) {
                crate::media_list::sync_marks(state, command.input);
            }
            events.push(EngineEvent::MediaChanged {
                input: command.input,
                frame: self.target_frame.get(),
                operation: command.operation,
            });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::PageNavigate { input, navigation } = self.command {
            // A document's page is show state (IN-014); a slideshow's slide
            // is runtime, stepped on the target frame (IN-009).
            if let Some(page) = crate::paged::accept(state, input, navigation, self.paged_count) {
                state.set_paged_page_unchecked(input, page?);
                events.push(EngineEvent::PagedChanged { input, navigation });
            } else {
                crate::slideshow::paged_config(state, input)?;
                events.push(EngineEvent::PageNavigated {
                    input,
                    frame: self.target_frame.get(),
                    navigation,
                });
            }
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::MediaList(command) = &self.command {
            let plan = self.media_list.clone().ok_or_else(|| {
                Rejection::new(RejectionCode::Internal, "a media list step was not planned")
            })??;
            if let crate::EngineMediaListOperation::Create { name, .. } = &command.operation {
                let item = plan.list.active_item().expect("a new list has its item");
                let import = crate::EngineInputLifecycleEdit::Import {
                    items: vec![fm_model::BatchImportRequest {
                        input: command.input,
                        name: Some(name.clone()),
                        asset_uri: item.asset_uri.clone(),
                    }],
                };
                // One command, one durable event: the list's event names
                // the input it appended.
                crate::input_lifecycle::apply(state, &import)?;
            }
            crate::media_list::apply(state, &plan);
            events.push(EngineEvent::MediaListChanged {
                input: command.input,
                frame: self.target_frame.get(),
                operation: command.operation.clone(),
            });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetTransitionFavorite { slot, favorite } = self.command {
            state.set_transition_favorite(slot, favorite)?;
            events.push(EngineEvent::TransitionFavoriteChanged { slot });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::Grade(edit) = self.command {
            crate::grade_edit::apply_show_grade_edit(state, &edit)?;
            events.push(EngineEvent::GradingChanged(edit));
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if matches!(
            &self.command,
            EngineCommand::ConfigureStinger { .. } | EngineCommand::RemoveStinger { .. }
        ) {
            return apply_stinger_mutation(state, events, self);
        }
        if matches!(&self.command, EngineCommand::RenameInput { .. }) {
            let EngineCommand::RenameInput { input, name } = self.command else {
                unreachable!("only input renames enter the rename mutation")
            };
            state
                .rename_input(input, name.clone())
                .map_err(rename_input_rejection)?;
            events.push(EngineEvent::InputRenamed { input, name });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if matches!(
            &self.command,
            EngineCommand::AddInput { .. } | EngineCommand::RemoveInput { .. }
        ) {
            return apply_input_catalogue_mutation(state, events, self);
        }
        if let EngineCommand::ReorderInputs { inputs } = &self.command {
            state
                .reorder_inputs(inputs.clone())
                .map_err(reorder_inputs_rejection)?;
            events.push(EngineEvent::InputOrderChanged {
                inputs: inputs.clone(),
            });
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if let EngineCommand::SetInputAudioStrip {
            input,
            state: strip_state,
        } = &self.command
        {
            if !(MIN_INPUT_AUDIO_GAIN_MILLIDB..=MAX_INPUT_AUDIO_GAIN_MILLIDB)
                .contains(&strip_state.gain_millidb)
            {
                return Err(Rejection::new(
                    RejectionCode::InvalidCommand,
                    format!(
                        "input audio gain must be between {MIN_INPUT_AUDIO_GAIN_MILLIDB} and \
                         {MAX_INPUT_AUDIO_GAIN_MILLIDB} millidB"
                    ),
                ));
            }
            if !(MIN_INPUT_AUDIO_BALANCE_BASIS_POINTS..=MAX_INPUT_AUDIO_BALANCE_BASIS_POINTS)
                .contains(&strip_state.balance_basis_points)
            {
                return Err(Rejection::new(
                    RejectionCode::InvalidCommand,
                    format!(
                        "input audio balance must be between \
                         {MIN_INPUT_AUDIO_BALANCE_BASIS_POINTS} and \
                         {MAX_INPUT_AUDIO_BALANCE_BASIS_POINTS} basis points"
                    ),
                ));
            }
            if !(MIN_INPUT_AUDIO_BALANCE_BASIS_POINTS..=MAX_INPUT_AUDIO_BALANCE_BASIS_POINTS)
                .contains(&strip_state.pan_basis_points)
            {
                return Err(Rejection::new(
                    RejectionCode::InvalidCommand,
                    format!(
                        "input audio pan must be between \
                         {MIN_INPUT_AUDIO_BALANCE_BASIS_POINTS} and \
                         {MAX_INPUT_AUDIO_BALANCE_BASIS_POINTS} basis points"
                    ),
                ));
            }
            if strip_state.delay_samples > MAX_INPUT_AUDIO_DELAY_SAMPLES {
                return Err(Rejection::new(
                    RejectionCode::InvalidCommand,
                    format!(
                        "input audio delay must not exceed {MAX_INPUT_AUDIO_DELAY_SAMPLES} samples"
                    ),
                ));
            }
            state
                .set_input_audio_strip(*input, *strip_state)
                .map_err(|_| {
                    Rejection::new(
                        RejectionCode::InvalidCommand,
                        format!("input {input} is not part of this show"),
                    )
                    .with_reason(crate::error::unknown_input_reason(*input))
                })?;
            events.push(EngineEvent::DesiredSwitcherChanged(self.command));
            return Ok(EngineAcceptance {
                target_frame: self.target_frame,
            });
        }
        if matches!(
            &self.command,
            EngineCommand::StreamStart { .. }
                | EngineCommand::StreamStop { .. }
                | EngineCommand::StreamControl(_)
                | EngineCommand::SetStreamTarget { .. }
        ) {
            return apply_stream_mutation(state, events, self);
        }
        if matches!(
            &self.command,
            EngineCommand::RecordStart { .. } | EngineCommand::RecordStop { .. }
        ) {
            return apply_recording_mutation(state, events, self);
        }
        if matches!(
            &self.command,
            EngineCommand::TakeOverlay { .. }
                | EngineCommand::UpdateOverlay { .. }
                | EngineCommand::OverlayOff { .. }
                | EngineCommand::ConfigureOverlayTransition { .. }
                | EngineCommand::ConfigureOverlayAppearance { .. }
                | EngineCommand::QueueOverlay { .. }
                | EngineCommand::TakeNextOverlay { .. }
                | EngineCommand::SetOverlayOutputInclusion { .. }
        ) {
            return apply_overlay_mutation(state, events, self);
        }
        if matches!(&self.command, EngineCommand::FadeToBlack { .. }) {
            return apply_fade_to_black_mutation(state, events, self);
        }
        let switcher_command = match &self.command {
            EngineCommand::RenameInput { .. } => {
                unreachable!("input renames return before switcher command mapping")
            }
            EngineCommand::ReorderInputs { .. } => {
                unreachable!("input reorders return before switcher command mapping")
            }
            EngineCommand::AddInput { .. } | EngineCommand::RemoveInput { .. } => {
                unreachable!("input catalogue mutations return before switcher command mapping")
            }
            EngineCommand::SetInputAudioStrip { .. } => {
                unreachable!("audio-strip mutations return before switcher command mapping")
            }
            EngineCommand::SelectPreview(input) => SwitcherCommand::SelectPreview(*input),
            EngineCommand::Cut => SwitcherCommand::Cut,
            EngineCommand::Fade { duration_frames } => {
                validate_transition_duration("fade", *duration_frames)?;
                SwitcherCommand::Cut
            }
            EngineCommand::AlphaFade { duration_frames } => {
                validate_transition_duration("alpha fade", *duration_frames)?;
                SwitcherCommand::Cut
            }
            EngineCommand::Slide { duration_frames } => {
                validate_transition_duration("slide", *duration_frames)?;
                SwitcherCommand::Cut
            }
            EngineCommand::Zoom { duration_frames } => {
                validate_transition_duration("zoom", *duration_frames)?;
                SwitcherCommand::Cut
            }
            EngineCommand::Transition {
                kind,
                duration_frames,
            } => {
                if matches!(kind, TransitionKind::Stinger(_)) {
                    return Err(Rejection::new(
                        RejectionCode::InvalidCommand,
                        "a Stinger is taken with the stinger command",
                    ));
                }
                validate_transition_duration(transition_kind_name(*kind), *duration_frames)?;
                SwitcherCommand::Cut
            }
            EngineCommand::Stinger {
                slot,
                duration_frames,
            } => {
                validate_transition_duration("stinger", *duration_frames)?;
                // The runtime switcher refuses every automatic transition,
                // including a KeepProgram fallback, while a manual one is
                // active; accepting it here would fail the scheduled tick.
                if state.desired_switcher().t_bar().is_some() {
                    return Err(switcher_rejection(SwitcherError::TransitionInProgress));
                }
                match state.desired_switcher().stinger_playback_decision(*slot) {
                    fm_switcher::StingerPlaybackDecision::Play => {
                        let descriptor = state
                            .desired_switcher()
                            .stinger(*slot)
                            .descriptor()
                            .expect("playable stinger has a descriptor");
                        if descriptor.cut_point_frames > *duration_frames {
                            return Err(switcher_rejection(
                                SwitcherError::StingerCutPointOutOfRange {
                                    slot: *slot,
                                    cut_point_frames: descriptor.cut_point_frames,
                                    duration_frames: *duration_frames,
                                },
                            ));
                        }
                        SwitcherCommand::Cut
                    }
                    fm_switcher::StingerPlaybackDecision::Fallback(
                        fm_switcher::MissingMediaFallback::KeepProgram,
                    ) => {
                        events.push(EngineEvent::DesiredSwitcherChanged(self.command));
                        return Ok(EngineAcceptance {
                            target_frame: self.target_frame,
                        });
                    }
                    fm_switcher::StingerPlaybackDecision::Fallback(
                        fm_switcher::MissingMediaFallback::Cut
                        | fm_switcher::MissingMediaFallback::Fade,
                    ) => SwitcherCommand::Cut,
                    fm_switcher::StingerPlaybackDecision::Unconfigured => {
                        return Err(switcher_rejection(SwitcherError::UnconfiguredStinger(
                            *slot,
                        )));
                    }
                }
            }
            EngineCommand::ConfigureStinger { .. } | EngineCommand::RemoveStinger { .. } => {
                unreachable!("Stinger mutations return before switcher command mapping")
            }
            EngineCommand::TakeOverlay { .. }
            | EngineCommand::UpdateOverlay { .. }
            | EngineCommand::OverlayOff { .. }
            | EngineCommand::ConfigureOverlayTransition { .. }
            | EngineCommand::ConfigureOverlayAppearance { .. }
            | EngineCommand::QueueOverlay { .. }
            | EngineCommand::TakeNextOverlay { .. }
            | EngineCommand::SetOverlayOutputInclusion { .. } => {
                unreachable!("overlay mutations return before switcher command mapping")
            }
            EngineCommand::Wipe { duration_frames } => {
                validate_transition_duration("wipe", *duration_frames)?;
                SwitcherCommand::Cut
            }
            EngineCommand::FadeToBlack { .. } => {
                unreachable!("Fade-to-Black mutations return before switcher command mapping")
            }
            EngineCommand::StartManualTransition { kind } => manual_start_command(*kind),
            EngineCommand::SetManualTransitionPosition { position } => {
                SwitcherCommand::SetTBarPosition(position.domain())
            }
            EngineCommand::CommitManualTransition => SwitcherCommand::CommitTBar,
            EngineCommand::CancelManualTransition => SwitcherCommand::CancelTBar,
            EngineCommand::StreamStart { .. }
            | EngineCommand::StreamStop { .. }
            | EngineCommand::StreamControl(_)
            | EngineCommand::SetStreamTarget { .. } => {
                unreachable!("stream mutations return before switcher command mapping")
            }
            EngineCommand::RecordStart { .. } | EngineCommand::RecordStop { .. } => {
                unreachable!("recording mutations return before switcher command mapping")
            }
            EngineCommand::Undo { .. }
            | EngineCommand::Redo { .. }
            | EngineCommand::SetEditLock { .. } => {
                unreachable!("history and lock mutations return before switcher command mapping")
            }
            EngineCommand::SetTitleFields { .. }
            | EngineCommand::TakeTitle { .. }
            | EngineCommand::TitleOffAir { .. }
            | EngineCommand::RealizeTitle(_) => {
                unreachable!("title mutations return before switcher command mapping")
            }
            EngineCommand::SetPlaylist { .. }
            | EngineCommand::PlaylistControl(_)
            | EngineCommand::PlaylistAdvance { .. }
            | EngineCommand::RealizePlaylistCue(_) => {
                unreachable!("playlist mutations return before switcher command mapping")
            }
            EngineCommand::SetCueList { .. }
            | EngineCommand::CueControl { .. }
            | EngineCommand::CueAdvance { .. }
            | EngineCommand::RealizeCueActions(_)
            | EngineCommand::SetScript { .. }
            | EngineCommand::RunScript { .. }
            | EngineCommand::StopScript { .. }
            | EngineCommand::SetShortcut { .. }
            | EngineCommand::FireShortcut { .. } => {
                unreachable!("cue mutations return before switcher command mapping")
            }
            EngineCommand::Grade(_)
            | EngineCommand::TitleAnimation(_)
            | EngineCommand::TitleExposure(_)
            | EngineCommand::SceneAnimation(_)
            | EngineCommand::SceneLayer(_)
            | EngineCommand::AudioMixer(_)
            | EngineCommand::SetFadeToBlackOutput { .. }
            | EngineCommand::SetFadeToBlackOutputDuration { .. }
            | EngineCommand::SetFadeToBlackCurve { .. }
            | EngineCommand::SetDisplayOutput { .. }
            | EngineCommand::SetLiveLan(_)
            | EngineCommand::SetAlertRule(_)
            | EngineCommand::Replay(_)
            | EngineCommand::Reel(_)
            | EngineCommand::SetNetworkStream { .. }
            | EngineCommand::SetCaptureSettings { .. }
            | EngineCommand::AnimationPreset(_)
            | EngineCommand::SetTransitionFavorite { .. }
            | EngineCommand::Media(_)
            | EngineCommand::PageNavigate { .. }
            | EngineCommand::MediaList(_)
            | EngineCommand::InputLifecycle(_) => {
                unreachable!("grade and animation mutations return before switcher command mapping")
            }
        };
        state
            .desired_switcher_mut()
            .apply(switcher_command)
            .map_err(switcher_rejection)?;
        events.push(EngineEvent::DesiredSwitcherChanged(self.command));
        Ok(EngineAcceptance {
            target_frame: self.target_frame,
        })
    }
}

fn apply_playlist_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        target_frame,
        playlist,
        ..
    } = mutation;
    let Some(step) = playlist else {
        return Err(Rejection::new(
            RejectionCode::InvalidCommand,
            "playlist cues are realized by the engine and are not commands",
        ));
    };
    let PlaylistStep { playlist, cue } = step?;
    if let Some(cue) = &cue {
        apply_desired_cue(state.desired_switcher_mut(), cue).map_err(switcher_rejection)?;
    }
    state.replace_playlist(playlist);
    events.push(EngineEvent::PlaylistChanged {
        frame: target_frame.get(),
        cue,
    });
    Ok(EngineAcceptance { target_frame })
}

fn apply_input_catalogue_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        command,
        target_frame,
        ..
    } = mutation;
    match command {
        EngineCommand::AddInput {
            input,
            name,
            source,
        } => {
            if let EngineInputSource::NetworkStream(config) = &source {
                config.validate().map_err(network_stream_rejection)?;
                check_listen_port(state, input, config)?;
                check_network_stream_room(state)?;
            }
            if let EngineInputSource::Capture(config) = &source {
                config.validate().map_err(capture_rejection)?;
                check_capture_room(state)?;
            }
            if let EngineInputSource::SystemAudio(config) = &source {
                config.validate().map_err(system_audio_rejection)?;
                check_system_audio_room(state)?;
            }
            if let EngineInputSource::Image(config) = &source {
                config.validate().map_err(still_rejection)?;
            }
            if let EngineInputSource::Slideshow(config) = &source {
                config.validate().map_err(still_rejection)?;
                check_slideshow_room(state)?;
            }
            if let EngineInputSource::ScreenCapture(config) = &source {
                config.validate().map_err(screen_capture_rejection)?;
                check_screen_capture_room(state)?;
            }
            if matches!(source, EngineInputSource::Replay) {
                check_replay_room(state)?;
            }
            state
                .add_input(input, name.clone())
                .map_err(input_catalogue_rejection)?;
            state
                .set_input_source(input, source.spec())
                .map_err(|_| input_catalogue_rejection(InputCatalogueError::UnknownInput(input)))?;
            if let EngineInputSource::Capture(config) = &source {
                state.set_capture_settings_unchecked(input, config.settings.clone());
            }
            events.push(EngineEvent::InputAdded {
                input,
                name,
                source,
            });
        }
        EngineCommand::RemoveInput { input } => {
            state
                .remove_input(input)
                .map_err(input_catalogue_rejection)?;
            events.push(EngineEvent::InputRemoved { input });
        }
        _ => unreachable!("only input catalogue commands are delegated"),
    }
    Ok(EngineAcceptance { target_frame })
}

/// Refuses an unusable capture device key or settings (IN-001).
fn capture_rejection(error: fm_types::capture::CaptureError) -> Rejection {
    let message = error.to_string();
    let (field, code) = match error {
        fm_types::capture::CaptureError::StableKey => ("device", "invalid_device"),
        fm_types::capture::CaptureError::AlsaDevice | fm_types::capture::CaptureError::Token => {
            ("audio", "invalid_audio")
        }
        fm_types::capture::CaptureError::Dimensions
        | fm_types::capture::CaptureError::Rate
        | fm_types::capture::CaptureError::FormatToken => ("format", "invalid_format"),
    };
    Rejection::new(RejectionCode::InvalidCommand, message.clone())
        .with_field(FieldIssue::new(field, code, message))
}

/// Refuses an unusable screen capture target or setting (IN-011).
pub(crate) fn screen_capture_rejection(
    error: fm_types::screen_capture::ScreenCaptureError,
) -> Rejection {
    let message = error.to_string();
    Rejection::new(RejectionCode::InvalidCommand, message.clone()).with_field(FieldIssue::new(
        error.field(),
        error.code(),
        message,
    ))
}

/// Refuses another screen capture input once the show holds
/// [`fm_types::screen_capture::MAX_SCREEN_CAPTURE_INPUTS`] (IN-011).
pub(crate) fn check_screen_capture_room(state: &ShowState) -> Result<(), Rejection> {
    let count = state.screen_capture_inputs().count();
    if count >= fm_types::screen_capture::MAX_SCREEN_CAPTURE_INPUTS {
        let message =
            format!("the show already has {count} screen capture inputs, the most it may hold");
        return Err(
            Rejection::new(RejectionCode::Conflict, message.clone()).with_field(FieldIssue::new(
                "kind",
                "too_many_screen_capture_inputs",
                message,
            )),
        );
    }
    Ok(())
}

/// Refuses one more Replay input once the show holds the most it may
/// (OR-012).
pub(crate) fn check_replay_room(state: &ShowState) -> Result<(), Rejection> {
    let count = state.replay_inputs().count();
    if count >= fm_types::replay::MAX_REPLAY_INPUTS {
        let message = format!("the show already has {count} replay inputs, the most it may hold");
        return Err(Rejection::new(RejectionCode::Conflict, message.clone())
            .with_field(FieldIssue::new("kind", "too_many_replay_inputs", message)));
    }
    Ok(())
}

/// Refuses an unusable system audio device name (AU-011).
pub(crate) fn system_audio_rejection(error: fm_types::system_audio::SystemAudioError) -> Rejection {
    let message = error.to_string();
    Rejection::new(RejectionCode::InvalidCommand, message.clone()).with_field(FieldIssue::new(
        error.field(),
        error.code(),
        message,
    ))
}

/// The refusal of unusable image or slideshow settings (IN-008, IN-009).
pub(crate) fn still_rejection(error: fm_types::still::StillInputError) -> Rejection {
    let message = error.to_string();
    Rejection::new(RejectionCode::InvalidCommand, message.clone()).with_field(FieldIssue::new(
        error.field(),
        error.code(),
        message,
    ))
}

/// Refuses another slideshow input once the show holds
/// [`fm_types::still::MAX_SLIDESHOW_INPUTS`] (IN-009).
pub(crate) fn check_slideshow_room(state: &ShowState) -> Result<(), Rejection> {
    let count = state
        .input_sources()
        .values()
        .filter(|source| matches!(source, fm_model::InputSourceSpec::Slideshow(_)))
        .count();
    if count >= fm_types::still::MAX_SLIDESHOW_INPUTS {
        let message =
            format!("the show already has {count} slideshow inputs, the most it may hold");
        return Err(
            Rejection::new(RejectionCode::Conflict, message.clone()).with_field(FieldIssue::new(
                "kind",
                "too_many_slideshow_inputs",
                message,
            )),
        );
    }
    Ok(())
}

/// Refuses another system audio input once the show holds
/// [`fm_types::system_audio::MAX_SYSTEM_AUDIO_INPUTS`] (AU-011).
pub(crate) fn check_system_audio_room(state: &ShowState) -> Result<(), Rejection> {
    let count = state.system_audio_inputs().count();
    if count >= fm_types::system_audio::MAX_SYSTEM_AUDIO_INPUTS {
        let message =
            format!("the show already has {count} system audio inputs, the most it may hold");
        return Err(
            Rejection::new(RejectionCode::Conflict, message.clone()).with_field(FieldIssue::new(
                "kind",
                "too_many_system_audio_inputs",
                message,
            )),
        );
    }
    Ok(())
}

/// Refuses another document input once the show holds
/// [`fm_types::paged::MAX_DOCUMENT_INPUTS`] (IN-014).
pub(crate) fn check_document_room(state: &ShowState) -> Result<(), Rejection> {
    let count = state.paged_inputs().len();
    if count >= fm_types::paged::MAX_DOCUMENT_INPUTS {
        let message = format!("the show already has {count} document inputs, the most it may hold");
        return Err(Rejection::new(RejectionCode::Conflict, message.clone())
            .with_field(FieldIssue::new("kind", "too_many_document_inputs", message)));
    }
    Ok(())
}

/// Refuses another capture input once the show holds
/// [`fm_types::capture::MAX_CAPTURE_INPUTS`] (IN-001).
pub(crate) fn check_capture_room(state: &ShowState) -> Result<(), Rejection> {
    let count = state.capture_inputs().count();
    if count >= fm_types::capture::MAX_CAPTURE_INPUTS {
        let message = format!("the show already has {count} capture inputs, the most it may hold");
        return Err(Rejection::new(RejectionCode::Conflict, message.clone())
            .with_field(FieldIssue::new("kind", "too_many_capture_inputs", message)));
    }
    Ok(())
}

/// Refuses a network stream that would listen on a local port another
/// network stream input (other than `input`) already listens on (IN-016,
/// IN-017).
/// Refuses another network stream once the show holds
/// [`fm_types::network_stream::MAX_NETWORK_STREAM_INPUTS`] (IN-015).
pub(crate) fn check_network_stream_room(state: &ShowState) -> Result<(), Rejection> {
    let count = state.network_streams().count();
    if count >= fm_types::network_stream::MAX_NETWORK_STREAM_INPUTS {
        let message = format!("the show already has {count} network streams, the most it may hold");
        return Err(Rejection::new(RejectionCode::Conflict, message.clone())
            .with_field(FieldIssue::new("kind", "too_many_network_streams", message)));
    }
    Ok(())
}

pub(crate) fn check_listen_port(
    state: &ShowState,
    input: InputId,
    config: &fm_types::network_stream::NetworkStreamConfig,
) -> Result<(), Rejection> {
    let Some(endpoint) = config.listen_endpoint() else {
        return Ok(());
    };
    if let Some((holder, _)) = state.network_streams().find(|(other, other_config)| {
        *other != input && other_config.listen_endpoint() == Some(endpoint)
    }) {
        let message = format!("input {holder} already listens on {endpoint}");
        return Err(Rejection::new(RejectionCode::Conflict, message.clone())
            .with_field(FieldIssue::new("url", "listen_port_in_use", message)));
    }
    Ok(())
}

/// Refuses an unusable network stream configuration (IN-015), naming its
/// field and error code.
fn network_stream_rejection(error: fm_types::network_stream::NetworkStreamError) -> Rejection {
    let message = error.to_string();
    Rejection::new(RejectionCode::InvalidCommand, message.clone()).with_field(FieldIssue::new(
        error.field(),
        error.code(),
        message,
    ))
}

fn input_catalogue_rejection(error: InputCatalogueError) -> Rejection {
    let (code, field, issue) = match error {
        InputCatalogueError::DuplicateInput(_) => {
            (RejectionCode::Conflict, "input", "duplicate_input")
        }
        InputCatalogueError::EmptyName | InputCatalogueError::NameTooLong => {
            (RejectionCode::InvalidCommand, "name", "invalid_name")
        }
        InputCatalogueError::DuplicateName => (RejectionCode::Conflict, "name", "duplicate_name"),
        InputCatalogueError::TooManyInputs { .. } => {
            (RejectionCode::Conflict, "input", "too_many_inputs")
        }
        InputCatalogueError::UnknownInput(_) => (RejectionCode::NotFound, "input", "unknown_input"),
        InputCatalogueError::InUse { .. }
        | InputCatalogueError::InPlaylist { .. }
        | InputCatalogueError::InCueList { .. }
        | InputCatalogueError::BoundByCueRun { .. }
        | InputCatalogueError::ReplaySource(_)
        | InputCatalogueError::ReelClip { .. } => {
            (RejectionCode::Conflict, "input", "input_in_use")
        }
    };
    let message = error.to_string();
    Rejection::new(code, message.clone())
        .with_field(FieldIssue::new(field, issue, message))
        .with_reason(error.reason())
}

fn apply_edit_lock_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    scope: EditLockScope,
    locked: bool,
    target_frame: FrameNumber,
) -> Result<EngineAcceptance, Rejection> {
    match scope {
        EditLockScope::Project => state.set_project_locked(locked),
        EditLockScope::Input(input) => state.set_input_locked(input, locked).map_err(|_| {
            Rejection::new(
                RejectionCode::NotFound,
                format!("show does not contain input {input}"),
            )
            .with_reason(crate::error::unknown_input_reason(input))
        })?,
    }
    // Exactly one durable event per acceptance keeps revision, event
    // sequence, and runtime generation in lockstep for restore validation.
    events.push(EngineEvent::EditLockChanged { scope, locked });
    Ok(EngineAcceptance { target_frame })
}

fn apply_stream_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        command,
        target_frame,
        ..
    } = mutation;
    match command {
        EngineCommand::StreamStart { target } | EngineCommand::StreamStop { target } => {
            let running = matches!(command, EngineCommand::StreamStart { .. });
            state
                .desired_switcher_mut()
                .set_stream_running(target, running)
                .map_err(switcher_rejection)?;
        }
        EngineCommand::StreamControl(EngineStreamControl::StartAll) => {
            let _ = state.desired_switcher_mut().set_all_streams_running(true);
        }
        EngineCommand::StreamControl(EngineStreamControl::StopAll) => {
            let _ = state.desired_switcher_mut().set_all_streams_running(false);
        }
        EngineCommand::StreamControl(EngineStreamControl::Retry { target }) => {
            match state.stream_running(target) {
                None => {
                    return Err(switcher_rejection(SwitcherError::UnknownStreamTarget(
                        target,
                    )));
                }
                Some(false) => {
                    return Err(Rejection::new(
                        RejectionCode::Conflict,
                        format!("stream target {target} is not running; start it instead"),
                    )
                    .with_field(FieldIssue::new(
                        "target",
                        "stream_not_running",
                        "only a running destination can be retried",
                    )));
                }
                Some(true) => {}
            }
        }
        EngineCommand::SetStreamTarget {
            target,
            spec,
            position,
        } => {
            let config = crate::stream_target::apply(state, target, spec.as_deref(), position)?;
            events.push(EngineEvent::StreamTargetChanged {
                target,
                config: config.map(Box::new),
                streams: desired_stream_statuses(state),
            });
            return Ok(EngineAcceptance { target_frame });
        }
        _ => unreachable!("only stream mutations are delegated"),
    }
    events.push(EngineEvent::StreamsChanged {
        streams: desired_stream_statuses(state),
    });
    Ok(EngineAcceptance { target_frame })
}

fn apply_recording_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        target_frame,
        command,
        ..
    } = mutation;
    let (recorder, active) = match &command {
        EngineCommand::RecordStart { recorder } => (*recorder, true),
        EngineCommand::RecordStop { recorder } => (*recorder, false),
        _ => unreachable!("only recording commands are delegated"),
    };
    state
        .desired_switcher_mut()
        .set_recording_desired(recorder, active)
        .map_err(switcher_rejection)?;
    let resulting = state.desired_switcher().recording_desired(recorder);
    // Every acceptance carries one durable event, exactly like the stream
    // roster events: the snapshot's revision, event sequence, and runtime
    // generation advance together per accepted command, so a command that
    // leaves the addressed slot unchanged (an operator stopping a stopped
    // recorder) must still emit it or the persisted counters diverge and the
    // project refuses to restore.
    events.push(EngineEvent::RecordingChanged {
        recorder,
        active: resulting,
    });
    Ok(EngineAcceptance { target_frame })
}

fn desired_stream_statuses(state: &ShowState) -> Vec<EngineStreamStatus> {
    state
        .streams()
        .iter()
        .map(|stream| EngineStreamStatus {
            target: stream.id(),
            name: stream.name().to_owned(),
            desired_running: state
                .stream_running(stream.id())
                .expect("inventoried stream has a desired flag"),
        })
        .collect()
}

fn apply_fade_to_black_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        command,
        target_frame,
        ..
    } = mutation;
    let EngineCommand::FadeToBlack {
        active,
        duration_frames,
    } = &command
    else {
        unreachable!("only Fade-to-Black mutations are delegated")
    };
    validate_fade_to_black_duration(*duration_frames)?;
    state
        .desired_switcher_mut()
        .restore_settled_fade_to_black(*active);
    events.push(EngineEvent::DesiredSwitcherChanged(command));
    Ok(EngineAcceptance { target_frame })
}

fn apply_overlay_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        command,
        target_frame,
        ..
    } = mutation;
    if let EngineCommand::SetOverlayOutputInclusion { output, .. } = &command
        && !state
            .outputs()
            .iter()
            .any(|(candidate, _)| candidate == output)
    {
        return Err(Rejection::new(
            RejectionCode::NotFound,
            format!("show does not contain output {output}"),
        ));
    }
    let switcher_command = match &command {
        EngineCommand::TakeOverlay { channel, source } => SwitcherCommand::TakeOverlay {
            channel: *channel,
            source: *source,
        },
        EngineCommand::UpdateOverlay { channel, source } => SwitcherCommand::UpdateOverlay {
            channel: *channel,
            source: *source,
        },
        EngineCommand::OverlayOff { channel } => SwitcherCommand::OverlayOff(*channel),
        EngineCommand::ConfigureOverlayTransition {
            channel,
            transition,
            duration_frames,
        } => SwitcherCommand::ConfigureOverlayTransition {
            channel: *channel,
            transition: *transition,
            duration_frames: *duration_frames,
        },
        EngineCommand::ConfigureOverlayAppearance {
            channel,
            position,
            border,
        } => SwitcherCommand::ConfigureOverlayAppearance {
            channel: *channel,
            position: *position,
            border: *border,
        },
        EngineCommand::QueueOverlay { channel, source } => SwitcherCommand::QueueOverlay {
            channel: *channel,
            source: *source,
        },
        EngineCommand::TakeNextOverlay { channel } => SwitcherCommand::TakeNextOverlay(*channel),
        EngineCommand::SetOverlayOutputInclusion {
            channel,
            output,
            included,
        } => SwitcherCommand::SetOverlayOutputInclusion {
            channel: *channel,
            output: *output,
            included: *included,
        },
        _ => unreachable!("only overlay mutation commands are delegated"),
    };
    state
        .desired_switcher_mut()
        .apply(switcher_command)
        .map_err(switcher_rejection)?;
    events.push(EngineEvent::DesiredSwitcherChanged(command));
    Ok(EngineAcceptance { target_frame })
}

fn apply_stinger_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        command,
        target_frame,
        ..
    } = mutation;
    if state.desired_switcher().t_bar().is_some() {
        return Err(Rejection::new(
            RejectionCode::Conflict,
            "Stinger slot configuration cannot change during a manual transition",
        ));
    }
    match &command {
        EngineCommand::ConfigureStinger { slot, descriptor } => {
            state
                .desired_switcher_mut()
                .configure_stinger(*slot, *descriptor)
                .map_err(switcher_rejection)?;
            if descriptor.preload {
                let _ = state
                    .desired_switcher_mut()
                    .preload_stinger(*slot, true)
                    .map_err(switcher_rejection)?;
            }
        }
        EngineCommand::RemoveStinger { slot } => state.remove_stinger(*slot),
        _ => unreachable!("only Stinger mutation commands are delegated"),
    }
    events.push(EngineEvent::DesiredSwitcherChanged(command));
    Ok(EngineAcceptance { target_frame })
}

#[allow(clippy::too_many_lines)]
fn apply_runtime(
    switcher: &mut SwitcherState,
    command: EngineCommand,
) -> Result<Vec<SwitcherEvent>, SwitcherError> {
    if matches!(
        &command,
        EngineCommand::RenameInput { .. }
            | EngineCommand::SetInputAudioStrip { .. }
            | EngineCommand::SetCueList { .. }
            | EngineCommand::SetScript { .. }
            | EngineCommand::SetShortcut { .. }
            | EngineCommand::SetEditLock { .. }
            | EngineCommand::SetTitleFields { .. }
            | EngineCommand::Grade(_)
            | EngineCommand::TitleAnimation(_)
            | EngineCommand::TitleExposure(_)
            | EngineCommand::SceneAnimation(_)
            | EngineCommand::SceneLayer(_)
            | EngineCommand::AudioMixer(_)
            | EngineCommand::SetDisplayOutput { .. }
            | EngineCommand::SetLiveLan(_)
            | EngineCommand::SetAlertRule(_)
            | EngineCommand::Replay(_)
            | EngineCommand::Reel(_)
            | EngineCommand::SetNetworkStream { .. }
            | EngineCommand::SetCaptureSettings { .. }
            | EngineCommand::AnimationPreset(_)
            | EngineCommand::SetTransitionFavorite { .. }
            | EngineCommand::Media(_)
            | EngineCommand::PageNavigate { .. }
            | EngineCommand::MediaList(_)
    ) {
        return Ok(Vec::new());
    }
    if let EngineCommand::SetFadeToBlackOutput { output, affected } = command {
        return switcher.apply(SwitcherCommand::SetFadeToBlackOutput { output, affected });
    }
    if let EngineCommand::SetFadeToBlackOutputDuration {
        output,
        duration_frames,
    } = command
    {
        return switcher.apply(SwitcherCommand::SetFadeToBlackOutputDuration {
            output,
            duration_frames,
        });
    }
    if let EngineCommand::SetFadeToBlackCurve { curve } = command {
        return switcher.apply(SwitcherCommand::SetFadeToBlackCurve(curve));
    }
    if let EngineCommand::InputLifecycle(edit) = &command {
        return realize_input_lifecycle(switcher, edit);
    }
    if let EngineCommand::RealizePlaylistCue(cue) = &command {
        return cue
            .as_ref()
            .map_or_else(|| Ok(Vec::new()), |cue| realize_cue(switcher, cue));
    }
    if let Some(result) = apply_runtime_stream(switcher, &command) {
        return result;
    }
    if let Some(result) = apply_runtime_recording(switcher, &command) {
        return result;
    }
    if let EngineCommand::ReorderInputs { inputs } = command {
        switcher
            .reorder_inputs(inputs)
            .expect("accepted engine input orders are validated");
        return Ok(Vec::new());
    }
    match &command {
        EngineCommand::AddInput { input, .. } => {
            switcher.add_input(*input)?;
            return Ok(Vec::new());
        }
        EngineCommand::RemoveInput { input } => {
            switcher.remove_input(*input)?;
            return Ok(Vec::new());
        }
        _ => {}
    }
    if let EngineCommand::FadeToBlack {
        active,
        duration_frames,
    } = &command
    {
        return Ok(switcher
            .request_fade_to_black(*active, *duration_frames)
            .expect("accepted engine FTB durations are validated"));
    }
    if is_overlay_command(&command) {
        return apply_runtime_overlay(switcher, &command);
    }
    switcher.apply(match command {
        EngineCommand::RenameInput { .. } => {
            unreachable!("input renames return before switcher command mapping")
        }
        EngineCommand::ReorderInputs { .. } => {
            unreachable!("input reorders return before switcher command mapping")
        }
        EngineCommand::AddInput { .. } | EngineCommand::RemoveInput { .. } => {
            unreachable!("input catalogue commands return before switcher command mapping")
        }
        EngineCommand::SetInputAudioStrip { .. } => {
            unreachable!("audio-strip commands return before switcher command mapping")
        }
        EngineCommand::SelectPreview(input) => SwitcherCommand::SelectPreview(input),
        EngineCommand::Cut => SwitcherCommand::Cut,
        EngineCommand::Fade { duration_frames } => SwitcherCommand::Transition {
            kind: TransitionKind::Fade,
            duration_frames,
        },
        EngineCommand::AlphaFade { duration_frames } => SwitcherCommand::Transition {
            kind: TransitionKind::AlphaFade,
            duration_frames,
        },
        EngineCommand::Slide { duration_frames } => SwitcherCommand::Transition {
            kind: TransitionKind::Slide,
            duration_frames,
        },
        EngineCommand::Zoom { duration_frames } => SwitcherCommand::Transition {
            kind: TransitionKind::Zoom,
            duration_frames,
        },
        EngineCommand::Transition {
            kind,
            duration_frames,
        } => SwitcherCommand::Transition {
            kind,
            duration_frames,
        },
        EngineCommand::Stinger {
            slot,
            duration_frames,
        } => SwitcherCommand::Transition {
            kind: TransitionKind::Stinger(slot),
            duration_frames,
        },
        EngineCommand::ConfigureStinger { slot, descriptor } => {
            switcher.configure_stinger(slot, descriptor)?;
            let mut events = Vec::new();
            if descriptor.preload {
                events.push(switcher.preload_stinger(slot, true)?);
            }
            return Ok(events);
        }
        EngineCommand::RemoveStinger { slot } => {
            switcher.remove_stinger(slot);
            return Ok(Vec::new());
        }
        EngineCommand::TakeOverlay { .. }
        | EngineCommand::UpdateOverlay { .. }
        | EngineCommand::OverlayOff { .. }
        | EngineCommand::ConfigureOverlayTransition { .. }
        | EngineCommand::ConfigureOverlayAppearance { .. }
        | EngineCommand::QueueOverlay { .. }
        | EngineCommand::TakeNextOverlay { .. }
        | EngineCommand::SetOverlayOutputInclusion { .. } => {
            unreachable!("overlay commands return before switcher command mapping")
        }
        EngineCommand::Wipe { duration_frames } => SwitcherCommand::Wipe { duration_frames },
        EngineCommand::FadeToBlack { .. } => {
            unreachable!("Fade-to-Black commands return before switcher command mapping")
        }
        EngineCommand::StartManualTransition { kind } => manual_start_command(kind),
        EngineCommand::SetManualTransitionPosition { position } => {
            SwitcherCommand::SetTBarPosition(position.domain())
        }
        EngineCommand::CommitManualTransition => SwitcherCommand::CommitTBar,
        EngineCommand::CancelManualTransition => SwitcherCommand::CancelTBar,
        EngineCommand::StreamStart { .. }
        | EngineCommand::StreamStop { .. }
        | EngineCommand::StreamControl(_)
        | EngineCommand::SetStreamTarget { .. } => {
            unreachable!("stream commands return before switcher command mapping")
        }
        EngineCommand::RecordStart { .. } | EngineCommand::RecordStop { .. } => {
            unreachable!("recording commands return before switcher command mapping")
        }
        EngineCommand::Undo { .. } | EngineCommand::Redo { .. } => {
            unreachable!("undo and redo are resolved to edits before scheduling")
        }
        EngineCommand::SetEditLock { .. } => {
            unreachable!("edit-lock commands return before switcher command mapping")
        }
        EngineCommand::SetTitleFields { .. }
        | EngineCommand::TakeTitle { .. }
        | EngineCommand::TitleOffAir { .. }
        | EngineCommand::RealizeTitle(_) => {
            unreachable!("title commands are realized as title cues or ignored")
        }
        EngineCommand::SetPlaylist { .. }
        | EngineCommand::PlaylistControl(_)
        | EngineCommand::PlaylistAdvance { .. }
        | EngineCommand::RealizePlaylistCue(_) => {
            unreachable!("playlist commands are scheduled as playlist cues")
        }
        EngineCommand::SetCueList { .. }
        | EngineCommand::CueControl { .. }
        | EngineCommand::CueAdvance { .. }
        | EngineCommand::RealizeCueActions(_)
        | EngineCommand::SetScript { .. }
        | EngineCommand::RunScript { .. }
        | EngineCommand::StopScript { .. }
        | EngineCommand::SetShortcut { .. }
        | EngineCommand::FireShortcut { .. } => {
            unreachable!("cue commands are realized by the engine tick")
        }
        EngineCommand::Grade(_)
        | EngineCommand::TitleAnimation(_)
        | EngineCommand::TitleExposure(_)
        | EngineCommand::SceneAnimation(_)
        | EngineCommand::SceneLayer(_)
        | EngineCommand::AudioMixer(_)
        | EngineCommand::SetFadeToBlackOutput { .. }
        | EngineCommand::SetFadeToBlackOutputDuration { .. }
        | EngineCommand::SetFadeToBlackCurve { .. }
        | EngineCommand::SetDisplayOutput { .. }
        | EngineCommand::SetLiveLan(_)
        | EngineCommand::SetAlertRule(_)
        | EngineCommand::Replay(_)
        | EngineCommand::Reel(_)
        | EngineCommand::SetNetworkStream { .. }
        | EngineCommand::SetCaptureSettings { .. }
        | EngineCommand::AnimationPreset(_)
        | EngineCommand::SetTransitionFavorite { .. }
        | EngineCommand::Media(_)
        | EngineCommand::PageNavigate { .. }
        | EngineCommand::MediaList(_)
        | EngineCommand::InputLifecycle(_) => {
            unreachable!("grade and animation commands return before switcher command mapping")
        }
    })
}

/// Realizes an input lifecycle edit on the realized switcher: the inputs a
/// duplicate or import appends (the import already filtered to its
/// accepted items) and the inputs a batch removal removes.
fn realize_input_lifecycle(
    switcher: &mut SwitcherState,
    edit: &crate::EngineInputLifecycleEdit,
) -> Result<Vec<SwitcherEvent>, SwitcherError> {
    match edit {
        crate::EngineInputLifecycleEdit::Duplicate { input, .. } => switcher.add_input(*input)?,
        crate::EngineInputLifecycleEdit::Import { items } => {
            for item in items {
                switcher.add_input(item.input)?;
            }
        }
        crate::EngineInputLifecycleEdit::RemoveInputs { inputs } => {
            for input in inputs {
                switcher.remove_input(*input)?;
            }
        }
        crate::EngineInputLifecycleEdit::ReplaceSource { .. }
        | crate::EngineInputLifecycleEdit::SetCategory { .. }
        | crate::EngineInputLifecycleEdit::SetColor { .. }
        | crate::EngineInputLifecycleEdit::SetPaused { .. }
        | crate::EngineInputLifecycleEdit::SetClosed { .. }
        | crate::EngineInputLifecycleEdit::SetCategories { .. } => {}
    }
    Ok(Vec::new())
}

fn apply_runtime_stream(
    switcher: &mut SwitcherState,
    command: &EngineCommand,
) -> Option<Result<Vec<SwitcherEvent>, SwitcherError>> {
    match command {
        EngineCommand::StreamStart { target } => Some(switcher.set_stream_running(*target, true)),
        EngineCommand::StreamStop { target } => Some(switcher.set_stream_running(*target, false)),
        EngineCommand::StreamControl(EngineStreamControl::StartAll) => {
            Some(Ok(switcher.set_all_streams_running(true)))
        }
        EngineCommand::StreamControl(EngineStreamControl::StopAll) => {
            Some(Ok(switcher.set_all_streams_running(false)))
        }
        EngineCommand::StreamControl(EngineStreamControl::Retry { .. }) => Some(Ok(Vec::new())),
        EngineCommand::SetStreamTarget {
            target,
            spec,
            position,
        } => Some(match spec {
            Some(spec) => switcher
                .upsert_stream_at(*target, spec.name.clone(), *position)
                .map(|()| Vec::new()),

            None => switcher.remove_stream(*target).map(|()| Vec::new()),
        }),
        _ => None,
    }
}

fn apply_runtime_recording(
    switcher: &mut SwitcherState,
    command: &EngineCommand,
) -> Option<Result<Vec<SwitcherEvent>, SwitcherError>> {
    match command {
        EngineCommand::RecordStart { recorder } => {
            Some(switcher.set_recording_desired(*recorder, true))
        }
        EngineCommand::RecordStop { recorder } => {
            Some(switcher.set_recording_desired(*recorder, false))
        }
        _ => None,
    }
}

fn apply_runtime_overlay(
    switcher: &mut SwitcherState,
    command: &EngineCommand,
) -> Result<Vec<SwitcherEvent>, SwitcherError> {
    match command {
        EngineCommand::TakeOverlay { channel, source } => {
            switcher.request_overlay_take(*channel, *source)
        }
        EngineCommand::OverlayOff { channel } => Ok(switcher.request_overlay_off(*channel)),
        EngineCommand::TakeNextOverlay { channel } => switcher.request_overlay_take_next(*channel),
        EngineCommand::UpdateOverlay { channel, source } => {
            switcher.apply(SwitcherCommand::UpdateOverlay {
                channel: *channel,
                source: *source,
            })
        }
        EngineCommand::ConfigureOverlayTransition {
            channel,
            transition,
            duration_frames,
        } => switcher.apply(SwitcherCommand::ConfigureOverlayTransition {
            channel: *channel,
            transition: *transition,
            duration_frames: *duration_frames,
        }),
        EngineCommand::ConfigureOverlayAppearance {
            channel,
            position,
            border,
        } => switcher.apply(SwitcherCommand::ConfigureOverlayAppearance {
            channel: *channel,
            position: *position,
            border: *border,
        }),
        EngineCommand::QueueOverlay { channel, source } => {
            switcher.apply(SwitcherCommand::QueueOverlay {
                channel: *channel,
                source: *source,
            })
        }
        EngineCommand::SetOverlayOutputInclusion {
            channel,
            output,
            included,
        } => switcher.apply(SwitcherCommand::SetOverlayOutputInclusion {
            channel: *channel,
            output: *output,
            included: *included,
        }),
        _ => unreachable!("only overlay commands are delegated"),
    }
}

fn runtime_transition_kind(show: &ShowState, command: &EngineCommand) -> Option<TransitionKind> {
    match command {
        EngineCommand::Fade { .. } => Some(TransitionKind::Fade),
        EngineCommand::AlphaFade { .. } => Some(TransitionKind::AlphaFade),
        EngineCommand::Slide { .. } => Some(TransitionKind::Slide),
        EngineCommand::Zoom { .. } => Some(TransitionKind::Zoom),
        EngineCommand::Transition { kind, .. } => Some(*kind),
        EngineCommand::Wipe { .. } => Some(TransitionKind::Wipe),
        EngineCommand::Stinger { slot, .. } => stinger_runtime_transition(show, *slot),
        EngineCommand::ConfigureStinger { .. }
        | EngineCommand::RemoveStinger { .. }
        | EngineCommand::RenameInput { .. }
        | EngineCommand::ReorderInputs { .. }
        | EngineCommand::AddInput { .. }
        | EngineCommand::RemoveInput { .. }
        | EngineCommand::SetInputAudioStrip { .. }
        | EngineCommand::TakeOverlay { .. }
        | EngineCommand::UpdateOverlay { .. }
        | EngineCommand::OverlayOff { .. }
        | EngineCommand::ConfigureOverlayTransition { .. }
        | EngineCommand::ConfigureOverlayAppearance { .. }
        | EngineCommand::QueueOverlay { .. }
        | EngineCommand::TakeNextOverlay { .. }
        | EngineCommand::SetOverlayOutputInclusion { .. }
        | EngineCommand::SelectPreview(_)
        | EngineCommand::Cut
        | EngineCommand::FadeToBlack { .. }
        | EngineCommand::StartManualTransition { .. }
        | EngineCommand::SetManualTransitionPosition { .. }
        | EngineCommand::CommitManualTransition
        | EngineCommand::CancelManualTransition
        | EngineCommand::StreamStart { .. }
        | EngineCommand::StreamStop { .. }
        | EngineCommand::StreamControl(_)
        | EngineCommand::SetStreamTarget { .. }
        | EngineCommand::RecordStart { .. }
        | EngineCommand::RecordStop { .. }
        | EngineCommand::Undo { .. }
        | EngineCommand::Redo { .. }
        | EngineCommand::SetEditLock { .. }
        | EngineCommand::SetTitleFields { .. }
        | EngineCommand::TakeTitle { .. }
        | EngineCommand::TitleOffAir { .. }
        | EngineCommand::SetPlaylist { .. }
        | EngineCommand::PlaylistControl(_)
        | EngineCommand::PlaylistAdvance { .. }
        | EngineCommand::RealizePlaylistCue(_)
        | EngineCommand::RealizeTitle(_)
        | EngineCommand::Grade(_)
        | EngineCommand::TitleAnimation(_)
        | EngineCommand::TitleExposure(_)
        | EngineCommand::SceneAnimation(_)
        | EngineCommand::SceneLayer(_)
        | EngineCommand::AudioMixer(_)
        | EngineCommand::SetFadeToBlackOutput { .. }
        | EngineCommand::SetFadeToBlackOutputDuration { .. }
        | EngineCommand::SetFadeToBlackCurve { .. }
        | EngineCommand::SetDisplayOutput { .. }
        | EngineCommand::SetLiveLan(_)
        | EngineCommand::SetAlertRule(_)
        | EngineCommand::Replay(_)
        | EngineCommand::Reel(_)
        | EngineCommand::SetNetworkStream { .. }
        | EngineCommand::SetCaptureSettings { .. }
        | EngineCommand::AnimationPreset(_)
        | EngineCommand::SetTransitionFavorite { .. }
        | EngineCommand::Media(_)
        | EngineCommand::PageNavigate { .. }
        | EngineCommand::MediaList(_)
        | EngineCommand::InputLifecycle(_)
        | EngineCommand::SetCueList { .. }
        | EngineCommand::CueControl { .. }
        | EngineCommand::CueAdvance { .. }
        | EngineCommand::RealizeCueActions(_)
        | EngineCommand::SetScript { .. }
        | EngineCommand::RunScript { .. }
        | EngineCommand::StopScript { .. }
        | EngineCommand::SetShortcut { .. }
        | EngineCommand::FireShortcut { .. } => None,
    }
}

/// Whether `command` is refused while a Program transition is in flight:
/// another Program change, a Preview selection, a manual transition, a
/// removal or close of an input, or a strip change.
fn refused_during_transition(command: &EngineCommand) -> bool {
    !matches!(command, EngineCommand::FadeToBlack { .. })
        && !matches!(command, EngineCommand::RenameInput { .. })
        && !matches!(command, EngineCommand::ReorderInputs { .. })
        && !matches!(command, EngineCommand::AddInput { .. })
        && !matches!(command, EngineCommand::SetEditLock { .. })
        && !matches!(command, EngineCommand::Grade(_))
        && !matches!(command, EngineCommand::TitleAnimation(_))
        && !matches!(command, EngineCommand::SceneAnimation(_))
        && !matches!(command, EngineCommand::SceneLayer(_))
        && !matches!(command, EngineCommand::AudioMixer(_))
        && !matches!(command, EngineCommand::SetFadeToBlackOutput { .. })
        && !matches!(command, EngineCommand::SetFadeToBlackOutputDuration { .. })
        && !matches!(command, EngineCommand::SetFadeToBlackCurve { .. })
        && !matches!(command, EngineCommand::SetDisplayOutput { .. })
        && !matches!(command, EngineCommand::SetLiveLan(_))
        && !matches!(command, EngineCommand::SetAlertRule(_))
        && !matches!(command, EngineCommand::Replay(_))
        && !matches!(command, EngineCommand::Reel(_))
        && !matches!(command, EngineCommand::SetNetworkStream { .. })
        && !matches!(command, EngineCommand::SetCaptureSettings { .. })
        && !matches!(command, EngineCommand::AnimationPreset(_))
        && !matches!(command, EngineCommand::SetTransitionFavorite { .. })
        && !matches!(command, EngineCommand::Media(_))
        && !matches!(command, EngineCommand::PageNavigate { .. })
        && !matches!(command, EngineCommand::MediaList(_))
        && !matches!(
            command,
            EngineCommand::InputLifecycle(edit) if !matches!(
                edit,
                crate::EngineInputLifecycleEdit::RemoveInputs { .. }
                    | crate::EngineInputLifecycleEdit::SetClosed { closed: true, .. }
            )
        )
        && !matches!(
            command,
            EngineCommand::StreamStart { .. }
                | EngineCommand::StreamStop { .. }
                | EngineCommand::StreamControl(_)
                | EngineCommand::SetStreamTarget { .. }
        )
        && !matches!(
            command,
            EngineCommand::RecordStart { .. } | EngineCommand::RecordStop { .. }
        )
        && !is_overlay_command(command)
        && !is_title_command(command)
}

/// The operator-facing lower-case name of a transition kind.
const fn transition_kind_name(kind: TransitionKind) -> &'static str {
    match kind {
        TransitionKind::Fade => "fade",
        TransitionKind::AlphaFade => "alpha fade",
        TransitionKind::Slide => "slide",
        TransitionKind::VerticalSlide => "vertical slide",
        TransitionKind::Zoom => "zoom",
        TransitionKind::Wipe => "wipe",
        TransitionKind::VerticalWipe => "vertical wipe",
        TransitionKind::Fly => "fly",
        TransitionKind::FlyRotate => "fly-rotate",
        TransitionKind::Cube => "cube",
        TransitionKind::CubeZoom => "cube-zoom",
        TransitionKind::CrossZoom => "cross-zoom",
        TransitionKind::Merge => "merge",
        TransitionKind::Stinger(_) => "stinger",
    }
}

const fn is_overlay_command(command: &EngineCommand) -> bool {
    matches!(
        command,
        EngineCommand::TakeOverlay { .. }
            | EngineCommand::UpdateOverlay { .. }
            | EngineCommand::OverlayOff { .. }
            | EngineCommand::ConfigureOverlayTransition { .. }
            | EngineCommand::ConfigureOverlayAppearance { .. }
            | EngineCommand::QueueOverlay { .. }
            | EngineCommand::TakeNextOverlay { .. }
            | EngineCommand::SetOverlayOutputInclusion { .. }
    )
}

const fn is_title_command(command: &EngineCommand) -> bool {
    matches!(
        command,
        EngineCommand::SetTitleFields { .. }
            | EngineCommand::TakeTitle { .. }
            | EngineCommand::TitleOffAir { .. }
    )
}

/// Returns the runtime action that realizes an accepted `command`.
///
/// Title takes and off-airs realize as [`TitleCue`]s on the overlay channel
/// they resolve to against the pre-command desired state, which is the
/// state the mutation validates, carrying the title's animation timing; a
/// refused command's action is cancelled before it runs.
/// The runtime action that re-cues a media list's open transport: its
/// planned media operations, realized in order on the target frame.
fn media_list_runtime(
    command: &EngineCommand,
    operations: &[crate::EngineMediaOperation],
) -> EngineCommand {
    let EngineCommand::MediaList(command) = command else {
        return command.clone();
    };
    EngineCommand::RealizeCueActions(
        operations
            .iter()
            .map(|operation| {
                EngineCommand::Media(crate::EngineMediaCommand {
                    input: command.input,
                    operation: *operation,
                })
            })
            .collect(),
    )
}

fn runtime_action(state: &ShowState, command: &EngineCommand) -> EngineCommand {
    let timing = |input: InputId| {
        state
            .title(input)
            .map_or_else(TitleAirTiming::default, |title| title.timing)
    };
    match command {
        EngineCommand::TakeTitle { input, channel, .. } => EngineCommand::RealizeTitle(TitleCue {
            input: *input,
            channel: *channel,
            action: TitleCueAction::Take,
            timing: timing(*input),
        }),
        EngineCommand::TitleOffAir { input } => {
            match title_off_air_channel(state.desired_switcher(), None, *input) {
                Ok(channel) => EngineCommand::RealizeTitle(TitleCue {
                    input: *input,
                    channel,
                    action: TitleCueAction::OffAir,
                    timing: timing(*input),
                }),
                Err(_) => command.clone(),
            }
        }
        // A new media list input joins the realized switcher like an
        // imported file (IN-006).
        EngineCommand::MediaList(crate::EngineMediaListCommand {
            input,
            operation: crate::EngineMediaListOperation::Create { items, .. },
        }) => EngineCommand::InputLifecycle(crate::EngineInputLifecycleEdit::Import {
            items: vec![fm_model::BatchImportRequest {
                input: *input,
                name: None,
                asset_uri: items
                    .first()
                    .map(|item| item.asset_uri.clone())
                    .unwrap_or_default(),
            }],
        }),
        // The realized switcher appends only the items the import accepts,
        // which the pre-command show determines.
        EngineCommand::InputLifecycle(crate::EngineInputLifecycleEdit::Import { items }) => {
            let accepted = crate::input_lifecycle::added_inputs(
                state,
                &crate::EngineInputLifecycleEdit::Import {
                    items: items.clone(),
                },
                &[],
            )
            .map(|(added, _)| added)
            .unwrap_or_default();
            EngineCommand::InputLifecycle(crate::EngineInputLifecycleEdit::Import {
                items: items
                    .iter()
                    .filter(|item| accepted.iter().any(|(input, _)| *input == item.input))
                    .cloned()
                    .collect(),
            })
        }
        _ => command.clone(),
    }
}

/// Realizes one title cue on the realized switcher at `frame`.
///
/// A take with a nonzero In phase cuts the channel to full opacity (the
/// animation is the entrance) and starts the In phase on `frame`; with no
/// In phase the channel's configured transition plays and the title holds.
/// An off-air with a nonzero Out phase holds the channel at its opacity for
/// exactly the Out phase's frames, then cuts it off; with no Out phase the
/// channel's configured transition plays over the Hold pose. A re-take
/// while the Out phase plays restarts the In phase at its first frame.
fn realize_title(
    switcher: &mut SwitcherState,
    title_air: &mut BTreeMap<InputId, (OverlayChannelId, TitleAir)>,
    cue: TitleCue,
    frame: u64,
    rate: FrameRate,
) -> Result<Vec<SwitcherEvent>, SwitcherError> {
    let TitleCue {
        input,
        channel,
        action,
        timing,
    } = cue;
    match action {
        TitleCueAction::Take => {
            let events = if timing.in_frames == 0 {
                switcher.request_overlay_take(channel, input)?
            } else {
                switcher.request_overlay_cut_take(channel, input)?
            };
            let air = match title_air.get(&input) {
                Some((shown_on, air)) if *shown_on == channel => air.retake(frame),
                _ => TitleAir::take(timing, rate, frame),
            };
            title_air.insert(input, (channel, air));
            Ok(events)
        }
        TitleCueAction::OffAir => {
            // A title put on air without a title take (a plain overlay take,
            // or one restored from a snapshot) holds; it leaves the same way.
            let air = title_air
                .get(&input)
                .filter(|(shown_on, _)| *shown_on == channel)
                .map_or_else(|| TitleAir::take(timing, rate, frame), |(_, air)| *air)
                .off_air(frame);
            let out_frames = air.phase_frames();
            if out_frames == 0 {
                title_air.remove(&input);
                Ok(switcher.request_overlay_off(channel))
            } else {
                title_air.insert(input, (channel, air));
                Ok(switcher.request_overlay_off_after(
                    channel,
                    u32::try_from(out_frames).unwrap_or(u32::MAX),
                ))
            }
        }
    }
}

/// Every tracked title at `frame`, ascending by input.
fn title_air_frames(
    title_air: &BTreeMap<InputId, (OverlayChannelId, TitleAir)>,
    frame: u64,
) -> Vec<EngineTitleAir> {
    title_air
        .iter()
        .filter_map(|(input, (channel, air))| {
            air.at(frame).map(|frame| EngineTitleAir {
                input: *input,
                channel: *channel,
                air: *air,
                frame,
            })
        })
        .collect()
}

/// Resolves the one desired overlay channel that takes a title off air.
fn title_off_air_channel(
    desired: &SwitcherState,
    realized_on_air: Option<InputUsage>,
    input: InputId,
) -> Result<OverlayChannelId, TitleRejection> {
    let on_bus = |usage: Option<InputUsage>| {
        matches!(
            usage,
            Some(InputUsage::Program | InputUsage::Transition | InputUsage::ManualTransition)
        )
    };
    if on_bus(desired.on_air_usage(input)) || on_bus(realized_on_air) {
        return Err(TitleRejection::OnProgram(input));
    }
    let mut channels = desired.overlay_channels_showing(input);
    match (channels.next(), channels.next()) {
        (None, _) => Err(TitleRejection::OffAir(input)),
        (Some(channel), None) => Ok(channel),
        (Some(_), Some(_)) => Err(TitleRejection::OnMultipleOverlays(input)),
    }
}

fn apply_title_mutation(
    state: &mut ShowState,
    events: &mut Vec<EngineEvent>,
    mutation: EngineMutation,
) -> Result<EngineAcceptance, Rejection> {
    let EngineMutation {
        command,
        target_frame,
        realized_on_air,
        ..
    } = mutation;
    let input = match &command {
        EngineCommand::SetTitleFields { input, .. }
        | EngineCommand::TakeTitle { input, .. }
        | EngineCommand::TitleOffAir { input } => *input,
        _ => unreachable!("only title commands are delegated"),
    };
    if state.title(input).is_none() {
        return Err(TitleRejection::UnknownTitle(input).into_rejection());
    }
    let on_air = state
        .desired_switcher()
        .on_air_usage(input)
        .or(realized_on_air)
        .is_some();
    match &command {
        EngineCommand::SetTitleFields {
            base_revision,
            require,
            fields,
            ..
        } => {
            match (require, on_air) {
                (TitleAirRequirement::OffAir, true) => {
                    return Err(TitleRejection::OnAir(input).into_rejection());
                }
                (TitleAirRequirement::OnAir, false) => {
                    return Err(TitleRejection::OffAir(input).into_rejection());
                }
                _ => {}
            }
            let revision = update_title_fields(state, input, *base_revision, fields)?;
            events.push(EngineEvent::TitleFieldsChanged { input, revision });
        }
        EngineCommand::TakeTitle {
            channel,
            base_revision,
            fields,
            ..
        } => {
            if on_air {
                return Err(TitleRejection::OnAir(input).into_rejection());
            }
            update_title_fields(state, input, *base_revision, fields)?;
            state
                .desired_switcher_mut()
                .apply(SwitcherCommand::TakeOverlay {
                    channel: *channel,
                    source: input,
                })
                .map_err(switcher_rejection)?;
            events.push(EngineEvent::DesiredSwitcherChanged(command));
        }
        EngineCommand::TitleOffAir { .. } => {
            let channel = title_off_air_channel(state.desired_switcher(), realized_on_air, input)
                .map_err(TitleRejection::into_rejection)?;
            state
                .desired_switcher_mut()
                .apply(SwitcherCommand::OverlayOff(channel))
                .map_err(switcher_rejection)?;
            events.push(EngineEvent::DesiredSwitcherChanged(command));
        }
        _ => unreachable!("only title commands are delegated"),
    }
    Ok(EngineAcceptance { target_frame })
}

fn update_title_fields(
    state: &mut ShowState,
    input: InputId,
    base_revision: u64,
    fields: &[(FieldId, FieldValue)],
) -> Result<u64, Rejection> {
    let title = state
        .title_mut(input)
        .ok_or_else(|| TitleRejection::UnknownTitle(input).into_rejection())?;
    title
        .fields
        .update(base_revision, fields)
        .map_err(|error| TitleRejection::from(error).into_rejection())
}

fn rename_input_rejection(error: RenameInputError) -> Rejection {
    let code = match error {
        RenameInputError::UnknownInput(_) => RejectionCode::NotFound,
        RenameInputError::EmptyName | RenameInputError::NameTooLong => {
            RejectionCode::InvalidCommand
        }
        RenameInputError::DuplicateName => RejectionCode::Conflict,
    };
    let reason = match error {
        RenameInputError::UnknownInput(input) => crate::error::unknown_input_reason(input),
        RenameInputError::EmptyName | RenameInputError::NameTooLong => {
            crate::error::invalid_input_name_reason()
        }
        RenameInputError::DuplicateName => RefusalReason::DuplicateName,
    };
    Rejection::new(code, error.to_string()).with_reason(reason)
}

fn reorder_inputs_rejection(error: InputOrderError) -> Rejection {
    let code = match error {
        InputOrderError::UnknownInput(_) => RejectionCode::NotFound,
        InputOrderError::EmptyOrder
        | InputOrderError::WrongLength { .. }
        | InputOrderError::DuplicateInput(_) => RejectionCode::InvalidCommand,
    };
    let rejection = Rejection::new(code, error.to_string());
    match error {
        InputOrderError::UnknownInput(input) => {
            rejection.with_reason(crate::error::unknown_input_reason(input))
        }
        _ => rejection,
    }
}

const fn engine_fade_to_black_state(switcher: &SwitcherState) -> EngineFadeToBlackState {
    EngineFadeToBlackState {
        active: switcher.fade_to_black(),
        position: switcher.fade_to_black_position(),
    }
}

fn validate_fade_to_black_duration(duration_frames: u32) -> Result<(), Rejection> {
    if duration_frames == 0 {
        Err(Rejection::new(
            RejectionCode::InvalidCommand,
            "Fade-to-Black duration must be nonzero",
        ))
    } else if duration_frames > fm_switcher::MAX_FADE_TO_BLACK_DURATION_FRAMES {
        Err(Rejection::new(
            RejectionCode::InvalidCommand,
            format!(
                "Fade-to-Black duration must not exceed {} frames",
                fm_switcher::MAX_FADE_TO_BLACK_DURATION_FRAMES
            ),
        ))
    } else {
        Ok(())
    }
}

fn validate_transition_duration(name: &str, duration_frames: u32) -> Result<(), Rejection> {
    if duration_frames == 0 {
        return Err(Rejection::new(
            RejectionCode::InvalidCommand,
            format!("{name} duration must be nonzero"),
        ));
    }
    if duration_frames > MAX_TRANSITION_DURATION_FRAMES {
        return Err(Rejection::new(
            RejectionCode::InvalidCommand,
            format!("{name} duration must not exceed {MAX_TRANSITION_DURATION_FRAMES} frames"),
        ));
    }
    Ok(())
}

fn stinger_runtime_transition(
    show: &ShowState,
    slot: fm_switcher::StingerSlotId,
) -> Option<TransitionKind> {
    match show.desired_switcher().stinger_playback_decision(slot) {
        fm_switcher::StingerPlaybackDecision::Play => Some(TransitionKind::Stinger(slot)),
        fm_switcher::StingerPlaybackDecision::Fallback(fm_switcher::MissingMediaFallback::Fade) => {
            Some(TransitionKind::Fade)
        }
        fm_switcher::StingerPlaybackDecision::Fallback(
            fm_switcher::MissingMediaFallback::Cut | fm_switcher::MissingMediaFallback::KeepProgram,
        )
        | fm_switcher::StingerPlaybackDecision::Unconfigured => None,
    }
}

const fn manual_start_command(kind: EngineManualTransitionKind) -> SwitcherCommand {
    match kind {
        EngineManualTransitionKind::Stinger {
            slot,
            duration_frames,
        } => SwitcherCommand::StartStingerTBar {
            slot,
            duration_frames,
        },
        _ => SwitcherCommand::StartTBar {
            kind: kind.transition_kind(),
        },
    }
}

fn engine_manual_state(state: fm_switcher::TBarState) -> EngineManualTransitionState {
    EngineManualTransitionState {
        kind: EngineManualTransitionKind::from_t_bar(state),
        from: state.from(),
        to: state.to(),
        interval_start: EngineManualTransitionPosition::new(state.interval_start().basis_points())
            .expect("switcher manual transition positions are bounded"),
        position: EngineManualTransitionPosition::new(state.position().basis_points())
            .expect("switcher manual transition positions are bounded"),
    }
}

fn switcher_rejection(error: SwitcherError) -> Rejection {
    let code = match error {
        SwitcherError::UnknownInput(_)
        | SwitcherError::UnconfiguredStinger(_)
        | SwitcherError::UnknownStreamTarget(_) => RejectionCode::NotFound,
        SwitcherError::TransitionInProgress
        | SwitcherError::StingerMediaUnavailable(_)
        | SwitcherError::DuplicateInput(_)
        | SwitcherError::InputInUse { .. } => RejectionCode::Conflict,
        SwitcherError::UnsupportedManualTransitionKind
        | SwitcherError::InvalidManualTransitionRoute
        | SwitcherError::PreviewEqualsProgram(_)
        | SwitcherError::ZeroDuration
        | SwitcherError::InvalidOverlayTransitionDuration { .. }
        | SwitcherError::OverlayQueueFull { .. }
        | SwitcherError::OverlayQueueEmpty(_)
        | SwitcherError::StingerCutPointOutOfRange { .. }
        | SwitcherError::DuplicateStreamTarget(_)
        | SwitcherError::InvalidStreamName
        | SwitcherError::TooManyStreams { .. } => RejectionCode::InvalidCommand,
    };
    let rejection = Rejection::new(code, error.to_string());
    match error {
        SwitcherError::UnknownInput(input) => {
            rejection.with_reason(crate::error::unknown_input_reason(input))
        }
        SwitcherError::InputInUse { input, usage } => {
            rejection.with_reason(RefusalReason::InputInUse {
                input: input.get().get(),
                usage: crate::error::input_use(usage),
            })
        }
        _ => rejection,
    }
}
