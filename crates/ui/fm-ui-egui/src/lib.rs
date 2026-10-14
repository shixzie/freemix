//! Pure `egui` presentation for the native `FreeMix` studio shell.
//!
//! This crate translates immutable client views into operator-facing controls
//! and returns intents. It performs no command dispatch, persistence, or I/O.
//!
//! Drawing is split by console region: `theme` (palette and style),
//! `chrome` (top and status bars), `monitors` (Preview/Program wells),
//! `transitions` (transition column, T-bar, FTB), `input_bank` (source
//! tiles), `mixer` (channel strips), `outputs` (recorder and stream status
//! rows), `panels`/`overlays` (side panel),
//! `menu` (menu bar and Help windows), `connection_overlay` (the status card
//! over the monitors), and `toasts` (transient notices). Pure helpers live in
//! `gating` (disabled-control reasons), `meter_ballistics` (peak hold),
//! `stream_control` (per-target stream Start/Stop gating), and
//! `stinger_config` (Stinger slot assignment and clearing), and
//! `input_catalogue` (live input add and remove, with its dialogs).

use egui::{
    Align, CentralPanel, Event, Frame, Key, Layout, Margin, Modifiers, Panel, ScrollArea, Stroke,
    Ui, Vec2,
};
use fm_protocol::{
    AudioMetersMessage, LiveInputSource, ManualTransitionKind, ManualTransitionPosition,
    MonitorFrameOrigin, MonitorImage, MonitorTap, StreamStatusMessage, WireOverlayChannelId,
    WireRecorderSlot, WireStingerSlotId, WireStreamTargetId,
};
use fm_types::{InputId, OutputId};
use fm_ui_model::i18n::{Locale, Msg};
use fm_ui_model::title_editor::{TitleAccess, TitleCommand};
use fm_ui_model::{ClientView, ManualTransitionStatus, ProjectState, StreamStatus, SwitcherState};

mod chrome;
mod connection_overlay;
mod edit_history;
mod fade_to_black;
mod gating;
mod health;
mod input_bank;
mod input_catalogue;
mod l10n;
mod menu;
mod meter_ballistics;
mod mixer;
mod monitors;
mod outputs;
mod overlay_status;
mod overlays;
mod panels;
mod playlist;
mod preferences;
mod stinger_config;
mod stream_control;
mod theme;
mod titles;
mod toasts;
mod transitions;

pub use edit_history::{
    EditHistoryAvailability, EditLockSummary, HistoryActionAvailability, HistoryVerb,
    PadlockToggle, REDO_SHORTCUT, UNDO_SHORTCUT, command_rejection_message,
    edit_history_availability, edit_lock_summary, input_edit_lock_reason, padlock_toggle,
    stinger_edit_lock_reason,
};
pub use fade_to_black::{FadeToBlackAvailability, FadeToBlackGate, fade_to_black_availability};
pub use input_catalogue::{
    CatalogueAvailability, CatalogueRefusal, CatalogueRequest, MAX_LIVE_INPUTS, NewInputKind,
    add_input_availability, catalogue_refusal_message, input_in_use_detail, input_name_problem,
    input_usage_reason, next_free_input_id, remove_input_availability, suggested_input_name,
};
pub use menu::ConsoleLayout;
pub use playlist::{
    Countdown, EntryForm, EntryRole, ListEdit, PlaylistIntent, PlaylistPhase,
    TransportAvailability, control_block as playlist_control_block,
    countdown as playlist_countdown, edit_block as playlist_edit_block,
    entry_role as playlist_entry_role, list_edit as playlist_list_edit,
    next_entry as playlist_next_entry, parse_duration as parse_playlist_duration,
    row_take as playlist_row_take, timecode,
    transport_availability as playlist_transport_availability,
};
pub use preferences::Appearance;
pub use stinger_config::{
    default_stinger_cut_point_frames, stinger_assign_intent, stinger_configuration_available,
    stinger_remove_intent, stinger_setup_block_reason,
};
pub use stream_control::{StreamAction, stream_action, stream_command_pending};
pub use titles::TitleUpdate;

use panels::SideTab;
use theme::{border_color, graphite_color, panel_color};

const NARROW_MONITOR_WIDTH: f32 = 700.0;
const SIDE_PANEL_WIDTH: f32 = 370.0;
/// The side panel's egui id; its dragged width lives in egui memory.
const SIDE_PANEL_ID: &str = "studio-side-panel";
const MIN_LOWER_ROW_HEIGHT: f32 = 370.0;

/// Changed controls for one input's Master audio strip.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InputAudioStripUpdate {
    pub gain_millidb: Option<i32>,
    pub balance_basis_points: Option<i32>,
    pub muted: Option<bool>,
    pub soloed: Option<bool>,
    pub follow_video: Option<bool>,
    pub delay_samples: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMoveDirection {
    Up,
    Down,
}

/// Operator actions emitted by [`StudioShell`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StudioIntent {
    /// Moves one input by one position in the current project order.
    MoveInput {
        input: InputId,
        direction: InputMoveDirection,
    },
    /// Renames one input using the exact submitted text.
    RenameInput { input: InputId, name: String },
    /// Updates changed Master-strip controls on one input.
    SetInputAudioStrip {
        input: InputId,
        update: InputAudioStripUpdate,
    },
    /// Selects an input on the Preview bus.
    SelectPreview(InputId),
    /// Performs an immediate Cut transition.
    Cut,
    /// Performs a Fade transition with a duration in frames.
    Fade { duration_frames: u32 },
    /// Performs an `AlphaFade` transition with a duration in frames.
    AlphaFade { duration_frames: u32 },
    /// Performs a horizontal Slide transition with a duration in frames.
    Slide { duration_frames: u32 },
    /// Performs a centered Zoom transition with a duration in frames.
    Zoom { duration_frames: u32 },
    /// Fires one of eight configured Stinger slots.
    Stinger {
        slot: WireStingerSlotId,
        duration_frames: u32,
    },
    /// Takes the current Preview source on one independent overlay channel.
    TakeOverlay {
        channel: WireOverlayChannelId,
        source: InputId,
    },
    /// Updates the source on one active independent overlay channel.
    UpdateOverlay {
        channel: WireOverlayChannelId,
        source: InputId,
    },
    /// Removes one overlay channel from Program.
    OverlayOff { channel: WireOverlayChannelId },
    /// Toggles one overlay channel between Cut and Fade.
    ToggleOverlayTransition {
        channel: WireOverlayChannelId,
        duration_frames: u32,
    },
    /// Cycles one overlay channel's position preset.
    CycleOverlayPosition { channel: WireOverlayChannelId },
    /// Cycles one overlay channel's border preset.
    CycleOverlayBorder { channel: WireOverlayChannelId },
    /// Appends the current Preview source to one overlay queue.
    QueueOverlay {
        channel: WireOverlayChannelId,
        source: InputId,
    },
    /// Takes and removes the head of one overlay queue.
    TakeNextOverlay { channel: WireOverlayChannelId },
    /// Toggles one overlay channel's inclusion on one configured output.
    ToggleOverlayOutput {
        channel: WireOverlayChannelId,
        output: OutputId,
    },
    /// Performs a Wipe transition with a duration in frames.
    Wipe { duration_frames: u32 },
    /// Fades realized Program video and audio to black or back to live.
    FadeToBlack { active: bool, duration_frames: u32 },
    /// Toggles Fade-to-Black from the latest confirmed desired target.
    ToggleFadeToBlack { duration_frames: u32 },
    /// Starts or stops the configured program recorder.
    Recording(RecordingAction),
    /// Starts or stops one configured stream target.
    Stream(StreamAction),
    /// Assigns one input as a Stinger slot's media. A configured slot keeps
    /// its other settings; an unconfigured slot is preloaded, mixes with
    /// Program audio, falls back to Cut, and uses
    /// `default_cut_point_frames` as its cut point.
    AssignStingerMedia {
        slot: WireStingerSlotId,
        media_input: InputId,
        default_cut_point_frames: u32,
    },
    /// Clears one configured Stinger slot.
    RemoveStinger { slot: WireStingerSlotId },
    /// Starts a held manual Fade, Wipe, `AlphaFade`, or Slide transition.
    StartManualTransition { kind: ManualTransitionKind },
    /// Sets the exact manual-transition position in basis points.
    SetManualTransitionPosition { position: ManualTransitionPosition },
    /// Commits the active manual transition at its current direction.
    CommitManualTransition,
    /// Cancels the active manual transition and restores its starting routing.
    CancelManualTransition,
    /// Reverts the shared history's top project edit, which must be the edit
    /// first accepted at `expected_edit` (the one the operator saw).
    Undo { expected_edit: u64 },
    /// Reapplies the most recently undone project edit, which must be the
    /// edit first accepted at `expected_edit`.
    Redo { expected_edit: u64 },
    /// Engages or releases the project-wide edit padlock.
    SetProjectLock { locked: bool },
    /// Appends a live generator input with a Studio-chosen unused identifier.
    AddInput {
        input: InputId,
        name: String,
        source: LiveInputSource,
    },
    /// Removes one input nothing on air or configured references.
    RemoveInput { input: InputId },
    /// Sends one title-editor command (save, take, update live, off air).
    /// The editor already marked the title pending; the worker answers with
    /// a [`TitleUpdate`] for `command.request`.
    Title(TitleCommand),
    /// Sends one playlist command: a list edit or transport.
    Playlist(PlaylistIntent),
}

impl StudioIntent {
    /// The title-editor request this intent carries, if any.
    #[must_use]
    pub const fn title_request(&self) -> Option<u64> {
        match self {
            Self::Title(command) => Some(command.request),
            _ => None,
        }
    }
}

/// External control actions accepted by native Studio ingress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalStudioAction {
    SelectPreview(u8),
    Cut,
    Fade,
    AlphaFade,
    Slide,
    Zoom,
    Wipe,
    FadeToBlack { active: bool },
    CommitManualTransition,
    CancelManualTransition,
}

/// Native studio connection lifecycle as presented to an operator.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StudioConnectionStatus {
    /// The native process is starting its control client.
    #[default]
    Launching,
    /// A transport connection is being established.
    Connecting,
    /// State is connected but not yet current.
    Synchronizing,
    /// State is current and operator controls are available.
    Ready,
    /// Reconnection is delayed by retry backoff.
    Backoff,
    /// No control connection is present.
    Disconnected,
    /// Connection or synchronization failed.
    Failed,
    /// The peer did not select the exact current protocol.
    ProtocolMismatch,
}

impl StudioConnectionStatus {
    /// Returns the compact uppercase label used in the studio header.
    #[must_use]
    pub const fn operator_label(self) -> &'static str {
        match self {
            Self::Launching => Msg::ConnLaunching.en(),
            Self::Connecting => Msg::ConnConnecting.en(),
            Self::Synchronizing => Msg::ConnSynchronizing.en(),
            Self::Ready => Msg::ConnReady.en(),
            Self::Backoff => Msg::ConnBackoff.en(),
            Self::Disconnected => Msg::ConnDisconnected.en(),
            Self::Failed => Msg::ConnFailed.en(),
            Self::ProtocolMismatch => Msg::ConnProtocolMismatch.en(),
        }
    }

    /// Returns whether this connection state permits production controls.
    #[must_use]
    pub const fn controls_enabled(self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Owned render input for a terminally uncertain command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalUncertaintyNotice {
    pub command_id: String,
    pub received_command_id: String,
}

/// Reconnect backoff facts shown while the control connection retries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectStatus {
    /// One-based reconnect attempt number.
    pub attempt: u32,
    /// Delay before the next attempt in milliseconds.
    pub delay_ms: u64,
}

/// Owned render input for one studio frame.
#[allow(clippy::struct_excessive_bools)] // each bool is a distinct operator permission gate
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StudioUiState {
    pub connection_status: StudioConnectionStatus,
    pub view: Option<ClientView>,
    pub can_select_preview: bool,
    pub can_transition: bool,
    pub can_control_audio: bool,
    pub can_edit_project: bool,
    pub pending_commands: usize,
    pub notice: Option<String>,
    pub external_notice: Option<String>,
    pub error: Option<String>,
    pub terminal_uncertainties: Vec<TerminalUncertaintyNotice>,
    pub audio_meters: Option<AudioMetersMessage>,
    pub streams: Vec<StreamStatus>,
    pub stream_status: Option<StreamStatusMessage>,
    /// Authoritative desired recording flag per recorder slot, slot order.
    pub recording_desired: [bool; RECORDER_SLOT_COUNT],
    /// Whether one recorder slot's record command is still unresolved.
    pub recording_pending: [bool; RECORDER_SLOT_COUNT],
    /// Stream targets whose start/stop command is still unresolved.
    pub stream_pending: Vec<WireStreamTargetId>,
    /// Reconnect attempt and delay while the connection is in backoff.
    pub reconnect: Option<ReconnectStatus>,
    /// Latest decoded encoded-fallback monitor frame per
    /// [`MonitorTap::index`] and when the client completed it.
    pub monitor_images: [Option<(MonitorImage, std::time::Instant)>; 2],
    /// Live input adds and removes sent by this client and not yet resolved,
    /// in send order.
    pub catalogue_pending: Vec<CatalogueRequest>,
    /// The latest refused input add or remove, published once with its
    /// rejection.
    pub catalogue_refusal: Option<CatalogueRefusal>,
    /// The replicated authoritative project state while control is ready;
    /// the title editor syncs its titles from it.
    pub project_state: Option<ProjectState>,
    /// Title access from the granted session permissions.
    pub title_access: TitleAccess,
    /// Title command answers in arrival order. Each is published once;
    /// the host hands them to [`StudioShell::receive_title_updates`].
    pub title_updates: Vec<TitleUpdate>,
}

impl StudioUiState {
    /// Creates a state without a replicated client view.
    #[must_use]
    pub const fn new(connection_status: StudioConnectionStatus) -> Self {
        Self {
            connection_status,
            view: None,
            can_select_preview: false,
            can_transition: false,
            can_control_audio: false,
            can_edit_project: false,
            pending_commands: 0,
            notice: None,
            external_notice: None,
            error: None,
            terminal_uncertainties: Vec::new(),
            audio_meters: None,
            streams: Vec::new(),
            stream_status: None,
            recording_desired: [false; RECORDER_SLOT_COUNT],
            recording_pending: [false; RECORDER_SLOT_COUNT],
            stream_pending: Vec::new(),
            reconnect: None,
            monitor_images: [None, None],
            catalogue_pending: Vec::new(),
            catalogue_refusal: None,
            project_state: None,
            title_access: TitleAccess::new(false, false, false),
            title_updates: Vec::new(),
        }
    }

    /// Attaches an owned client view.
    #[must_use]
    pub fn with_view(mut self, view: ClientView) -> Self {
        self.view = Some(view);
        self
    }

    /// Applies the switcher permissions granted for the active session.
    #[must_use]
    pub const fn with_switcher_permissions(
        mut self,
        can_select_preview: bool,
        can_transition: bool,
    ) -> Self {
        self.can_select_preview = can_select_preview;
        self.can_transition = can_transition;
        self
    }

    /// Applies the granted audio-control permission.
    #[must_use]
    pub const fn with_audio_permission(mut self, can_control_audio: bool) -> Self {
        self.can_control_audio = can_control_audio;
        self
    }

    /// Applies the granted project-edit permission.
    #[must_use]
    pub const fn with_edit_project_permission(mut self, can_edit_project: bool) -> Self {
        self.can_edit_project = can_edit_project;
        self
    }
}

/// The daemon's fixed number of independently controllable recorder slots.
pub const RECORDER_SLOT_COUNT: usize = 2;

/// Recording operator actions emitted by the compact recording panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingAction {
    /// Starts the addressed configured program recorder.
    Start(WireRecorderSlot),
    /// Stops the addressed configured program recorder.
    Stop(WireRecorderSlot),
}

impl RecordingAction {
    /// The recorder slot this action addresses.
    #[must_use]
    pub const fn recorder(self) -> WireRecorderSlot {
        match self {
            Self::Start(recorder) | Self::Stop(recorder) => recorder,
        }
    }

    /// Whether this action starts (`true`) or stops the addressed recorder.
    #[must_use]
    pub const fn starts(self) -> bool {
        matches!(self, Self::Start(_))
    }

    const fn slot_index(self) -> usize {
        self.recorder().get() as usize - 1
    }
}

/// Validates one requested recording action against the session gates and the
/// addressed slot's authoritative desired flag, returning it only when the
/// current state permits exactly that action.
#[must_use]
pub const fn recording_action(
    state: &StudioUiState,
    requested: RecordingAction,
) -> Option<RecordingAction> {
    let slot = requested.slot_index();
    let enabled = state.connection_status.controls_enabled()
        && state.view.is_some()
        && state.can_transition
        && !state.recording_pending[slot];
    match (enabled, state.recording_desired[slot], requested) {
        (true, false, RecordingAction::Start(_)) | (true, true, RecordingAction::Stop(_)) => {
            Some(requested)
        }
        _ => None,
    }
}

/// Pure transition-control availability derived from one UI state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionAvailability {
    base: TransitionBaseAvailability,
    pub alpha_fade: bool,
    pub wipe: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TransitionBaseAvailability {
    basic: bool,
    slide: bool,
    additive: AdditiveTransitionAvailability,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AdditiveTransitionAvailability {
    zoom: bool,
    stinger: bool,
}

impl TransitionAvailability {
    /// Returns whether protocol-independent Cut and Fade controls are available.
    #[must_use]
    pub const fn basic(self) -> bool {
        self.base.basic
    }

    /// Returns whether automatic Slide is available.
    #[must_use]
    pub const fn slide(self) -> bool {
        self.base.slide
    }

    /// Returns whether automatic Zoom is available.
    #[must_use]
    pub const fn zoom(self) -> bool {
        self.base.additive.zoom
    }

    /// Returns whether numbered Stinger controls are available.
    #[must_use]
    pub const fn stinger(self) -> bool {
        self.base.additive.stinger
    }
}

/// Session gates for automatic transition controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionGate {
    pub connection_status: StudioConnectionStatus,
    pub has_view: bool,
    pub can_transition: bool,
    pub manual_transition_in_flight: bool,
}

impl TransitionGate {
    #[must_use]
    pub const fn from_state(state: &StudioUiState) -> Self {
        Self {
            connection_status: state.connection_status,
            has_view: state.view.is_some(),
            can_transition: state.can_transition,
            manual_transition_in_flight: match &state.view {
                Some(view) => manual_transition_in_flight(view.switcher),
                None => false,
            },
        }
    }
}

/// Pure manual T-bar control availability derived from replicated state and session gates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManualTransitionAvailability {
    pub start: bool,
    pub alpha_fade_start_exposed: bool,
    pub active_controls: bool,
}

/// Session and replicated-state gates for manual T-bar controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManualTransitionGate {
    pub connection_status: StudioConnectionStatus,
    pub has_view: bool,
    pub can_transition: bool,
    pub manual_transition_in_flight: bool,
}

impl ManualTransitionGate {
    #[must_use]
    pub const fn from_state(state: &StudioUiState) -> Self {
        Self {
            connection_status: state.connection_status,
            has_view: state.view.is_some(),
            can_transition: state.can_transition,
            manual_transition_in_flight: match &state.view {
                Some(view) => manual_transition_in_flight(view.switcher),
                None => false,
            },
        }
    }
}

/// Computes transition availability without drawing or dispatching intents.
#[must_use]
pub const fn transition_availability(gate: TransitionGate) -> TransitionAvailability {
    let base = gate.connection_status.controls_enabled()
        && gate.has_view
        && gate.can_transition
        && !gate.manual_transition_in_flight;
    TransitionAvailability {
        base: TransitionBaseAvailability {
            basic: base,
            slide: base,
            additive: AdditiveTransitionAvailability {
                zoom: base,
                stinger: base,
            },
        },
        alpha_fade: base,
        wipe: base,
    }
}

const fn manual_transition_in_flight(switcher: SwitcherState) -> bool {
    matches!(
        switcher.desired_manual_transition,
        ManualTransitionStatus::Active(_)
    ) || matches!(
        switcher.realized_manual_transition,
        ManualTransitionStatus::Active(_)
    )
}

const fn preview_selection_available(gate: TransitionGate, can_select_preview: bool) -> bool {
    gate.connection_status.controls_enabled()
        && gate.has_view
        && can_select_preview
        && !gate.manual_transition_in_flight
}

/// Converts one external action using the latest replicated state and current operator gates.
#[must_use]
pub fn external_intent(
    state: &StudioUiState,
    action: ExternalStudioAction,
    transition_duration_frames: u32,
    fade_to_black_duration_frames: u32,
) -> Option<StudioIntent> {
    let gate = TransitionGate::from_state(state);
    let transition = transition_availability(gate).basic();
    match action {
        ExternalStudioAction::SelectPreview(number) => state
            .view
            .as_ref()
            .and_then(|view| view.inputs.get(usize::from(number.checked_sub(1)?)))
            .copied()
            .filter(|_| preview_selection_available(gate, state.can_select_preview))
            .map(StudioIntent::SelectPreview),
        ExternalStudioAction::Cut if transition => Some(StudioIntent::Cut),
        ExternalStudioAction::Fade if transition => Some(StudioIntent::Fade {
            duration_frames: transition_duration_frames,
        }),
        ExternalStudioAction::AlphaFade if transition => Some(StudioIntent::AlphaFade {
            duration_frames: transition_duration_frames,
        }),
        ExternalStudioAction::Slide if transition => Some(StudioIntent::Slide {
            duration_frames: transition_duration_frames,
        }),
        ExternalStudioAction::Zoom if transition => Some(StudioIntent::Zoom {
            duration_frames: transition_duration_frames,
        }),
        ExternalStudioAction::Wipe if transition => Some(StudioIntent::Wipe {
            duration_frames: transition_duration_frames,
        }),
        ExternalStudioAction::FadeToBlack { active } => {
            let desired = state
                .view
                .as_ref()
                .map(|view| view.switcher.desired_fade_to_black);
            let availability =
                fade_to_black_availability(FadeToBlackGate::from_state(state), desired);
            let allowed = if active {
                availability.to_black
            } else {
                availability.to_live
            };
            (allowed && !gate.manual_transition_in_flight).then_some(StudioIntent::FadeToBlack {
                active,
                duration_frames: fade_to_black_duration_frames,
            })
        }
        ExternalStudioAction::CommitManualTransition
        | ExternalStudioAction::CancelManualTransition => {
            let active = state.view.as_ref().is_some_and(|view| {
                matches!(
                    view.switcher.desired_manual_transition,
                    ManualTransitionStatus::Active(_)
                )
            });
            let available =
                manual_transition_availability(ManualTransitionGate::from_state(state), active);
            if !available.active_controls {
                return None;
            }
            Some(match action {
                ExternalStudioAction::CommitManualTransition => {
                    StudioIntent::CommitManualTransition
                }
                ExternalStudioAction::CancelManualTransition => {
                    StudioIntent::CancelManualTransition
                }
                _ => unreachable!(),
            })
        }
        _ => None,
    }
}

const fn numbered_preview_index(key: Key) -> Option<usize> {
    match key {
        Key::Num1 => Some(0),
        Key::Num2 => Some(1),
        Key::Num3 => Some(2),
        Key::Num4 => Some(3),
        Key::Num5 => Some(4),
        Key::Num6 => Some(5),
        Key::Num7 => Some(6),
        Key::Num8 => Some(7),
        _ => None,
    }
}

/// Section of the keyboard shortcuts reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShortcutGroup {
    Switching,
    Transitions,
    Editing,
    InputBank,
    Console,
}

impl ShortcutGroup {
    /// Every group in reference order.
    pub(crate) const ALL: [Self; 5] = [
        Self::Switching,
        Self::Transitions,
        Self::Editing,
        Self::InputBank,
        Self::Console,
    ];

    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Console => Msg::ShortcutGroupConsole.en(),
            Self::Switching => Msg::ShortcutGroupSwitching.en(),
            Self::Transitions => Msg::ShortcutGroupTransitions.en(),
            Self::Editing => Msg::ShortcutGroupEditing.en(),
            Self::InputBank => Msg::ShortcutGroupInputBank.en(),
        }
    }
}

/// One entry of the keyboard shortcuts reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShortcutEntry {
    pub(crate) group: ShortcutGroup,
    pub(crate) keys: &'static str,
    pub(crate) action: &'static str,
    /// The key handled by [`studio_shortcuts`], when this is a global key.
    pub(crate) key: Option<Key>,
}

/// The shortcuts reference shown in Help; global keys are verified against
/// [`studio_shortcuts`] by unit test.
pub(crate) const STUDIO_SHORTCUTS: [ShortcutEntry; 16] = [
    ShortcutEntry {
        group: ShortcutGroup::Switching,
        keys: "1 – 8",
        action: Msg::ShortcutSelectPreview.en(),
        key: Some(Key::Num1),
    },
    ShortcutEntry {
        group: ShortcutGroup::Switching,
        keys: "C",
        action: Msg::ShortcutCut.en(),
        key: Some(Key::C),
    },
    ShortcutEntry {
        group: ShortcutGroup::Transitions,
        keys: "F",
        action: Msg::ShortcutFade.en(),
        key: Some(Key::F),
    },
    ShortcutEntry {
        group: ShortcutGroup::Transitions,
        keys: "B",
        action: Msg::ShortcutFadeToBlack.en(),
        key: Some(Key::B),
    },
    ShortcutEntry {
        group: ShortcutGroup::Transitions,
        keys: "Esc",
        action: Msg::ShortcutCancelManual.en(),
        key: Some(Key::Escape),
    },
    ShortcutEntry {
        group: ShortcutGroup::Editing,
        keys: UNDO_SHORTCUT,
        action: Msg::ShortcutUndo.en(),
        key: None,
    },
    ShortcutEntry {
        group: ShortcutGroup::Editing,
        keys: REDO_SHORTCUT,
        action: Msg::ShortcutRedo.en(),
        key: None,
    },
    ShortcutEntry {
        group: ShortcutGroup::InputBank,
        keys: Msg::KeyClick.en(),
        action: Msg::ShortcutTileClick.en(),
        key: None,
    },
    ShortcutEntry {
        group: ShortcutGroup::InputBank,
        keys: Msg::KeyDoubleClick.en(),
        action: Msg::ShortcutTileDoubleClick.en(),
        key: None,
    },
    ShortcutEntry {
        group: ShortcutGroup::InputBank,
        keys: Msg::KeyRightClick.en(),
        action: Msg::ShortcutTileMenu.en(),
        key: None,
    },
    ShortcutEntry {
        group: ShortcutGroup::InputBank,
        keys: Msg::KeyTabEnter.en(),
        action: Msg::ShortcutTileFocus.en(),
        key: None,
    },
    // Handled by [`help_key`], which works whatever the connection state.
    ShortcutEntry {
        group: ShortcutGroup::Console,
        keys: Msg::KeyHelp.en(),
        action: Msg::ShortcutHelp.en(),
        key: None,
    },
    ShortcutEntry {
        group: ShortcutGroup::Console,
        keys: "Esc",
        action: Msg::ShortcutCloseDialog.en(),
        key: None,
    },
    // Handled by [`preferences::preference_key`] in every connection state.
    ShortcutEntry {
        group: ShortcutGroup::Console,
        keys: preferences::PREFERENCES_SHORTCUT,
        action: Msg::ShortcutPreferences.en(),
        key: None,
    },
    ShortcutEntry {
        group: ShortcutGroup::Console,
        keys: preferences::UI_SCALE_SHORTCUT,
        action: Msg::ShortcutUiScale.en(),
        key: None,
    },
    ShortcutEntry {
        group: ShortcutGroup::Console,
        keys: preferences::UI_SCALE_RESET_SHORTCUT,
        action: Msg::ShortcutUiScaleReset.en(),
        key: None,
    },
];

/// Whether a key press toggles the keyboard reference: `?` (any Shift
/// state, so it works on layouts where `?` needs Shift) or a bare F1.
pub(crate) fn help_key(key: Key, modifiers: Modifiers) -> bool {
    let plain = !(modifiers.ctrl || modifiers.alt || modifiers.command || modifiers.mac_cmd);
    match key {
        Key::Questionmark => plain,
        Key::F1 => modifiers.is_none(),
        _ => false,
    }
}

fn studio_shortcuts(
    ui: &mut Ui,
    gate: TransitionGate,
    can_select_preview: bool,
    manual_transition_active_controls: bool,
    inputs: &[InputId],
    transition_duration_frames: u32,
    fade_to_black_duration_frames: u32,
) -> Vec<StudioIntent> {
    if ui.ctx().egui_wants_keyboard_input() {
        return Vec::new();
    }
    let preview_available = preview_selection_available(gate, can_select_preview);
    let transition_available = transition_availability(gate).basic();
    if !preview_available && !transition_available && !manual_transition_active_controls {
        return Vec::new();
    }
    ui.input_mut(|input| {
        let mut intents = Vec::new();
        input.events.retain(|event| {
            let Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
                ..
            } = event
            else {
                return true;
            };
            match key {
                Key::B if transition_available => {
                    intents.push(StudioIntent::ToggleFadeToBlack {
                        duration_frames: fade_to_black_duration_frames,
                    });
                    false
                }
                Key::Escape if manual_transition_active_controls => {
                    intents.push(StudioIntent::CancelManualTransition);
                    false
                }
                Key::C if transition_available => {
                    intents.push(StudioIntent::Cut);
                    false
                }
                Key::F if transition_available => {
                    intents.push(StudioIntent::Fade {
                        duration_frames: transition_duration_frames,
                    });
                    false
                }
                _ if preview_available => {
                    let Some(input) =
                        numbered_preview_index(*key).and_then(|index| inputs.get(index))
                    else {
                        return true;
                    };
                    intents.push(StudioIntent::SelectPreview(*input));
                    false
                }
                _ => true,
            }
        });
        intents
    })
}

/// Computes manual T-bar availability without drawing or dispatching intents.
#[must_use]
pub fn manual_transition_availability(
    gate: ManualTransitionGate,
    active: bool,
) -> ManualTransitionAvailability {
    let base = gate.connection_status.controls_enabled() && gate.has_view && gate.can_transition;
    ManualTransitionAvailability {
        start: base && !gate.manual_transition_in_flight,
        alpha_fade_start_exposed: true,
        active_controls: base && active,
    }
}

/// Independent tally facts for one input.
///
/// Desired flags are set only when the corresponding bus has not yet realized
/// that desired input. Separate flags preserve unusual but valid cases where
/// one input occupies more than one role.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TallyState(u8);

impl TallyState {
    const REALIZED_PROGRAM: u8 = 1 << 0;
    const REALIZED_PREVIEW: u8 = 1 << 1;
    const DESIRED_PROGRAM: u8 = 1 << 2;
    const DESIRED_PREVIEW: u8 = 1 << 3;

    /// Returns true when the input is realized on the Program bus.
    #[must_use]
    pub const fn is_realized_program(self) -> bool {
        self.0 & Self::REALIZED_PROGRAM != 0
    }

    /// Returns true when the input is realized on the Preview bus.
    #[must_use]
    pub const fn is_realized_preview(self) -> bool {
        self.0 & Self::REALIZED_PREVIEW != 0
    }

    /// Returns true when the input is desired, but not realized, on Program.
    #[must_use]
    pub const fn is_desired_program(self) -> bool {
        self.0 & Self::DESIRED_PROGRAM != 0
    }

    /// Returns true when the input is desired, but not realized, on Preview.
    #[must_use]
    pub const fn is_desired_preview(self) -> bool {
        self.0 & Self::DESIRED_PREVIEW != 0
    }

    /// Returns true when the input has no realized or pending bus role.
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    /// Returns a compact, deterministic operator label for this tally.
    #[must_use]
    pub fn operator_label(self, locale: Locale) -> String {
        let mut labels = Vec::with_capacity(4);
        if self.is_realized_program() {
            labels.push(locale.text(Msg::TallyProgram));
        }
        if self.is_realized_preview() {
            labels.push(locale.text(Msg::TallyPreview));
        }
        if self.is_desired_program() {
            labels.push(locale.text(Msg::TallyProgramDesired));
        }
        if self.is_desired_preview() {
            labels.push(locale.text(Msg::TallyPreviewDesired));
        }
        if labels.is_empty() {
            locale.text(Msg::TallyNone).to_owned()
        } else {
            labels.join(" / ")
        }
    }
}

/// Formats a stable input label from its one-based ordinal and domain ID.
#[must_use]
pub fn input_label(ordinal: usize, input: InputId) -> String {
    format!("{ordinal:02} | Input {input}")
}

/// Computes realized and not-yet-realized desired tallies for an input.
#[must_use]
pub const fn tally_state(input: InputId, switcher: SwitcherState) -> TallyState {
    let mut flags = 0;
    if switcher.realized.program.get().get() == input.get().get() {
        flags |= TallyState::REALIZED_PROGRAM;
    }
    if switcher.realized.preview.get().get() == input.get().get() {
        flags |= TallyState::REALIZED_PREVIEW;
    }
    if switcher.desired.program.get().get() == input.get().get()
        && switcher.desired.program.get().get() != switcher.realized.program.get().get()
    {
        flags |= TallyState::DESIRED_PROGRAM;
    }
    if switcher.desired.preview.get().get() == input.get().get()
        && switcher.desired.preview.get().get() != switcher.realized.preview.get().get()
    {
        flags |= TallyState::DESIRED_PREVIEW;
    }
    TallyState(flags)
}

/// Stateful controller for the pure studio presentation.
#[allow(clippy::struct_excessive_bools)] // each bool is one independent dialog or drag state
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudioShell {
    transition_duration_frames: u32,
    fade_to_black_duration_frames: u32,
    rename_draft: Option<RenameDraft>,
    side_tab: SideTab,
    stinger_editor: stinger_config::StingerEditor,
    layout: ConsoleLayout,
    show_shortcuts: bool,
    show_about: bool,
    toasts: toasts::ToastQueue,
    meters: meter_ballistics::MeterBallistics,
    monitor_textures: monitors::MonitorTextures,
    /// Canvas shape of the latest monitor frame, kept while frames lapse.
    canvas: (u16, u16),
    /// The open Add input dialog.
    add_input: Option<input_catalogue::AddInputDraft>,
    /// The open Remove input confirmation.
    remove_input: Option<input_catalogue::RemoveConfirm>,
    /// Sent input adds and removes awaiting their outcome.
    catalogue: input_catalogue::CatalogueTracker,
    /// The operator's UI language and clock; local to this console.
    locale: Locale,
    /// The Titles tab and its title editor.
    titles: titles::TitlesPanel,
    /// The Playlist tab: selection, entry form, and countdown anchor.
    playlist: playlist::PlaylistPanel,
    /// Meter and stream message arrival times for the status bar strip.
    health: health::HealthTracker,
    /// Whether the side panel's edge was being dragged last frame.
    side_panel_resizing: bool,
    /// UI scale and contrast; local to this console.
    appearance: preferences::Appearance,
    /// The locale "Reset to defaults" returns to.
    default_locale: Locale,
    /// Whether the Preferences window is open.
    show_preferences: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RenameDraft {
    input: InputId,
    name: String,
    confirmed_name: String,
    /// The editor takes keyboard focus once, on the frame it opens.
    focus_pending: bool,
}

impl RenameDraft {
    fn new(input: InputId, name: &str) -> Self {
        Self {
            input,
            name: name.to_owned(),
            confirmed_name: name.to_owned(),
            focus_pending: true,
        }
    }
}

impl StudioShell {
    pub const MIN_TRANSITION_DURATION_FRAMES: u32 = 1;
    pub const MAX_TRANSITION_DURATION_FRAMES: u32 = 3_600;
    pub const DEFAULT_TRANSITION_DURATION_FRAMES: u32 = 30;

    /// Returns the current transition duration in frames.
    #[must_use]
    pub const fn transition_duration_frames(&self) -> u32 {
        self.transition_duration_frames
    }

    /// Sets and clamps the transition duration to the supported frame range.
    pub fn set_transition_duration_frames(&mut self, duration_frames: u32) {
        self.transition_duration_frames = duration_frames.clamp(
            Self::MIN_TRANSITION_DURATION_FRAMES,
            Self::MAX_TRANSITION_DURATION_FRAMES,
        );
    }

    /// Returns the current Fade-to-Black duration in frames.
    #[must_use]
    pub const fn fade_to_black_duration_frames(&self) -> u32 {
        self.fade_to_black_duration_frames
    }

    /// Sets and clamps the Fade-to-Black duration to the supported frame range.
    pub fn set_fade_to_black_duration_frames(&mut self, duration_frames: u32) {
        self.fade_to_black_duration_frames = duration_frames.clamp(
            Self::MIN_TRANSITION_DURATION_FRAMES,
            Self::MAX_TRANSITION_DURATION_FRAMES,
        );
    }

    /// Returns which console regions are visible.
    #[must_use]
    pub const fn layout(&self) -> ConsoleLayout {
        self.layout
    }

    /// Shows or hides console regions.
    pub const fn set_layout(&mut self, layout: ConsoleLayout) {
        self.layout = layout;
    }

    /// Restores every panel and the default side panel width, forgetting
    /// the width egui keeps for the dragged panel edge.
    pub(crate) fn reset_layout(&mut self, context: &egui::Context) {
        self.layout = ConsoleLayout::default();
        self.side_panel_resizing = false;
        context.data_mut(|data| data.remove::<egui::PanelState>(egui::Id::new(SIDE_PANEL_ID)));
    }

    /// Keeps the side panel at the layout's width for this window: egui
    /// remembers the panel's last width itself, so after a window resize or
    /// a restored preference that width is dropped and the panel takes
    /// `width` as its default. Never during a drag of the panel edge.
    fn apply_side_panel_width(&self, context: &egui::Context, width: f32) {
        let id = egui::Id::new(SIDE_PANEL_ID);
        if !self.side_panel_resizing
            && egui::PanelState::load(context, id)
                .is_some_and(|panel| (panel.size().x - width).abs() > 0.5)
        {
            context.data_mut(|data| data.remove::<egui::PanelState>(id));
        }
    }

    /// Records the side panel width once a drag of its edge ends, so the
    /// host can remember it; window resizes that clamp the panel are not
    /// operator choices and are not recorded.
    fn track_side_panel_width(&mut self, context: &egui::Context, default_width: f32) {
        let id = egui::Id::new(SIDE_PANEL_ID);
        let dragging = context
            .read_response(id.with("__resize"))
            .is_some_and(|response| response.dragged());
        if self.side_panel_resizing
            && !dragging
            && let Some(panel) = egui::PanelState::load(context, id)
        {
            self.layout
                .record_side_panel_width(panel.size().x, default_width);
        }
        self.side_panel_resizing = dragging;
    }

    /// Returns the console's UI language and clock preference.
    #[must_use]
    pub const fn locale(&self) -> Locale {
        self.locale
    }

    /// Switches the UI language or clock; the next frame draws in it. The
    /// choice is local to this console and never sent to the daemon.
    pub const fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    /// Collects this frame's global keyboard shortcuts in UI order: switcher
    /// shortcuts first, then edit history (Ctrl+Z / Ctrl+Shift+Z).
    fn keyboard_intents(
        &self,
        ui: &mut Ui,
        state: &StudioUiState,
        manual_transition_controls: ManualTransitionAvailability,
    ) -> Vec<StudioIntent> {
        if self.console_keys_blocked(ui.ctx()) {
            return Vec::new();
        }
        let inputs = state
            .view
            .as_ref()
            .map_or(&[] as &[InputId], |view| view.inputs.as_slice());
        let mut intents = studio_shortcuts(
            ui,
            TransitionGate::from_state(state),
            state.can_select_preview,
            manual_transition_controls.active_controls,
            inputs,
            self.transition_duration_frames,
            self.fade_to_black_duration_frames,
        );
        intents.extend(edit_history::history_shortcuts(
            ui,
            edit_history_availability(state),
        ));
        intents
    }

    /// `?` or F1 opens the keyboard reference, and closes it again, unless
    /// a text field, another dialog, or a menu owns the keyboard. Works in
    /// every connection state, unlike the console keys.
    fn toggle_help_on_key(&mut self, ui: &mut Ui) {
        let context = ui.ctx().clone();
        let other_owner = self.show_about
            || self.show_preferences
            || self.add_input.is_some()
            || self.remove_input.is_some()
            || (!self.show_shortcuts && egui::Popup::is_any_open(&context));
        if other_owner || context.egui_wants_keyboard_input() {
            return;
        }
        let pressed = ui.input_mut(|input| {
            let mut pressed = false;
            input.events.retain(|event| match event {
                Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } if help_key(*key, *modifiers) => {
                    pressed = true;
                    false
                }
                _ => true,
            });
            pressed
        });
        if pressed {
            self.show_shortcuts = !self.show_shortcuts;
        }
    }

    /// Whether a dialog, menu, or popup owns the keyboard. Dialogs are modal
    /// and an open menu or popup closes on Esc, so Esc must not also cancel a
    /// live manual transition, nor may console keys act behind them.
    fn console_keys_blocked(&self, context: &egui::Context) -> bool {
        self.show_shortcuts
            || self.show_about
            || self.show_preferences
            || self.add_input.is_some()
            || self.remove_input.is_some()
            || egui::Popup::is_any_open(context)
    }

    /// Applies the worker's title command answers to the title editor, in
    /// order. Call once per received [`StudioUiState`] with its
    /// [`StudioUiState::title_updates`] before drawing it, and with an
    /// [`TitleUpdate::Abandoned`] for a [`StudioIntent::Title`] that could
    /// not be handed to the worker.
    pub fn receive_title_updates(&mut self, updates: impl IntoIterator<Item = TitleUpdate>) {
        self.titles.receive(updates);
    }

    /// Unblocks every title command still awaiting an answer. Call when the
    /// worker that would answer them has stopped.
    pub fn abandon_title_requests(&mut self) {
        self.titles.abandon_all();
    }

    /// Reconciles console-local drafts and dialogs with the latest state.
    fn reconcile_local_state(&mut self, state: &StudioUiState) {
        self.reconcile_rename_draft(state);
        self.reconcile_catalogue_dialogs(state);
        self.titles.sync(state);
        self.playlist.sync(state, std::time::Instant::now());
    }

    /// Publishes the console locale to this frame's drawing code and to the
    /// queues that word their own toasts.
    fn apply_locale(&mut self, context: &egui::Context) {
        l10n::set_locale(context, self.locale);
        self.toasts.locale = self.locale;
        self.catalogue.locale = self.locale;
    }

    /// Draws one complete shell frame and returns operator intents in UI order.
    ///
    /// The UI must belong to an active `egui` pass, as is customary for
    /// immediate-mode drawing.
    pub fn draw(&mut self, ui: &mut Ui, state: &StudioUiState) -> Vec<StudioIntent> {
        self.handle_preference_keys(ui);
        theme::apply_console_visuals(ui.ctx(), self.appearance.high_contrast);
        self.apply_ui_scale(ui.ctx());
        self.apply_locale(ui.ctx());
        self.monitor_textures.sync(ui.ctx(), &state.monitor_images);
        self.canvas = monitors::canvas_shape(&state.monitor_images, self.canvas);
        if state.monitor_images.iter().any(Option::is_some) {
            // Frames age into STALE without any new state arriving.
            ui.ctx()
                .request_repaint_after(monitors::MONITOR_REPAINT_INTERVAL);
        }
        self.set_transition_duration_frames(self.transition_duration_frames);
        self.set_fade_to_black_duration_frames(self.fade_to_black_duration_frames);

        self.reconcile_local_state(state);
        let manual_transition_controls = manual_transition_availability(
            ManualTransitionGate::from_state(state),
            state.view.as_ref().is_some_and(|view| {
                matches!(
                    view.switcher.desired_manual_transition,
                    ManualTransitionStatus::Active(_)
                )
            }),
        );
        self.toggle_help_on_key(ui);
        let mut intents = self.keyboard_intents(ui, state, manual_transition_controls);
        // Studio repaints on timers, so egui's `stable_dt` stays at its 1/60 s
        // prediction however long a frame gap really is; toasts and meter
        // ballistics age by the measured time since the previous frame.
        let dt_ms = meter_ballistics::frame_dt_ms(ui.input(|input| input.unstable_dt));
        // Age existing toasts before announcing new messages, so a message
        // arriving after a long idle gap still gets its full lifetime.
        self.toasts.tick(dt_ms);
        self.observe_toasts(state, dt_ms);
        let bar_frame = Frame::new()
            .fill(panel_color())
            .stroke(Stroke::new(crate::theme::line_width(), border_color()))
            .inner_margin(Margin::symmetric(14, 0));
        Panel::top("studio-menu-bar")
            .exact_size(26.0)
            .frame(
                Frame::new()
                    .fill(theme::graphite_color())
                    .stroke(Stroke::new(crate::theme::line_width(), border_color()))
                    .inner_margin(Margin::symmetric(8, 0)),
            )
            .show(ui, |ui| {
                self.draw_menu_bar(ui, state, manual_transition_controls, &mut intents);
            });
        Panel::top("studio-top-bar")
            .exact_size(46.0)
            .frame(bar_frame)
            .show(ui, |ui| chrome::draw_top_bar(ui, state, &mut intents));
        let now = std::time::Instant::now();
        self.health.observe(state, now);
        if self.layout.show_status_bar {
            // The lamps age with the monitor, meter, and clock repaints.
            let health = health::health_items(self.locale, state, &self.health, now);
            Panel::bottom("studio-status-bar")
                .exact_size(26.0)
                .frame(bar_frame)
                .show(ui, |ui| chrome::draw_status_bar(ui, state, &health));
        }
        if self.layout.show_side_panel {
            self.draw_side_panel_region(ui, state, &mut intents);
        }
        let central = CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(graphite_color())
                    .inner_margin(Margin::same(12)),
            )
            .show(ui, |ui| {
                // Tall canvases may grow their wells into about half the
                // console height; 16:9 wells always fill their width.
                let well_budget = (ui.available_height() * 0.5).max(200.0);
                // Small windows keep every region reachable by scrolling.
                ScrollArea::vertical()
                    .id_salt("studio-console")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let top = ui.cursor().min;
                        self.draw_switcher_row(
                            ui,
                            state,
                            manual_transition_controls,
                            well_budget,
                            &mut intents,
                        );
                        let monitors = egui::Rect::from_min_max(
                            top,
                            egui::pos2(top.x + ui.available_width(), ui.min_rect().max.y),
                        );
                        if let Some(card) = connection_overlay::connection_card(self.locale, state)
                        {
                            connection_overlay::paint_connection_card(ui, monitors, &card);
                        }
                        if self.layout.show_input_bank || self.layout.show_mixer {
                            ui.add_space(12.0);
                            self.draw_lower_row(ui, state, dt_ms, &mut intents);
                        }
                    });
            });
        self.draw_toasts(ui.ctx(), central.response.rect);
        self.draw_help_windows(ui.ctx(), state);
        self.draw_preferences_window(ui.ctx());
        self.draw_catalogue_dialogs(ui.ctx(), state, &mut intents);
        self.track_catalogue_intents(ui.ctx(), state, &intents);
        theme::paint_focus_ring(ui.ctx());
        intents
    }

    /// The resizable side panel at the layout's remembered width.
    fn draw_side_panel_region(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        // Compact windows give the monitors the width; wide ones may grow
        // the side panel up to a quarter of the window.
        let max_side = (ui.available_width() * 0.25).clamp(320.0, 560.0);
        let default_side = SIDE_PANEL_WIDTH.min(max_side);
        let side_width = self.layout.side_panel_width_within(default_side, max_side);
        self.apply_side_panel_width(ui.ctx(), side_width);
        Panel::right(SIDE_PANEL_ID)
            .resizable(true)
            .default_size(side_width)
            .min_size(f32::from(ConsoleLayout::MIN_SIDE_PANEL_WIDTH))
            .max_size(max_side)
            .frame(
                Frame::new()
                    .fill(panel_color())
                    .stroke(Stroke::new(crate::theme::line_width(), border_color()))
                    .inner_margin(Margin::same(10)),
            )
            .show(ui, |ui| self.draw_side_panel(ui, state, intents));
        // After the panel stored this frame's width, so the width on
        // the frame a drag ends is the released one.
        self.track_side_panel_width(ui.ctx(), default_side);
    }

    /// Announces this frame's new messages and input add/remove outcomes.
    fn observe_toasts(&mut self, state: &StudioUiState, dt_ms: u32) {
        let catalogue = self.catalogue.observe(state, dt_ms);
        if catalogue.refusal_announced {
            // The refusal toast below already explains this rejection.
            self.toasts.acknowledge_error(state.error.as_ref());
        }
        self.toasts.observe(state);
        for toast in catalogue.toasts {
            self.toasts.upsert(&toast.key, toast.kind, toast.message);
        }
    }

    /// Starts tracking this frame's input adds and removes with a pending
    /// toast each.
    fn track_catalogue_intents(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &[StudioIntent],
    ) {
        let pending = self.catalogue.submit_intents(intents, state.view.as_ref());
        if pending.is_empty() {
            return;
        }
        for toast in pending {
            self.toasts.upsert(&toast.key, toast.kind, toast.message);
        }
        context.request_repaint_after(std::time::Duration::from_millis(100));
    }

    /// Stacked toasts at the bottom-right of the console area.
    fn draw_toasts(&mut self, context: &egui::Context, area: egui::Rect) {
        if self.toasts.is_empty() {
            return;
        }
        context.request_repaint_after(std::time::Duration::from_millis(100));
        let mut dismissed = None;
        egui::Area::new(egui::Id::new("studio-toasts"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_BOTTOM)
            .fixed_pos(area.right_bottom() - Vec2::new(16.0, 16.0))
            .interactable(true)
            .show(context, |ui| {
                ui.set_width(380.0);
                for toast in self.toasts.iter() {
                    if chrome::draw_toast(ui, toast) {
                        dismissed = Some(toast.id);
                    }
                    ui.add_space(6.0);
                }
            });
        if let Some(id) = dismissed {
            self.toasts.dismiss(id);
        }
    }

    /// Preview monitor, transition column, Program monitor.
    fn draw_switcher_row(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        manual: ManualTransitionAvailability,
        well_budget: f32,
        intents: &mut Vec<StudioIntent>,
    ) {
        let gap = 12.0;
        let canvas = self.canvas;
        let available = ui.available_width();
        let column = transitions::TRANSITION_COLUMN_WIDTH + 2.0 * gap;
        let now = std::time::Instant::now();
        let native_runtime = state
            .monitor_images
            .iter()
            .flatten()
            .any(|(image, _)| image.origin == MonitorFrameOrigin::Native);
        let pictures = MonitorTap::ALL.map(|tap| {
            self.monitor_textures
                .picture(tap, &state.monitor_images, now)
        });
        let draw_bus = |ui: &mut Ui, bus: monitors::Bus, width: f32| {
            monitors::draw_monitor(
                ui,
                bus,
                state.view.as_ref(),
                monitors::WellShape {
                    width,
                    video_height: monitors::monitor_video_height(width, canvas, well_budget),
                    canvas,
                },
                pictures[bus.tap().index()],
                native_runtime,
            );
        };
        if available >= NARROW_MONITOR_WIDTH {
            let monitor_width = ((available - column) / 2.0).floor();
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                draw_bus(ui, monitors::Bus::Preview, monitor_width);
                ui.vertical(|ui| {
                    transitions::draw_transition_column(ui, self, state, manual, intents);
                });
                draw_bus(ui, monitors::Bus::Program, monitor_width);
            });
        } else {
            let monitor_width = ((available - gap) / 2.0).floor();
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                draw_bus(ui, monitors::Bus::Preview, monitor_width);
                draw_bus(ui, monitors::Bus::Program, monitor_width);
            });
            ui.add_space(gap);
            ui.vertical(|ui| {
                transitions::draw_transition_column(ui, self, state, manual, intents);
            });
        }
    }

    /// Input bank beside the audio mixer; stacked when the window is narrow.
    /// Either region may be hidden from the View menu.
    fn draw_lower_row(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        dt_ms: u32,
        intents: &mut Vec<StudioIntent>,
    ) {
        let strips = state
            .view
            .as_ref()
            .map_or(0, |view| view.input_audio_strips.len());
        let available = ui.available_width();
        let height = ui.available_height().max(MIN_LOWER_ROW_HEIGHT);
        let show_bank = self.layout.show_input_bank;
        let show_mixer = self.layout.show_mixer;
        let mixer_width = if show_bank {
            // Without synchronized strips the mixer keeps a readable share
            // instead of collapsing to the Master strip's width.
            let wanted = if state.view.is_some() {
                mixer::mixer_width(strips)
            } else {
                available * 0.4
            };
            wanted.min(available * 0.55)
        } else {
            available
        };
        let bank_width = if show_mixer {
            available - mixer_width - 12.0
        } else {
            available
        };
        let side_by_side =
            !(show_bank && show_mixer) || bank_width >= input_bank::MIN_TILE_WIDTH * 2.0 + 24.0;
        if side_by_side {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                if show_bank {
                    ui.allocate_ui_with_layout(
                        Vec2::new(bank_width, height),
                        Layout::top_down(Align::Min),
                        |ui| {
                            theme::card().show(ui, |ui| {
                                ui.set_min_size(Vec2::new(ui.available_width(), height - 22.0));
                                self.draw_inputs(ui, state, intents);
                            });
                        },
                    );
                }
                if show_mixer {
                    ui.allocate_ui_with_layout(
                        Vec2::new(mixer_width, height),
                        Layout::top_down(Align::Min),
                        |ui| {
                            theme::card().show(ui, |ui| {
                                ui.set_min_size(Vec2::new(ui.available_width(), height - 22.0));
                                mixer::draw_mixer(ui, state, &mut self.meters, dt_ms, intents);
                            });
                        },
                    );
                }
            });
        } else {
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(MIN_LOWER_ROW_HEIGHT);
                self.draw_inputs(ui, state, intents);
            });
            ui.add_space(12.0);
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                mixer::draw_mixer(ui, state, &mut self.meters, dt_ms, intents);
            });
        }
    }
}

impl Default for StudioShell {
    fn default() -> Self {
        Self {
            transition_duration_frames: Self::DEFAULT_TRANSITION_DURATION_FRAMES,
            fade_to_black_duration_frames: Self::DEFAULT_TRANSITION_DURATION_FRAMES,
            rename_draft: None,
            side_tab: SideTab::default(),
            stinger_editor: stinger_config::StingerEditor::default(),
            layout: ConsoleLayout::default(),
            show_shortcuts: false,
            show_about: false,
            toasts: toasts::ToastQueue::default(),
            meters: meter_ballistics::MeterBallistics::default(),
            monitor_textures: monitors::MonitorTextures::default(),
            canvas: monitors::DEFAULT_CANVAS,
            add_input: None,
            remove_input: None,
            catalogue: input_catalogue::CatalogueTracker::default(),
            locale: Locale::ENGLISH,
            titles: titles::TitlesPanel::default(),
            playlist: playlist::PlaylistPanel::default(),
            health: health::HealthTracker::default(),
            side_panel_resizing: false,
            appearance: preferences::Appearance::default(),
            default_locale: Locale::ENGLISH,
            show_preferences: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_protocol::{
        OverlayBorderPreset, OverlayPositionPreset, OverlayTransitionKind, StingerReadiness,
        StreamRealizedState, StreamStatusSample,
    };
    use fm_ui_model::{ActiveManualTransition, BusSelection, ProjectCursor};

    use super::*;
    use crate::monitors::monitor_routing;
    use crate::overlays::overlay_update_intent;
    use crate::panels::{
        STREAMS_NONE_CONFIGURED, StingerSlotPresentation, StingerSlotState, latest_stream_sample,
        recording_status_label, stinger_slot_presentation, stinger_slot_state, stream_row_text,
        streams_show_none_configured,
    };
    use crate::theme::{AMBER, ERROR, PREVIEW, muted_color, routing_color};
    use crate::transitions::{desired_manual_position, manual_transition_label};

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn switcher(desired: (u128, u128), realized: (u128, u128)) -> SwitcherState {
        SwitcherState {
            desired: BusSelection::new(input(desired.0), input(desired.1)),
            realized: BusSelection::new(input(realized.0), input(realized.1)),
            desired_manual_transition: ManualTransitionStatus::Inactive,
            realized_manual_transition: ManualTransitionStatus::Inactive,
            desired_fade_to_black: fm_protocol::FadeToBlackState {
                target_active: false,
                position: fm_protocol::FadeToBlackPosition::LIVE,
            },
            realized_fade_to_black: fm_protocol::FadeToBlackState {
                target_active: false,
                position: fm_protocol::FadeToBlackPosition::LIVE,
            },
            runtime_generation: None,
        }
    }

    #[test]
    fn only_ready_enables_connection_controls() {
        let statuses = [
            StudioConnectionStatus::Launching,
            StudioConnectionStatus::Connecting,
            StudioConnectionStatus::Synchronizing,
            StudioConnectionStatus::Backoff,
            StudioConnectionStatus::Disconnected,
            StudioConnectionStatus::Failed,
            StudioConnectionStatus::ProtocolMismatch,
        ];
        assert!(StudioConnectionStatus::Ready.controls_enabled());
        assert_eq!(
            StudioConnectionStatus::Ready.operator_label(),
            "CONTROL READY"
        );
        for status in statuses {
            assert!(!status.controls_enabled(), "{status:?}");
            assert_ne!(status.operator_label(), "");
        }
    }

    #[test]
    fn switcher_permissions_are_explicit_and_separate() {
        let state = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_switcher_permissions(true, false);
        assert!(state.can_select_preview);
        assert!(!state.can_transition);
    }

    #[test]
    fn input_labels_preserve_ordinal_and_full_width_id() {
        let full_width = input(u128::MAX);
        assert_eq!(
            input_label(1, full_width),
            format!("01 | Input {}", u128::MAX)
        );
        assert_eq!(input_label(128, input(7)), "128 | Input 7");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn fade_to_black_shortcut_preserves_order_durations_and_operator_gates() {
        let inputs = [11, 12, 13, 14, 15, 16, 17, 18].map(input);
        let gate = TransitionGate {
            connection_status: StudioConnectionStatus::Ready,
            has_view: true,
            can_transition: true,
            manual_transition_in_flight: false,
        };
        let key = |key, modifiers, repeat| Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers,
        };
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        shell.set_transition_duration_frames(42);
        let duration = shell.transition_duration_frames();
        shell.set_fade_to_black_duration_frames(84);
        let fade_to_black_duration = shell.fade_to_black_duration_frames();
        let run = |events, gate, can_select_preview, manual_transition_active_controls, focused| {
            context.begin_pass(egui::RawInput {
                events,
                ..Default::default()
            });
            if focused {
                context.memory_mut(|memory| memory.request_focus(egui::Id::new("text-editor")));
            }
            let mut ui = egui::Ui::new(
                context.clone(),
                egui::Id::new("studio-shortcut"),
                egui::UiBuilder::new()
                    .layer_id(egui::LayerId::background())
                    .max_rect(context.viewport_rect()),
            );
            let intents = studio_shortcuts(
                &mut ui,
                gate,
                can_select_preview,
                manual_transition_active_controls,
                &inputs,
                duration,
                fade_to_black_duration,
            );
            let remaining = context.input(|input| input.events.clone());
            drop(ui);
            let _ = context.end_pass();
            (intents, remaining)
        };
        let shifted = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        let (intents, remaining) = run(
            vec![
                key(Key::Escape, Modifiers::NONE, false),
                key(Key::B, Modifiers::NONE, false),
                key(Key::C, Modifiers::NONE, false),
                key(Key::F, Modifiers::NONE, false),
                key(Key::Num1, Modifiers::NONE, false),
                key(Key::B, shifted, false),
                key(Key::Num2, shifted, false),
                key(Key::Escape, shifted, false),
                key(Key::Num8, Modifiers::NONE, false),
                key(Key::Num9, Modifiers::NONE, false),
            ],
            gate,
            true,
            true,
            false,
        );
        assert_eq!(
            intents,
            vec![
                StudioIntent::CancelManualTransition,
                StudioIntent::ToggleFadeToBlack {
                    duration_frames: fade_to_black_duration,
                },
                StudioIntent::Cut,
                StudioIntent::Fade {
                    duration_frames: duration,
                },
                StudioIntent::SelectPreview(input(11)),
                StudioIntent::SelectPreview(input(18)),
            ]
        );
        assert!(matches!(
            remaining.as_slice(),
            [
                Event::Key { key: Key::B, .. },
                Event::Key { key: Key::Num2, .. },
                Event::Key {
                    key: Key::Escape,
                    ..
                },
                Event::Key { key: Key::Num9, .. }
            ]
        ));

        context.begin_pass(egui::RawInput {
            events: vec![key(Key::Num3, Modifiers::NONE, false)],
            ..Default::default()
        });
        let _ = context.end_pass();
        let (intents, remaining) = run(
            vec![
                key(Key::B, Modifiers::NONE, true),
                key(Key::Num3, Modifiers::NONE, false),
            ],
            gate,
            true,
            true,
            false,
        );
        assert_eq!(intents, Vec::new());
        assert!(matches!(
            remaining.as_slice(),
            [
                Event::Key {
                    key: Key::B,
                    repeat: true,
                    ..
                },
                Event::Key {
                    key: Key::Num3,
                    repeat: true,
                    ..
                }
            ]
        ));

        let blocked = TransitionGate {
            manual_transition_in_flight: true,
            ..gate
        };
        let rejected_events = vec![
            key(Key::Escape, Modifiers::NONE, false),
            key(Key::B, Modifiers::NONE, false),
            key(Key::C, Modifiers::NONE, false),
            key(Key::F, Modifiers::NONE, false),
        ];
        let (intents, remaining) = run(rejected_events.clone(), blocked, true, false, false);
        assert_eq!(intents, Vec::new());
        assert!(matches!(
            remaining.as_slice(),
            [
                Event::Key {
                    key: Key::Escape,
                    ..
                },
                Event::Key { key: Key::B, .. },
                Event::Key { key: Key::C, .. },
                Event::Key { key: Key::F, .. }
            ]
        ));

        let (intents, remaining) = run(
            vec![key(Key::Num1, Modifiers::NONE, false)],
            gate,
            false,
            false,
            false,
        );
        assert_eq!(intents, Vec::new());
        assert_eq!(remaining.len(), 1);

        for blocked in [
            TransitionGate {
                connection_status: StudioConnectionStatus::Synchronizing,
                ..gate
            },
            TransitionGate {
                has_view: false,
                ..gate
            },
            TransitionGate {
                can_transition: false,
                ..gate
            },
        ] {
            let (intents, remaining) = run(
                vec![key(Key::B, Modifiers::NONE, false)],
                blocked,
                true,
                false,
                false,
            );
            assert_eq!(intents, Vec::new());
            assert_eq!(remaining.len(), 1);
        }

        let (intents, remaining) = run(rejected_events, gate, true, true, true);
        assert_eq!(intents, Vec::new());
        assert!(matches!(
            remaining.as_slice(),
            [
                Event::Key {
                    key: Key::Escape,
                    ..
                },
                Event::Key { key: Key::B, .. },
                Event::Key { key: Key::C, .. },
                Event::Key { key: Key::F, .. }
            ]
        ));
    }

    #[test]
    fn open_menus_and_dialogs_keep_esc_for_themselves() {
        let context = egui::Context::default();
        let state = StudioUiState::new(StudioConnectionStatus::Ready);
        let manual = ManualTransitionAvailability {
            start: false,
            alpha_fade_start_exposed: true,
            active_controls: true,
        };
        let run = |shell: &StudioShell, popup: bool| {
            context.begin_pass(egui::RawInput {
                events: vec![Event::Key {
                    key: Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            });
            if popup {
                egui::Popup::open_id(&context, egui::Id::new("studio-menu-under-test"));
            }
            let mut ui = egui::Ui::new(
                context.clone(),
                egui::Id::new("studio-blocked-keys"),
                egui::UiBuilder::new()
                    .layer_id(egui::LayerId::background())
                    .max_rect(context.viewport_rect()),
            );
            let intents = shell.keyboard_intents(&mut ui, &state, manual);
            let remaining = context.input(|input| input.events.len());
            drop(ui);
            egui::Popup::close_all(&context);
            let _ = context.end_pass();
            (intents, remaining)
        };
        let mut shell = StudioShell::default();
        assert_eq!(
            run(&shell, false),
            (vec![StudioIntent::CancelManualTransition], 0)
        );
        // An open menu or context menu closes on Esc instead.
        assert_eq!(run(&shell, true), (Vec::new(), 1));
        // So do the modal input dialogs.
        shell.remove_input = Some(input_catalogue::RemoveConfirm {
            input: input(3),
            name: "Graphics".to_owned(),
        });
        assert_eq!(run(&shell, false), (Vec::new(), 1));
        shell.remove_input = None;
        shell.show_shortcuts = true;
        assert_eq!(run(&shell, false), (Vec::new(), 1));
    }

    #[test]
    fn shortcut_reference_lists_exactly_the_handled_keys() {
        let inputs = [11, 12].map(input);
        let gate = TransitionGate {
            connection_status: StudioConnectionStatus::Ready,
            has_view: true,
            can_transition: true,
            manual_transition_in_flight: false,
        };
        let context = egui::Context::default();
        let press = |key| {
            context.begin_pass(egui::RawInput {
                events: vec![Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            });
            let mut ui = egui::Ui::new(
                context.clone(),
                egui::Id::new("studio-shortcut-reference"),
                egui::UiBuilder::new()
                    .layer_id(egui::LayerId::background())
                    .max_rect(context.viewport_rect()),
            );
            let intents = studio_shortcuts(&mut ui, gate, true, true, &inputs, 30, 30);
            drop(ui);
            let _ = context.end_pass();
            intents
        };
        let listed: Vec<Key> = STUDIO_SHORTCUTS
            .iter()
            .filter_map(|entry| entry.key)
            .collect();
        for key in &listed {
            assert_eq!(press(*key).len(), 1, "{key:?} is listed but not handled");
        }
        // The numbered row is listed once as "1 – 8"; other keys stay unbound.
        assert_eq!(press(Key::Num2).len(), 1);
        for key in [
            Key::A,
            Key::D,
            Key::E,
            Key::G,
            Key::Space,
            Key::Enter,
            Key::Num9,
        ] {
            assert!(press(key).is_empty(), "{key:?} is handled but not listed");
        }
        assert!(
            STUDIO_SHORTCUTS
                .iter()
                .all(|entry| !entry.action.is_empty())
        );
    }

    #[test]
    fn console_layout_defaults_to_every_region_and_round_trips() {
        let mut shell = StudioShell::default();
        assert_eq!(shell.layout(), ConsoleLayout::default());
        let layout = ConsoleLayout {
            show_mixer: false,
            ..ConsoleLayout::default()
        };
        shell.set_layout(layout);
        assert_eq!(shell.layout(), layout);
    }

    #[test]
    fn console_layout_preferences_round_trip_key_by_key() {
        let layout = ConsoleLayout {
            show_input_bank: false,
            show_side_panel: false,
            side_panel_width: Some(512),
            ..ConsoleLayout::default()
        };
        let mut read = ConsoleLayout::default();
        for (key, value) in layout.preference_entries() {
            assert!(read.apply_preference(key, &value), "{key} = {value}");
        }
        assert_eq!(read, layout);
        let keys = layout.preference_entries().map(|(key, _)| key);
        assert_eq!(keys, ConsoleLayout::PREFERENCE_KEYS);
        // Unknown keys and unusable values leave the layout unchanged.
        let mut unchanged = ConsoleLayout::default();
        for (key, value) in [
            ("layout.mixer", "sometimes"),
            ("layout.side_panel_width", "299"),
            ("layout.side_panel_width", "1201"),
            ("layout.side_panel_width", "-4"),
            ("language", "es"),
        ] {
            assert!(!unchanged.apply_preference(key, value), "{key} = {value}");
        }
        assert_eq!(unchanged, ConsoleLayout::default());
        assert!(unchanged.apply_preference("layout.status_bar", "off"));
        assert!(!unchanged.show_status_bar);
        assert!(unchanged.apply_preference("layout.side_panel_width", "300"));
        assert!(unchanged.apply_preference("layout.side_panel_width", "auto"));
        assert_eq!(unchanged.side_panel_width, None);
    }

    #[test]
    fn side_panel_width_is_recorded_from_drags_and_clamped_to_the_window() {
        let mut layout = ConsoleLayout::default();
        // Automatic: the default width, clamped like any other.
        assert!((layout.side_panel_width_within(370.0, 480.0) - 370.0).abs() < f32::EPSILON);
        assert!((layout.side_panel_width_within(320.0, 320.0) - 320.0).abs() < f32::EPSILON);
        layout.record_side_panel_width(452.4, 370.0);
        assert_eq!(layout.side_panel_width, Some(452));
        assert!((layout.side_panel_width_within(370.0, 560.0) - 452.0).abs() < f32::EPSILON);
        // A smaller window clamps the drawn width but keeps the choice.
        assert!((layout.side_panel_width_within(320.0, 320.0) - 320.0).abs() < f32::EPSILON);
        assert_eq!(layout.side_panel_width, Some(452));
        // Dragging back to the default width returns to automatic.
        layout.record_side_panel_width(370.4, 370.0);
        assert_eq!(layout.side_panel_width, None);
        assert_eq!(layout, ConsoleLayout::default());
        layout.record_side_panel_width(9_000.0, 370.0);
        assert_eq!(
            layout.side_panel_width,
            Some(ConsoleLayout::MAX_SIDE_PANEL_WIDTH)
        );
    }

    #[test]
    fn reset_layout_restores_panels_and_forgets_the_dragged_width() {
        let context = egui::Context::default();
        let id = egui::Id::new(SIDE_PANEL_ID);
        context.data_mut(|data| {
            data.insert_persisted(
                id,
                egui::PanelState {
                    outer_rect: egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(500.0, 400.0),
                    ),
                },
            );
        });
        let mut shell = StudioShell::default();
        shell.set_layout(ConsoleLayout {
            show_mixer: false,
            side_panel_width: Some(500),
            ..ConsoleLayout::default()
        });
        shell.reset_layout(&context);
        assert_eq!(shell.layout(), ConsoleLayout::default());
        assert!(egui::PanelState::load(&context, id).is_none());
    }

    #[test]
    fn question_mark_and_f1_toggle_the_keyboard_reference() {
        let shift = Modifiers::SHIFT;
        assert!(help_key(Key::Questionmark, Modifiers::NONE));
        assert!(help_key(Key::Questionmark, shift));
        assert!(help_key(Key::F1, Modifiers::NONE));
        assert!(!help_key(Key::F1, shift));
        assert!(!help_key(Key::Questionmark, Modifiers::CTRL));
        assert!(!help_key(Key::Slash, Modifiers::NONE));
        // The reference lists the help key in its own Console group, which
        // the key handler does not own (it works in every connection state).
        let console: Vec<_> = STUDIO_SHORTCUTS
            .iter()
            .filter(|entry| entry.group == ShortcutGroup::Console)
            .collect();
        assert_eq!(console.len(), 5);
        assert_eq!(console[0].keys, Msg::KeyHelp.en());
        assert!(console.iter().all(|entry| entry.key.is_none()));
        // Every group has entries, and every entry's group is drawn.
        for group in ShortcutGroup::ALL {
            assert!(STUDIO_SHORTCUTS.iter().any(|entry| entry.group == group));
        }

        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        let press = |shell: &mut StudioShell, key| {
            let event = |pressed| Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: shift,
            };
            context.begin_pass(egui::RawInput {
                // Released in the same pass, so the next press is not a repeat.
                events: vec![event(true), event(false)],
                ..Default::default()
            });
            let mut ui = egui::Ui::new(
                context.clone(),
                egui::Id::new("studio-help-key"),
                egui::UiBuilder::new()
                    .layer_id(egui::LayerId::background())
                    .max_rect(context.viewport_rect()),
            );
            shell.toggle_help_on_key(&mut ui);
            drop(ui);
            let _ = context.end_pass();
        };
        press(&mut shell, Key::Questionmark);
        assert!(shell.show_shortcuts);
        press(&mut shell, Key::Questionmark);
        assert!(!shell.show_shortcuts);
        // Another dialog owns the keyboard.
        shell.show_about = true;
        press(&mut shell, Key::Questionmark);
        assert!(!shell.show_shortcuts);
    }

    #[test]
    fn monitor_routing_projects_preview_and_program_names() {
        let inputs = [input(1), input(2), input(3)];
        let names = [
            "Camera A".to_owned(),
            "Camera B".to_owned(),
            "Graphics".to_owned(),
        ];
        let switcher = switcher((3, 2), (1, 2));
        let preview = monitor_routing(Locale::ENGLISH, switcher, &inputs, &names, |selection| {
            selection.preview
        });
        let program = monitor_routing(Locale::ENGLISH, switcher, &inputs, &names, |selection| {
            selection.program
        });

        assert_eq!(preview.desired, (input(2), "Camera B".to_owned()));
        assert_eq!(preview.realized, (input(2), "Camera B".to_owned()));
        assert_eq!(program.desired, (input(3), "Graphics".to_owned()));
        assert_eq!(program.realized, (input(1), "Camera A".to_owned()));
        assert_eq!(
            routing_color(preview.desired.0, preview.realized.0),
            muted_color()
        );
        assert_eq!(routing_color(program.desired.0, program.realized.0), AMBER);
    }

    #[test]
    fn tally_reports_realized_program_and_preview() {
        let state = switcher((1, 2), (1, 2));
        let program = tally_state(input(1), state);
        assert!(program.is_realized_program());
        assert!(!program.is_realized_preview());
        assert!(!program.is_desired_program());
        assert!(!program.is_desired_preview());

        let preview = tally_state(input(2), state);
        assert!(preview.is_realized_preview());
        assert!(!preview.is_realized_program());
        assert!(!preview.is_desired_program());
        assert!(!preview.is_desired_preview());
        assert!(tally_state(input(3), state).is_none());
    }

    #[test]
    fn tally_reports_desired_buses_until_each_is_realized() {
        let state = switcher((3, 4), (1, 2));
        assert!(tally_state(input(3), state).is_desired_program());
        assert!(tally_state(input(4), state).is_desired_preview());
        assert!(tally_state(input(1), state).is_realized_program());
        assert!(tally_state(input(2), state).is_realized_preview());
    }

    #[test]
    fn tally_preserves_combined_roles() {
        let state = switcher((2, 3), (1, 2));
        let tally = tally_state(input(2), state);
        assert!(tally.is_realized_preview());
        assert!(tally.is_desired_program());
        assert!(!tally.is_realized_program());
        assert!(!tally.is_desired_preview());
        assert_eq!(
            tally.operator_label(Locale::ENGLISH),
            "PREVIEW / PROGRAM DESIRED"
        );
        assert_eq!(
            tally.operator_label(Locale::new(fm_ui_model::i18n::Language::Spanish)),
            "PREVIO / PROGRAMA PEDIDO"
        );
    }

    #[test]
    fn transition_duration_is_shared_and_bounded() {
        let mut shell = StudioShell::default();
        assert_eq!(
            shell.transition_duration_frames(),
            StudioShell::DEFAULT_TRANSITION_DURATION_FRAMES
        );
        shell.set_transition_duration_frames(0);
        assert_eq!(
            shell.transition_duration_frames(),
            StudioShell::MIN_TRANSITION_DURATION_FRAMES
        );
        shell.set_transition_duration_frames(u32::MAX);
        assert_eq!(
            shell.transition_duration_frames(),
            StudioShell::MAX_TRANSITION_DURATION_FRAMES
        );
        assert_eq!(
            shell.fade_to_black_duration_frames(),
            StudioShell::DEFAULT_TRANSITION_DURATION_FRAMES
        );
        shell.set_fade_to_black_duration_frames(0);
        assert_eq!(
            shell.fade_to_black_duration_frames(),
            StudioShell::MIN_TRANSITION_DURATION_FRAMES
        );
        shell.set_fade_to_black_duration_frames(u32::MAX);
        assert_eq!(
            shell.fade_to_black_duration_frames(),
            StudioShell::MAX_TRANSITION_DURATION_FRAMES
        );
    }

    #[test]
    fn transition_availability_requires_ready_view_and_permission() {
        let base = TransitionGate {
            connection_status: StudioConnectionStatus::Ready,
            has_view: true,
            can_transition: true,
            manual_transition_in_flight: false,
        };
        assert_eq!(
            transition_availability(base),
            TransitionAvailability {
                base: TransitionBaseAvailability {
                    basic: true,
                    slide: true,
                    additive: AdditiveTransitionAvailability {
                        zoom: true,
                        stinger: true,
                    },
                },
                alpha_fade: true,
                wipe: true,
            }
        );
        let supported = transition_availability(base);
        assert!(supported.wipe);
        assert!(supported.alpha_fade);
        assert!(supported.slide());
        assert!(supported.zoom());
        assert!(supported.stinger());
        assert!(supported.basic());
        for gate in [
            TransitionGate {
                connection_status: StudioConnectionStatus::Connecting,
                ..base
            },
            TransitionGate {
                has_view: false,
                ..base
            },
            TransitionGate {
                can_transition: false,
                ..base
            },
        ] {
            assert_eq!(
                transition_availability(gate),
                TransitionAvailability {
                    base: TransitionBaseAvailability {
                        basic: false,
                        slide: false,
                        additive: AdditiveTransitionAvailability {
                            zoom: false,
                            stinger: false,
                        },
                    },
                    alpha_fade: false,
                    wipe: false,
                }
            );
        }
    }

    #[test]
    fn manual_transition_in_flight_blocks_conflicting_operator_controls() {
        let active = ManualTransitionStatus::Active(ActiveManualTransition {
            kind: ManualTransitionKind::Fade,
            from: input(1),
            to: input(2),
            interval_start: ManualTransitionPosition::START,
            position: ManualTransitionPosition::START,
        });
        let mut desired_active = switcher((1, 2), (1, 2));
        desired_active.desired_manual_transition = active;
        let mut realized_active = switcher((1, 2), (1, 2));
        realized_active.realized_manual_transition = active;
        for (switcher, desired_active) in [(desired_active, true), (realized_active, false)] {
            let gate = TransitionGate {
                connection_status: StudioConnectionStatus::Ready,
                has_view: true,
                can_transition: true,
                manual_transition_in_flight: manual_transition_in_flight(switcher),
            };
            assert!(gate.manual_transition_in_flight);
            assert!(!transition_availability(gate).basic());
            assert!(!preview_selection_available(gate, true));
            let manual = manual_transition_availability(
                ManualTransitionGate {
                    connection_status: StudioConnectionStatus::Ready,
                    has_view: true,
                    can_transition: true,
                    manual_transition_in_flight: true,
                },
                desired_active,
            );
            assert!(!manual.start);
            assert_eq!(manual.active_controls, desired_active);
        }
        let fade_to_black = fade_to_black::fade_to_black_availability(
            fade_to_black::FadeToBlackGate {
                connection_status: StudioConnectionStatus::Ready,
                has_view: true,
                can_transition: true,
            },
            Some(desired_active.desired_fade_to_black),
        );
        assert!(fade_to_black.to_black);
    }

    fn stream_roster_entry(value: u128, name: &str, desired_running: bool) -> StreamStatus {
        StreamStatus {
            target: NonZeroU128::new(value).unwrap(),
            name: name.to_owned(),
            desired_running,
            realized: StreamRealizedState::Stopped,
            detail: None,
        }
    }

    fn stream_sample(target: u128, failure: Option<&str>) -> StreamStatusSample {
        StreamStatusSample {
            target: fm_protocol::WireStreamTargetId::new(NonZeroU128::new(target).unwrap()),
            realized: StreamRealizedState::Live,
            connected: true,
            muxed_bytes: 2048,
            enqueued_pairs: 12,
            dropped_pairs: 3,
            failure: failure.map(str::to_owned),
            local: None,
        }
    }

    #[test]
    fn streams_placeholder_applies_only_when_ready_with_view_and_empty_roster() {
        assert_eq!(STREAMS_NONE_CONFIGURED, "No stream targets configured");
        let empty: [StreamStatus; 0] = [];
        assert!(streams_show_none_configured(
            StudioConnectionStatus::Ready,
            true,
            &empty
        ));
        assert!(!streams_show_none_configured(
            StudioConnectionStatus::Ready,
            false,
            &empty
        ));
        for status in [
            StudioConnectionStatus::Launching,
            StudioConnectionStatus::Connecting,
            StudioConnectionStatus::Synchronizing,
            StudioConnectionStatus::Backoff,
            StudioConnectionStatus::Disconnected,
            StudioConnectionStatus::Failed,
            StudioConnectionStatus::ProtocolMismatch,
        ] {
            assert!(
                !streams_show_none_configured(status, true, &empty),
                "{status:?}"
            );
        }
        let roster = [stream_roster_entry(40, "RTMP Primary", true)];
        assert!(!streams_show_none_configured(
            StudioConnectionStatus::Ready,
            true,
            &roster
        ));
    }

    fn recording_view(recordings: [bool; RECORDER_SLOT_COUNT]) -> ClientView {
        ClientView {
            cursor: ProjectCursor::from_resume_cursor(
                fm_types::ProjectId::new(NonZeroU128::new(1).unwrap()),
                fm_protocol::ResumeCursor {
                    server: fm_protocol::ServerIdentity {
                        engine_id: "e".to_owned(),
                        project_id: "1".to_owned(),
                        state_epoch: 1,
                        log_id: "l".to_owned(),
                    },
                    revision: 1,
                },
            )
            .unwrap(),
            show_name: "show".to_owned(),
            inputs: vec![input(1)],
            input_names: vec!["Input 1".to_owned()],
            outputs: Vec::new(),
            input_audio_strips: Vec::new(),
            stingers: Vec::new(),
            streams: Vec::new(),
            recordings: vec![
                fm_protocol::RecordingStatus {
                    recorder: REC_1,
                    desired_active: recordings[0],
                },
                fm_protocol::RecordingStatus {
                    recorder: REC_2,
                    desired_active: recordings[1],
                },
            ],
            desired_overlays: Vec::new(),
            realized_overlays: Vec::new(),
            switcher: switcher((1, 1), (1, 1)),
            edit_locks: fm_ui_model::EditLocks::default(),
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        }
    }

    fn recording_state(desired: [bool; RECORDER_SLOT_COUNT], pending: bool) -> StudioUiState {
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_view(recording_view(desired))
            .with_switcher_permissions(true, true);
        state.recording_desired = desired;
        state.recording_pending = [pending; RECORDER_SLOT_COUNT];
        state
    }

    const REC_1: WireRecorderSlot = WireRecorderSlot::new(1).expect("slot one exists");
    const REC_2: WireRecorderSlot = WireRecorderSlot::new(2).expect("slot two exists");

    #[test]
    fn recording_controls_require_ready_view_and_transition_permission() {
        let available = recording_state([false, false], false);
        assert_eq!(
            recording_action(&available, RecordingAction::Start(REC_1)),
            Some(RecordingAction::Start(REC_1))
        );
        let denied = [
            // The transition permission is missing.
            StudioUiState::new(StudioConnectionStatus::Ready)
                .with_view(recording_view([false, false]))
                .with_switcher_permissions(true, false),
            // No replicated view is present.
            StudioUiState::new(StudioConnectionStatus::Ready).with_switcher_permissions(true, true),
            // The lifecycle is not ready.
            StudioUiState::new(StudioConnectionStatus::Connecting)
                .with_view(recording_view([false, false]))
                .with_switcher_permissions(true, true),
        ];
        for state in denied {
            assert_eq!(
                recording_action(&state, RecordingAction::Start(REC_1)),
                None
            );
            assert_eq!(recording_action(&state, RecordingAction::Stop(REC_2)), None);
        }
    }

    #[test]
    fn recording_action_follows_the_authoritative_desired_flag_per_slot() {
        let state = recording_state([false, true], false);
        assert_eq!(
            recording_action(&state, RecordingAction::Start(REC_1)),
            Some(RecordingAction::Start(REC_1))
        );
        assert_eq!(
            recording_action(&state, RecordingAction::Stop(REC_2)),
            Some(RecordingAction::Stop(REC_2))
        );
        // Slot two is recording, so its Start is wrong; slot one is stopped,
        // so its Stop is wrong.
        assert_eq!(recording_action(&state, RecordingAction::Stop(REC_1)), None);
        assert_eq!(
            recording_action(&state, RecordingAction::Start(REC_2)),
            None
        );
        assert_eq!(recording_status_label(false), "STOPPED");
        assert_eq!(recording_status_label(true), "RECORDING");
    }

    #[test]
    fn recording_pending_blocks_a_second_intent_per_slot() {
        for (desired, action) in [
            ([false, false], RecordingAction::Start(REC_1)),
            ([true, true], RecordingAction::Stop(REC_2)),
        ] {
            assert_eq!(
                recording_action(&recording_state(desired, true), action),
                None
            );
        }
        // A pending slot does not disable the other slot's action.
        let mut state = recording_state([false, false], false);
        state.recording_pending = [true, false];
        assert_eq!(
            recording_action(&state, RecordingAction::Start(REC_1)),
            None
        );
        assert_eq!(
            recording_action(&state, RecordingAction::Start(REC_2)),
            Some(RecordingAction::Start(REC_2))
        );
    }

    #[test]
    fn recording_click_must_match_the_authoritative_desired_flag() {
        assert_eq!(
            recording_action(
                &recording_state([false, false], false),
                RecordingAction::Stop(REC_1)
            ),
            None
        );
        assert_eq!(
            recording_action(
                &recording_state([true, false], false),
                RecordingAction::Start(REC_1)
            ),
            None
        );
    }

    fn stream_target(value: u128) -> WireStreamTargetId {
        WireStreamTargetId::new(NonZeroU128::new(value).unwrap())
    }

    fn stream_state(roster: Vec<StreamStatus>) -> StudioUiState {
        let mut view = recording_view([false, false]);
        view.streams.clone_from(&roster);
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_view(view)
            .with_switcher_permissions(true, true);
        state.streams = roster;
        state
    }

    #[test]
    fn stream_controls_require_ready_view_transition_permission_and_a_roster_entry() {
        let roster = vec![stream_roster_entry(40, "RTMP Primary", false)];
        let start = StreamAction::Start(stream_target(40));
        assert_eq!(
            stream_action(&stream_state(roster.clone()), start),
            Some(start)
        );
        let mut no_permission = stream_state(roster.clone());
        no_permission.can_transition = false;
        let mut no_view = stream_state(roster.clone());
        no_view.view = None;
        let mut connecting = stream_state(roster.clone());
        connecting.connection_status = StudioConnectionStatus::Backoff;
        for state in [no_permission, no_view, connecting] {
            assert_eq!(stream_action(&state, start), None);
        }
        // A target missing from the replicated roster is never offered.
        assert_eq!(
            stream_action(
                &stream_state(roster),
                StreamAction::Start(stream_target(41))
            ),
            None
        );
        assert_eq!(
            gating::unavailable_reason(
                &StudioUiState::new(StudioConnectionStatus::Ready)
                    .with_view(recording_view([false, false])),
                gating::Capability::Transition
            ),
            "this session lacks the transition permission"
        );
    }

    #[test]
    fn stream_action_follows_the_desired_flag_and_pending_target() {
        let state = stream_state(vec![
            stream_roster_entry(40, "RTMP Primary", true),
            stream_roster_entry(41, "HLS", false),
        ]);
        assert_eq!(
            StreamAction::for_stream(&state.streams[0]),
            StreamAction::Stop(stream_target(40))
        );
        assert_eq!(
            StreamAction::for_stream(&state.streams[1]),
            StreamAction::Start(stream_target(41))
        );
        assert_eq!(
            stream_action(&state, StreamAction::Start(stream_target(40))),
            None
        );
        assert_eq!(
            stream_action(&state, StreamAction::Stop(stream_target(41))),
            None
        );
        let mut pending = state.clone();
        pending.stream_pending = vec![stream_target(40)];
        assert!(stream_command_pending(&pending, stream_target(40)));
        assert_eq!(
            stream_action(&pending, StreamAction::Stop(stream_target(40))),
            None
        );
        // A pending target does not block another target.
        assert_eq!(
            stream_action(&pending, StreamAction::Start(stream_target(41))),
            Some(StreamAction::Start(stream_target(41)))
        );
        assert_eq!(
            crate::stream_control::stream_unavailable_reason(&pending, &pending.streams[0]),
            "an earlier command for this stream target is still pending"
        );
        assert_eq!(
            crate::stream_control::stream_action_text(
                crate::l10n::Locale::ENGLISH,
                StreamAction::Stop(stream_target(40)),
                "RTMP Primary"
            ),
            "Stop stream target \"RTMP Primary\""
        );
        assert!(StreamAction::Start(stream_target(1)).starts());
        assert_eq!(
            StreamAction::Stop(stream_target(9)).target(),
            stream_target(9)
        );
    }

    fn stinger_state(stingers: Vec<fm_ui_model::StingerStatus>) -> StudioUiState {
        let mut view = recording_view([false, false]);
        view.inputs = vec![input(1), input(2)];
        view.input_names = vec!["Camera".to_owned(), "Clip".to_owned()];
        view.switcher = switcher((1, 2), (1, 2));
        view.stingers = stingers;
        StudioUiState::new(StudioConnectionStatus::Ready)
            .with_view(view)
            .with_switcher_permissions(true, true)
    }

    fn configured_slot(slot: u8, media: u128) -> fm_ui_model::StingerStatus {
        fm_ui_model::StingerStatus {
            slot,
            media_input: input(media),
            preload: true,
            cut_point_frames: 10,
            audio_policy: fm_protocol::StingerAudioPolicy::StingerOnly,
            missing_media_fallback: fm_protocol::StingerMissingMediaFallback::Fade,
            readiness: StingerReadiness::Ready,
        }
    }

    const S3: WireStingerSlotId = WireStingerSlotId::new(3).expect("slot three exists");

    #[test]
    fn stinger_configuration_requires_transition_gates_and_known_media() {
        let state = stinger_state(Vec::new());
        assert!(stinger_configuration_available(&state));
        assert_eq!(default_stinger_cut_point_frames(30), 15);
        assert_eq!(
            stinger_assign_intent(&state, S3, input(2), 30),
            Some(StudioIntent::AssignStingerMedia {
                slot: S3,
                media_input: input(2),
                default_cut_point_frames: 15,
            })
        );
        // Media that is not in the replicated view is never offered.
        assert_eq!(stinger_assign_intent(&state, S3, input(9), 30), None);
        let mut no_permission = state.clone();
        no_permission.can_transition = false;
        let mut not_ready = state.clone();
        not_ready.connection_status = StudioConnectionStatus::Synchronizing;
        let mut manual = state;
        if let Some(view) = manual.view.as_mut() {
            view.switcher.desired_manual_transition =
                ManualTransitionStatus::Active(ActiveManualTransition {
                    kind: ManualTransitionKind::Fade,
                    from: input(1),
                    to: input(2),
                    interval_start: ManualTransitionPosition::START,
                    position: ManualTransitionPosition::START,
                });
        }
        for blocked in [no_permission, not_ready, manual] {
            assert!(!stinger_configuration_available(&blocked));
            assert_eq!(stinger_assign_intent(&blocked, S3, input(2), 30), None);
        }
    }

    #[test]
    fn stinger_clear_requires_a_configured_slot() {
        let state = stinger_state(vec![configured_slot(3, 2)]);
        assert_eq!(
            stinger_remove_intent(&state, S3),
            Some(StudioIntent::RemoveStinger { slot: S3 })
        );
        assert_eq!(
            stinger_remove_intent(&state, WireStingerSlotId::new(4).unwrap()),
            None
        );
        let mut no_permission = state;
        no_permission.can_transition = false;
        assert_eq!(stinger_remove_intent(&no_permission, S3), None);
    }

    #[test]
    fn project_padlock_blocks_stinger_setup_but_names_its_reason() {
        let mut state = stinger_state(vec![configured_slot(3, 2)]);
        assert_eq!(stinger_setup_block_reason(&state), None);
        if let Some(view) = state.view.as_mut() {
            view.edit_locks.project = true;
        }
        // The session gates still pass; only the padlock blocks setup.
        assert!(stinger_configuration_available(&state));
        assert_eq!(
            stinger_setup_block_reason(&state),
            Some("the project is locked against edits")
        );
        assert_eq!(stinger_assign_intent(&state, S3, input(2), 30), None);
        assert_eq!(stinger_remove_intent(&state, S3), None);
        // A session gate is reported before the padlock.
        state.can_transition = false;
        assert_eq!(
            stinger_setup_block_reason(&state),
            Some("this session lacks the transition permission")
        );
    }

    #[test]
    fn stinger_editor_media_prefers_choice_then_slot_media_then_preview() {
        use crate::stinger_config::{StingerEditor, stinger_editor_media, stinger_summary};
        let state = stinger_state(vec![configured_slot(3, 1)]);
        let view = state.view.as_ref().unwrap();
        let editor = |slot, media| StingerEditor { slot, media };
        // Unconfigured slot without a choice: the desired Preview input.
        assert_eq!(stinger_editor_media(view, editor(1, None)), Some(input(2)));
        // Configured slot without a choice: its current media.
        assert_eq!(stinger_editor_media(view, editor(3, None)), Some(input(1)));
        // An explicit choice wins while it still exists.
        assert_eq!(
            stinger_editor_media(view, editor(3, Some(input(2)))),
            Some(input(2))
        );
        assert_eq!(
            stinger_editor_media(view, editor(3, Some(input(9)))),
            Some(input(1))
        );
        let en = crate::l10n::Locale::ENGLISH;
        assert_eq!(
            stinger_summary(en, view, 3),
            "S3: Camera · cut point 10 fr · preload yes · audio stinger only · fallback fade"
        );
        assert_eq!(stinger_summary(en, view, 4), "S4 is not configured");
        let spanish = crate::l10n::Locale::new(fm_ui_model::i18n::Language::Spanish);
        assert_eq!(
            stinger_summary(spanish, view, 3),
            "S3: Camera · punto de corte 10 fr · precarga sí · audio solo stinger · \
             alternativa fundido"
        );
    }

    #[test]
    fn stinger_details_show_replicated_settings_or_assign_defaults() {
        use crate::stinger_config::stinger_details;
        let state = stinger_state(vec![configured_slot(3, 1)]);
        let view = state.view.as_ref().unwrap();
        assert_eq!(
            stinger_details(crate::l10n::Locale::ENGLISH, view, 3, 30),
            [
                ("Cut point", "10 fr".to_owned()),
                ("Preload", "yes".to_owned()),
                ("Audio", "stinger only".to_owned()),
                ("Fallback", "fade".to_owned()),
            ]
        );
        // An empty slot lists exactly what ASSIGN would configure.
        assert_eq!(
            stinger_details(crate::l10n::Locale::ENGLISH, view, 4, 30),
            [
                ("Cut point", "15 fr (default)".to_owned()),
                ("Preload", "yes (default)".to_owned()),
                ("Audio", "mixed with Program (default)".to_owned()),
                ("Fallback", "cut (default)".to_owned()),
            ]
        );
    }

    #[test]
    fn stinger_cards_fire_only_when_ready_and_allowed_and_otherwise_configure() {
        use crate::panels::{StingerCardClick, stinger_card_click, stinger_slot_tone};
        use crate::theme::Tone;
        assert_eq!(stinger_card_click(true, true), StingerCardClick::Fire);
        assert_eq!(stinger_card_click(true, false), StingerCardClick::Configure);
        assert_eq!(stinger_card_click(false, true), StingerCardClick::Configure);
        for (state, tone) in [
            (StingerSlotState::Unavailable, Tone::Idle),
            (StingerSlotState::Unconfigured, Tone::Idle),
            (
                StingerSlotState::Readiness(StingerReadiness::Ready),
                Tone::Ready,
            ),
            (
                StingerSlotState::Readiness(StingerReadiness::NotRequested),
                Tone::Pending,
            ),
            (
                StingerSlotState::Readiness(StingerReadiness::Missing),
                Tone::Fault,
            ),
        ] {
            assert_eq!(stinger_slot_tone(state), tone, "{state:?}");
            // The lamp tone and the presentation color always agree.
            assert_eq!(
                tone.color(),
                stinger_slot_presentation(state).color,
                "{state:?}"
            );
        }
    }

    #[test]
    fn stream_rows_report_desired_only_without_samples_and_counters_with_samples() {
        let en = Locale::ENGLISH;
        let idle = stream_roster_entry(40, "RTMP Primary", true);
        assert_eq!(stream_row_text(en, None), "No transport sample yet");

        let sample = stream_sample(40, Some("auth_rejected"));
        assert_eq!(
            stream_row_text(en, Some(&sample)),
            "Transport: connected · 2.0 KB sent · 12 queued · 3 dropped\nFailure: auth_rejected"
        );

        let clean = StreamStatusSample {
            failure: None,
            ..sample
        };
        assert!(!stream_row_text(en, Some(&clean)).contains('\n'));

        // Local HLS output health adds one line after the counters.
        let local = StreamStatusSample {
            local: Some(fm_protocol::StreamLocalStatus {
                segments: 6,
                target_duration_ms: 6_000,
                free_bytes: 1_073_741_824,
            }),
            ..clean
        };
        assert_eq!(
            stream_row_text(en, Some(&local)),
            "Transport: connected · 2.0 KB sent · 12 queued · 3 dropped\n\
             Local HLS: 6 segments of 6.0 s · 1.0 GB free"
        );
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        assert_eq!(
            stream_row_text(spanish, Some(&local)),
            "Transporte: conectado · 2,0 KB enviados · 12 en cola · 3 descartados\n\
             HLS local: 6 segmentos de 6,0 s · 1,0 GB libres"
        );

        // Samples for targets outside the roster never leak into other rows.
        let status = StreamStatusMessage {
            server: fm_protocol::ServerIdentity {
                engine_id: "e".to_owned(),
                project_id: "1".to_owned(),
                state_epoch: 1,
                log_id: "l".to_owned(),
            },
            sequence: 7,
            samples: vec![stream_sample(99, None)],
        };
        assert_eq!(latest_stream_sample(Some(&status), idle.target), None);
        assert_eq!(
            latest_stream_sample(Some(&status), NonZeroU128::new(99).unwrap())
                .map(|sample| sample.muxed_bytes),
            Some(2048)
        );
        assert_eq!(latest_stream_sample(None, idle.target), None);
    }

    #[test]
    fn stinger_slot_presentation_uses_replicated_readiness() {
        assert_eq!(stinger_slot_state(None, 1), StingerSlotState::Unavailable);
        assert_eq!(
            stinger_slot_state(Some(&[]), 1),
            StingerSlotState::Unconfigured
        );
        for (state, label, color, ready) in [
            (
                StingerSlotState::Unavailable,
                "STATE UNAVAILABLE",
                muted_color(),
                false,
            ),
            (
                StingerSlotState::Unconfigured,
                "UNCONFIGURED",
                muted_color(),
                false,
            ),
            (
                StingerSlotState::Readiness(StingerReadiness::NotRequested),
                "NOT REQUESTED",
                AMBER,
                false,
            ),
            (
                StingerSlotState::Readiness(StingerReadiness::Ready),
                "READY",
                PREVIEW,
                true,
            ),
            (
                StingerSlotState::Readiness(StingerReadiness::Missing),
                "MISSING",
                ERROR,
                false,
            ),
        ] {
            assert_eq!(
                stinger_slot_presentation(state),
                StingerSlotPresentation {
                    label,
                    color,
                    ready,
                }
            );
        }
    }

    #[test]
    fn manual_t_bar_availability_requires_ready_view_permission_and_active_state() {
        let available = ManualTransitionGate {
            connection_status: StudioConnectionStatus::Ready,
            has_view: true,
            can_transition: true,
            manual_transition_in_flight: false,
        };
        assert_eq!(
            manual_transition_availability(available, false),
            ManualTransitionAvailability {
                start: true,
                alpha_fade_start_exposed: true,
                active_controls: false,
            }
        );
        assert_eq!(
            manual_transition_availability(available, true),
            ManualTransitionAvailability {
                start: true,
                alpha_fade_start_exposed: true,
                active_controls: true,
            }
        );
        for gate in [
            ManualTransitionGate {
                connection_status: StudioConnectionStatus::Connecting,
                ..available
            },
            ManualTransitionGate {
                has_view: false,
                ..available
            },
            ManualTransitionGate {
                can_transition: false,
                ..available
            },
        ] {
            assert_eq!(
                manual_transition_availability(gate, true),
                ManualTransitionAvailability {
                    start: false,
                    alpha_fade_start_exposed: true,
                    active_controls: false,
                }
            );
        }
    }

    #[test]
    fn manual_t_bar_labels_and_positions_come_from_replicated_model() {
        let active = ManualTransitionStatus::Active(ActiveManualTransition {
            kind: ManualTransitionKind::Wipe,
            from: input(1),
            to: input(2),
            interval_start: ManualTransitionPosition::new(8_000).unwrap(),
            position: ManualTransitionPosition::new(2_500).unwrap(),
        });
        assert_eq!(desired_manual_position(active).basis_points(), 2_500);
        let en = crate::l10n::Locale::ENGLISH;
        assert_eq!(manual_transition_label(en, active), "WIPE 1->2 @ 2500 BP");
        let alpha_fade = ManualTransitionStatus::Active(ActiveManualTransition {
            kind: ManualTransitionKind::AlphaFade,
            from: input(1),
            to: input(2),
            interval_start: ManualTransitionPosition::new(8_000).unwrap(),
            position: ManualTransitionPosition::new(2_500).unwrap(),
        });
        assert_eq!(
            manual_transition_label(en, alpha_fade),
            "ALPHA FADE 1->2 @ 2500 BP"
        );
        assert_eq!(
            desired_manual_position(ManualTransitionStatus::Inactive),
            ManualTransitionPosition::START
        );
        assert_eq!(
            manual_transition_label(en, ManualTransitionStatus::Inactive),
            "INACTIVE"
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn intents_are_typed_and_comparable() {
        assert_eq!(
            StudioIntent::SetInputAudioStrip {
                input: input(9),
                update: InputAudioStripUpdate {
                    gain_millidb: Some(-6_000),
                    balance_basis_points: Some(2_500),
                    muted: Some(true),
                    soloed: Some(true),
                    follow_video: Some(false),
                    delay_samples: Some(2_400),
                },
            },
            StudioIntent::SetInputAudioStrip {
                input: input(9),
                update: InputAudioStripUpdate {
                    gain_millidb: Some(-6_000),
                    balance_basis_points: Some(2_500),
                    muted: Some(true),
                    soloed: Some(true),
                    follow_video: Some(false),
                    delay_samples: Some(2_400),
                },
            }
        );
        assert_eq!(
            StudioIntent::SelectPreview(input(9)),
            StudioIntent::SelectPreview(input(9))
        );
        assert_ne!(StudioIntent::Cut, StudioIntent::Fade { duration_frames: 1 });
        assert_ne!(
            StudioIntent::Fade {
                duration_frames: 30
            },
            StudioIntent::AlphaFade {
                duration_frames: 30
            }
        );
        assert_ne!(
            StudioIntent::AlphaFade {
                duration_frames: 30
            },
            StudioIntent::Slide {
                duration_frames: 30
            }
        );
        assert_ne!(
            StudioIntent::Slide {
                duration_frames: 30
            },
            StudioIntent::Zoom {
                duration_frames: 30
            }
        );
        assert_ne!(
            StudioIntent::Zoom {
                duration_frames: 30
            },
            StudioIntent::Stinger {
                slot: WireStingerSlotId::new(1).unwrap(),
                duration_frames: 30
            }
        );
        assert_ne!(
            StudioIntent::Stinger {
                slot: WireStingerSlotId::new(1).unwrap(),
                duration_frames: 30
            },
            StudioIntent::Wipe {
                duration_frames: 30
            }
        );
        assert_ne!(
            StudioIntent::FadeToBlack {
                active: true,
                duration_frames: 30,
            },
            StudioIntent::FadeToBlack {
                active: false,
                duration_frames: 30,
            }
        );
        assert_eq!(
            StudioIntent::SetManualTransitionPosition {
                position: ManualTransitionPosition::START,
            },
            StudioIntent::SetManualTransitionPosition {
                position: ManualTransitionPosition::new(0).unwrap(),
            }
        );
        assert_ne!(
            StudioIntent::SetManualTransitionPosition {
                position: ManualTransitionPosition::END,
            },
            StudioIntent::SetManualTransitionPosition {
                position: ManualTransitionPosition::new(2_500).unwrap(),
            }
        );
        assert_eq!(
            StudioIntent::Fade {
                duration_frames: 30
            },
            StudioIntent::Fade {
                duration_frames: 30
            }
        );
    }

    #[test]
    fn draw_smoke_test_without_client_state() {
        let context = egui::Context::default();
        let state = StudioUiState::new(StudioConnectionStatus::Launching);
        let mut shell = StudioShell::default();
        context.begin_pass(egui::RawInput::default());
        let mut ui = egui::Ui::new(
            context.clone(),
            egui::Id::new("fm-ui-egui-smoke-test"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        let intents = shell.draw(&mut ui, &state);
        let _output = context.end_pass();
        assert_eq!(intents, Vec::new());
    }

    #[test]
    fn overlay_update_availability_uses_desired_active_channel_and_preview_source() {
        let channel = WireOverlayChannelId::new(3).unwrap();
        let preview = input(42);
        let desired_active = overlay_status(true);
        let realized_only_active = (overlay_status(false), overlay_status(true));
        let desired_active_realized_inactive = (overlay_status(true), overlay_status(false));
        assert!(realized_only_active.1.active);
        assert!(!desired_active_realized_inactive.1.active);
        let enabled = |status: StudioConnectionStatus, has_view: bool, can_transition: bool| {
            status.controls_enabled() && has_view && can_transition
        };
        let ready = enabled(StudioConnectionStatus::Ready, true, true);

        assert_eq!(
            overlay_update_intent(ready, channel, Some(&desired_active), Some(preview)),
            Some(StudioIntent::UpdateOverlay {
                channel,
                source: preview,
            })
        );
        // A realized-only active channel must not enable UPDATE.
        assert!(
            overlay_update_intent(ready, channel, Some(&realized_only_active.0), Some(preview),)
                .is_none()
        );
        assert!(overlay_update_intent(ready, channel, Some(&desired_active), None,).is_none());
        assert!(
            overlay_update_intent(
                enabled(StudioConnectionStatus::Ready, false, true),
                channel,
                Some(&desired_active),
                Some(preview),
            )
            .is_none()
        );
        assert!(
            overlay_update_intent(
                enabled(StudioConnectionStatus::Ready, true, false),
                channel,
                Some(&desired_active),
                Some(preview),
            )
            .is_none()
        );
        assert!(
            overlay_update_intent(
                enabled(StudioConnectionStatus::Connecting, true, true),
                channel,
                Some(&desired_active_realized_inactive.0),
                Some(preview),
            )
            .is_none()
        );
        // A desired active channel remains allowed while realized state is inactive.
        assert_eq!(
            overlay_update_intent(
                ready,
                channel,
                Some(&desired_active_realized_inactive.0),
                Some(preview),
            ),
            Some(StudioIntent::UpdateOverlay {
                channel,
                source: preview
            })
        );
    }

    fn overlay_status(active: bool) -> fm_ui_model::OverlayStatus {
        fm_ui_model::OverlayStatus {
            channel: 3,
            source: Some(input(7)),
            active,
            opacity: 255,
            transition: OverlayTransitionKind::Cut,
            duration_frames: 1,
            position: OverlayPositionPreset::FullFrame,
            border: OverlayBorderPreset::None,
            queued_sources: Vec::new(),
            included_outputs: Vec::new(),
        }
    }

    fn wire(value: u128) -> fm_protocol::WireInputId {
        fm_protocol::WireInputId::new(NonZeroU128::new(value).unwrap())
    }

    fn monitor_image(
        tap: MonitorTap,
        origin: MonitorFrameOrigin,
        input: fm_protocol::WireInputId,
        (width, height): (u16, u16),
    ) -> MonitorImage {
        let pixels = usize::from(width) * usize::from(height);
        MonitorImage {
            tap,
            sequence: 1,
            frame: 7,
            input,
            origin,
            width,
            height,
            rgba: std::iter::repeat_n([200_u8, 40, 30, 255], pixels)
                .flatten()
                .collect::<Vec<_>>()
                .into(),
        }
    }

    fn facts(origin: MonitorFrameOrigin, input: u128, age_ms: u64) -> crate::monitors::FrameFacts {
        crate::monitors::FrameFacts {
            origin,
            input: wire(input),
            width: 640,
            height: 360,
            age: std::time::Duration::from_millis(age_ms),
        }
    }

    #[test]
    fn monitor_wells_follow_the_canvas_shape_and_keep_it_while_frames_lapse() {
        use crate::monitors::{DEFAULT_CANVAS, canvas_shape, monitor_video_height};
        let now = std::time::Instant::now();
        let image = |tap, size| {
            Some((
                monitor_image(tap, MonitorFrameOrigin::Simulated, wire(1), size),
                now,
            ))
        };
        // No frame yet: the default 16:9 shape.
        assert_eq!(canvas_shape(&[None, None], DEFAULT_CANVAS), (16, 9));
        // Program wins over Preview; Preview stands in when Program is absent.
        let portrait = [
            image(MonitorTap::Preview, (640, 360)),
            image(MonitorTap::Program, (202, 360)),
        ];
        assert_eq!(canvas_shape(&portrait, DEFAULT_CANVAS), (202, 360));
        let preview_only = [image(MonitorTap::Preview, (360, 360)), None];
        assert_eq!(canvas_shape(&preview_only, DEFAULT_CANVAS), (360, 360));
        // Frames lapse: the last shape is kept so the layout does not jump.
        assert_eq!(canvas_shape(&[None, None], (202, 360)), (202, 360));

        // 16:9 always fills the width, whatever the budget.
        assert!((monitor_video_height(640.0, (16, 9), 900.0) - 360.0).abs() < f32::EPSILON);
        assert!((monitor_video_height(640.0, (1920, 1080), 100.0) - 360.0).abs() < f32::EPSILON);
        // Portrait grows into the height budget instead of a 16:9 box...
        assert!((monitor_video_height(640.0, (1080, 1920), 540.0) - 540.0).abs() < f32::EPSILON);
        // ...but never below the 16:9 height, and never beyond its own shape.
        assert!((monitor_video_height(640.0, (1080, 1920), 100.0) - 360.0).abs() < f32::EPSILON);
        assert!((monitor_video_height(640.0, (1, 1), 5_000.0) - 640.0).abs() < f32::EPSILON);
        // Wider canvases get a shorter well; a zero shape falls back to 16:9.
        assert!((monitor_video_height(640.0, (64, 27), 900.0) - 270.0).abs() < f32::EPSILON);
        assert!((monitor_video_height(640.0, (0, 0), 900.0) - 360.0).abs() < f32::EPSILON);
    }

    #[test]
    fn letterbox_fits_and_centers_every_aspect() {
        use crate::monitors::letterbox;
        let video = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), Vec2::new(320.0, 180.0));
        // 16:9 fills the well exactly.
        assert_eq!(letterbox(video, 640, 360), video);
        // 4:3 pillarboxes: full height, centered horizontally.
        let pillar = letterbox(video, 480, 360);
        assert_eq!(pillar.size(), Vec2::new(240.0, 180.0));
        assert_eq!(pillar.center(), video.center());
        assert_eq!(pillar.min.x, 50.0);
        // 9:16 portrait is a narrow centered column.
        let portrait = letterbox(video, 202, 360);
        assert!((portrait.height() - 180.0).abs() < 1e-3);
        assert!((portrait.width() - 101.0).abs() < 1e-3);
        assert_eq!(portrait.center(), video.center());
        // A wider-than-16:9 image letterboxes top and bottom.
        let wide = letterbox(video, 640, 200);
        assert_eq!(wide.width(), 320.0);
        assert!((wide.height() - 100.0).abs() < 1e-3);
        assert_eq!(wide.center(), video.center());
        // Degenerate sizes never produce NaN rectangles.
        assert_eq!(letterbox(video, 0, 360), video);
    }

    #[test]
    fn picture_visibility_requires_fresh_frames_of_the_realized_source() {
        use crate::monitors::picture_visible;
        use std::time::Duration;
        let fresh = Duration::from_millis(999);
        let stale = Duration::from_secs(1);
        let cases = [
            (MonitorFrameOrigin::Simulated, 1, Some(1), fresh, true),
            (MonitorFrameOrigin::Simulated, 1, Some(2), fresh, false),
            (MonitorFrameOrigin::Simulated, 1, None, fresh, false),
            (MonitorFrameOrigin::Simulated, 1, Some(1), stale, false),
            (MonitorFrameOrigin::Native, 1, Some(2), fresh, true),
            (MonitorFrameOrigin::Native, 1, None, fresh, true),
            (MonitorFrameOrigin::Native, 1, Some(1), stale, false),
        ];
        for (origin, frame, realized, age, expected) in cases {
            assert_eq!(
                picture_visible(origin, wire(frame), realized.map(wire), age),
                expected,
                "{origin:?} frame={frame} realized={realized:?} age={age:?}"
            );
        }
    }

    #[test]
    fn monitor_captions_state_origin_size_staleness_and_absence() {
        use crate::monitors::{Bus, PictureMode, monitor_caption};
        let caption_of = |bus, frame, native_runtime| {
            monitor_caption(Locale::ENGLISH, bus, frame, Some(wire(1)), native_runtime)
        };
        let sim = MonitorFrameOrigin::Simulated;
        let native = MonitorFrameOrigin::Native;
        #[allow(clippy::type_complexity)]
        let cases: [(
            Bus,
            Option<crate::monitors::FrameFacts>,
            bool,
            PictureMode,
            &[&str],
            &str,
        ); 9] = [
            (
                Bus::Program,
                Some(facts(sim, 1, 100)),
                false,
                PictureMode::Live,
                &["SIMULATED"],
                "SIMULATED RENDER · 640×360 encoded preview",
            ),
            (
                Bus::Preview,
                Some(facts(sim, 1, 100)),
                false,
                PictureMode::Live,
                &["SIMULATED"],
                "SIMULATED RENDER · 640×360 encoded preview",
            ),
            (
                Bus::Program,
                Some(facts(native, 9, 100)),
                true,
                PictureMode::Live,
                &[],
                "ENCODED PREVIEW 640×360 · CPU readback",
            ),
            (
                Bus::Program,
                Some(facts(sim, 1, 2_500)),
                false,
                PictureMode::Stale,
                &["SIMULATED", "STALE"],
                "STALE · last encoded frame 2.5 s old",
            ),
            (
                Bus::Program,
                Some(facts(native, 1, 1_000)),
                true,
                PictureMode::Stale,
                &["STALE"],
                "STALE · last encoded frame 1.0 s old",
            ),
            (
                Bus::Preview,
                Some(facts(sim, 2, 100)),
                false,
                PictureMode::Stale,
                &["SIMULATED", "STALE"],
                "STALE · encoded frame does not match the realized source",
            ),
            (
                Bus::Program,
                None,
                false,
                PictureMode::Hidden,
                &[],
                "NO VIDEO FEED · waiting for encoded preview",
            ),
            (
                Bus::Program,
                None,
                true,
                PictureMode::Hidden,
                &[],
                "NO VIDEO FEED · waiting for encoded preview",
            ),
            (
                Bus::Preview,
                None,
                true,
                PictureMode::Hidden,
                &[],
                "NO PREVIEW TAP · native Preview image not produced",
            ),
        ];
        for (bus, frame, native_runtime, picture, badges, text) in cases {
            let caption = caption_of(bus, frame, native_runtime);
            assert_eq!(caption.picture, picture, "{bus:?} {frame:?}");
            assert_eq!(caption.badges, badges, "{bus:?} {frame:?}");
            assert_eq!(caption.text, text, "{bus:?} {frame:?}");
            assert_ne!(caption.note, "");
        }
        // A simulated frame says what it composites, and never FTB.
        let note = caption_of(Bus::Program, Some(facts(sim, 1, 0)), false).note;
        for part in ["scene layers", "overlays", "Fade to Black is not"] {
            assert!(note.contains(part), "{note}");
        }
    }

    fn run_frame(
        context: &egui::Context,
        shell: &mut StudioShell,
        state: &StudioUiState,
    ) -> egui::FullOutput {
        context.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1600.0, 1000.0),
            )),
            ..egui::RawInput::default()
        });
        let mut ui = egui::Ui::new(
            context.clone(),
            egui::Id::new("fm-ui-egui-monitor-test"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        let _intents = shell.draw(&mut ui, state);
        context.end_pass()
    }

    /// Every string painted in one frame's output.
    fn painted_text(output: &egui::FullOutput) -> Vec<String> {
        fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn the_console_draws_in_the_chosen_language_from_the_next_frame() {
        use fm_ui_model::i18n::Language;
        let context = egui::Context::default();
        let state = recording_state([true, false], false);
        let mut shell = StudioShell::default();
        assert_eq!(shell.locale(), Locale::ENGLISH);
        let english = painted_text(&run_frame(&context, &mut shell, &state));
        for expected in ["File", "Inputs", "INPUT BANK", "CONTROL READY", "REC"] {
            assert!(
                english.iter().any(|text| text == expected),
                "missing {expected:?} in {english:?}"
            );
        }
        shell.set_locale(Locale::new(Language::Spanish));
        let spanish = painted_text(&run_frame(&context, &mut shell, &state));
        for expected in [
            "Archivo",
            "Entradas",
            "Ayuda",
            "BANCO DE ENTRADAS",
            "CONTROL LISTO",
            "DESBLOQUEADO",
        ] {
            assert!(
                spanish.iter().any(|text| text == expected),
                "missing {expected:?} in {spanish:?}"
            );
        }
        for english_only in ["File", "Help", "CONTROL READY", "UNLOCKED"] {
            assert!(
                !spanish.iter().any(|text| text == english_only),
                "{english_only:?} still painted in Spanish: {spanish:?}"
            );
        }
        // The language is presentation only: the same state yields the same
        // (empty) intents in both languages.
        assert_eq!(l10n::locale(&context).language, Language::Spanish);
    }

    #[test]
    fn monitor_textures_upload_once_per_frame_buffer_and_free_on_absence() {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        let now = std::time::Instant::now();
        let first = monitor_image(
            MonitorTap::Program,
            MonitorFrameOrigin::Simulated,
            wire(1),
            (64, 36),
        );
        state.monitor_images[MonitorTap::Program.index()] = Some((first.clone(), now));

        let output = run_frame(&context, &mut shell, &state);
        let program = shell
            .monitor_textures
            .texture_id(MonitorTap::Program)
            .expect("program texture uploaded");
        assert_eq!(shell.monitor_textures.texture_id(MonitorTap::Preview), None);
        let uploads = |output: &egui::FullOutput| {
            output
                .textures_delta
                .set
                .iter()
                .filter(|(id, _)| *id == program)
                .count()
        };
        assert_eq!(uploads(&output), 1);
        assert_eq!(output.textures_delta.free, Vec::<egui::TextureId>::new());

        // The same Arc (even inside a cloned state) is never re-uploaded.
        let output = run_frame(&context, &mut shell, &state.clone());
        assert_eq!(uploads(&output), 0);
        assert_eq!(
            shell.monitor_textures.texture_id(MonitorTap::Program),
            Some(program)
        );

        // Equal pixels in a new Arc are a new frame: one in-place upload.
        let mut second = first.clone();
        second.sequence = 2;
        second.rgba = first.rgba.to_vec().into();
        state.monitor_images[MonitorTap::Program.index()] = Some((second, now));
        let output = run_frame(&context, &mut shell, &state);
        assert_eq!(uploads(&output), 1);
        assert_eq!(
            shell.monitor_textures.texture_id(MonitorTap::Program),
            Some(program)
        );

        // No frame for the tap frees its texture.
        state.monitor_images[MonitorTap::Program.index()] = None;
        let output = run_frame(&context, &mut shell, &state);
        assert_eq!(shell.monitor_textures.texture_id(MonitorTap::Program), None);
        assert!(output.textures_delta.free.contains(&program));
        assert_eq!(uploads(&output), 0);

        // A malformed buffer is never uploaded.
        let mut malformed = first;
        malformed.rgba = vec![0_u8; 3].into();
        state.monitor_images[MonitorTap::Program.index()] = Some((malformed, now));
        let _output = run_frame(&context, &mut shell, &state);
        assert_eq!(shell.monitor_textures.texture_id(MonitorTap::Program), None);
    }

    #[test]
    fn monitors_draw_live_stale_and_absent_frames_without_panicking() {
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        let view = recording_view([false, false]);
        let program = fm_protocol::WireInputId::from_domain(view.switcher.realized.program);
        let preview = fm_protocol::WireInputId::from_domain(view.switcher.realized.preview);
        let now = std::time::Instant::now();
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready).with_view(view);
        state.monitor_images = [
            Some((
                monitor_image(
                    MonitorTap::Preview,
                    MonitorFrameOrigin::Simulated,
                    preview,
                    (48, 36),
                ),
                now.checked_sub(std::time::Duration::from_secs(3))
                    .unwrap_or(now),
            )),
            Some((
                monitor_image(
                    MonitorTap::Program,
                    MonitorFrameOrigin::Simulated,
                    program,
                    (64, 36),
                ),
                now,
            )),
        ];
        let output = run_frame(&context, &mut shell, &state);
        assert!(output.textures_delta.set.len() >= 2);
        // A frame on screen keeps the UI repainting so it can age into STALE.
        let repaint = output
            .viewport_output
            .values()
            .map(|viewport| viewport.repaint_delay)
            .min()
            .unwrap();
        assert!(repaint <= crate::monitors::MONITOR_REPAINT_INTERVAL);

        state.monitor_images[MonitorTap::Program.index()] = Some((
            monitor_image(
                MonitorTap::Program,
                MonitorFrameOrigin::Native,
                program,
                (64, 36),
            ),
            now,
        ));
        state.monitor_images[MonitorTap::Preview.index()] = None;
        let _output = run_frame(&context, &mut shell, &state);
    }

    fn run_timed_frame(
        context: &egui::Context,
        shell: &mut StudioShell,
        state: &StudioUiState,
        time: f64,
        events: Vec<egui::Event>,
    ) -> Vec<StudioIntent> {
        context.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1600.0, 1000.0),
            )),
            time: Some(time),
            events,
            ..egui::RawInput::default()
        });
        let mut ui = egui::Ui::new(
            context.clone(),
            egui::Id::new("fm-ui-egui-timed-test"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        let intents = shell.draw(&mut ui, state);
        let _output = context.end_pass();
        intents
    }

    #[test]
    fn toasts_expire_by_wall_clock_time_between_frames() {
        // Studio repaints on timers (`request_repaint_after`), so egui's
        // `stable_dt` stays at its 1/60 s prediction however long the gap
        // between frames really is. Toasts and meter ballistics must age by
        // the real time between frames.
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        state.notice = Some("saved".to_owned());
        let mut time = 10.0;
        for _ in 0..3 {
            run_timed_frame(&context, &mut shell, &state, time, Vec::new());
            time += 0.1;
        }
        assert_eq!(shell.toasts.iter().count(), 1);
        // Six seconds of 100 ms repaint ticks outlive the 5 s notice.
        for _ in 0..60 {
            run_timed_frame(&context, &mut shell, &state, time, Vec::new());
            time += 0.1;
        }
        assert!(shell.toasts.is_empty(), "{:?}", shell.toasts);
        // A message arriving after a long idle gap keeps its full lifetime.
        state.notice = Some("rejected".to_owned());
        run_timed_frame(&context, &mut shell, &state, time + 30.0, Vec::new());
        assert_eq!(
            shell
                .toasts
                .iter()
                .map(|toast| toast.remaining_ms)
                .collect::<Vec<_>>(),
            [crate::toasts::NOTICE_TOAST_MS]
        );
    }

    #[test]
    fn console_keys_do_not_act_behind_an_open_help_dialog() {
        // Esc closes the Help dialogs; it must not also cancel a live manual
        // transition, and switching keys must not act on the hidden console.
        let state = StudioUiState::new(StudioConnectionStatus::Ready);
        let manual = ManualTransitionAvailability {
            start: false,
            alpha_fade_start_exposed: true,
            active_controls: true,
        };
        let escape = || egui::Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let run = |shell: &StudioShell| {
            // A fresh context per run, so each Esc is a new press.
            let context = egui::Context::default();
            context.begin_pass(egui::RawInput {
                events: vec![escape()],
                ..Default::default()
            });
            let mut ui = egui::Ui::new(
                context.clone(),
                egui::Id::new("studio-modal-keys"),
                egui::UiBuilder::new()
                    .layer_id(egui::LayerId::background())
                    .max_rect(context.viewport_rect()),
            );
            let intents = shell.keyboard_intents(&mut ui, &state, manual);
            let remaining = context.input(|input| input.events.len());
            drop(ui);
            let _ = context.end_pass();
            (intents, remaining)
        };
        let mut shell = StudioShell::default();
        assert_eq!(run(&shell), (vec![StudioIntent::CancelManualTransition], 0));
        shell.show_shortcuts = true;
        assert_eq!(run(&shell), (Vec::new(), 1));
        shell.show_shortcuts = false;
        shell.show_about = true;
        assert_eq!(run(&shell), (Vec::new(), 1));
    }
}
