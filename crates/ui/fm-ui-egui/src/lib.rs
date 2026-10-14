//! Pure `egui` presentation for the native `FreeMix` studio shell.
//!
//! This crate translates immutable client views into operator-facing controls
//! and returns intents. It performs no command dispatch, persistence, or I/O.
//!
//! Drawing is split by console region: `theme` (palette and style),
//! `chrome` (top and status bars), `monitors` (Preview/Program wells),
//! `transitions` (transition column, T-bar, FTB), `input_bank` (source
//! tiles), `mixer` (channel strips), `panels`/`overlays` (side panel),
//! `menu` (menu bar and Help windows), `connection_overlay` (the status card
//! over the monitors), and `toasts` (transient notices). Pure helpers live in
//! `gating` (disabled-control reasons), `meter_ballistics` (peak hold),
//! `stream_control` (per-target stream Start/Stop gating), and
//! `stinger_config` (Stinger slot assignment and clearing).

use egui::{
    Align, CentralPanel, Event, Frame, Key, Layout, Margin, Modifiers, Panel, ScrollArea, Stroke,
    Ui, Vec2,
};
use fm_protocol::{
    AudioMetersMessage, ManualTransitionKind, ManualTransitionPosition, StreamStatusMessage,
    WireOverlayChannelId, WireRecorderSlot, WireStingerSlotId, WireStreamTargetId,
};
use fm_types::{InputId, OutputId};
use fm_ui_model::{ClientView, ManualTransitionStatus, StreamStatus, SwitcherState};

mod chrome;
mod connection_overlay;
mod fade_to_black;
mod gating;
mod input_bank;
mod menu;
mod meter_ballistics;
mod mixer;
mod monitors;
mod overlay_status;
mod overlays;
mod panels;
mod stinger_config;
mod stream_control;
mod theme;
mod toasts;
mod transitions;

pub use fade_to_black::{FadeToBlackAvailability, FadeToBlackGate, fade_to_black_availability};
pub use menu::ConsoleLayout;
pub use stinger_config::{
    default_stinger_cut_point_frames, stinger_assign_intent, stinger_configuration_available,
    stinger_remove_intent,
};
pub use stream_control::{StreamAction, stream_action, stream_command_pending};

use panels::SideTab;
use theme::{BORDER, GRAPHITE, PANEL};

const NARROW_MONITOR_WIDTH: f32 = 700.0;
const SIDE_PANEL_WIDTH: f32 = 370.0;
const MIN_LOWER_ROW_HEIGHT: f32 = 340.0;

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
            Self::Launching => "LAUNCHING",
            Self::Connecting => "CONNECTING",
            Self::Synchronizing => "SYNCHRONIZING",
            Self::Ready => "CONTROL READY",
            Self::Backoff => "RETRY BACKOFF",
            Self::Disconnected => "DISCONNECTED",
            Self::Failed => "FAILED",
            Self::ProtocolMismatch => "PROTOCOL MISMATCH",
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
    InputBank,
}

impl ShortcutGroup {
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Switching => "Switching",
            Self::Transitions => "Transitions",
            Self::InputBank => "Input bank (pointer and focus)",
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
pub(crate) const STUDIO_SHORTCUTS: [ShortcutEntry; 9] = [
    ShortcutEntry {
        group: ShortcutGroup::Switching,
        keys: "1 – 8",
        action: "Select input 1–8 on Preview",
        key: Some(Key::Num1),
    },
    ShortcutEntry {
        group: ShortcutGroup::Switching,
        keys: "C",
        action: "Cut Preview to Program",
        key: Some(Key::C),
    },
    ShortcutEntry {
        group: ShortcutGroup::Transitions,
        keys: "F",
        action: "Fade Preview to Program (shared duration)",
        key: Some(Key::F),
    },
    ShortcutEntry {
        group: ShortcutGroup::Transitions,
        keys: "B",
        action: "Toggle Fade-to-Black",
        key: Some(Key::B),
    },
    ShortcutEntry {
        group: ShortcutGroup::Transitions,
        keys: "Esc",
        action: "Cancel the active manual T-bar transition",
        key: Some(Key::Escape),
    },
    ShortcutEntry {
        group: ShortcutGroup::InputBank,
        keys: "Click",
        action: "Select the input on Preview",
        key: None,
    },
    ShortcutEntry {
        group: ShortcutGroup::InputBank,
        keys: "Double-click",
        action: "Select on Preview, then Cut to Program",
        key: None,
    },
    ShortcutEntry {
        group: ShortcutGroup::InputBank,
        keys: "Right-click",
        action: "Input menu: Preview, Cut, Rename, Move",
        key: None,
    },
    ShortcutEntry {
        group: ShortcutGroup::InputBank,
        keys: "Tab · Enter",
        action: "Focus a tile, then select it on Preview",
        key: None,
    },
];

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
    pub fn operator_label(self) -> String {
        let mut labels = Vec::with_capacity(4);
        if self.is_realized_program() {
            labels.push("PROGRAM");
        }
        if self.is_realized_preview() {
            labels.push("PREVIEW");
        }
        if self.is_desired_program() {
            labels.push("PROGRAM DESIRED");
        }
        if self.is_desired_preview() {
            labels.push("PREVIEW DESIRED");
        }
        if labels.is_empty() {
            "NO TALLY".to_owned()
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

    /// Draws one complete shell frame and returns operator intents in UI order.
    ///
    /// The UI must belong to an active `egui` pass, as is customary for
    /// immediate-mode drawing.
    pub fn draw(&mut self, ui: &mut Ui, state: &StudioUiState) -> Vec<StudioIntent> {
        theme::apply_console_visuals(ui.ctx());
        self.set_transition_duration_frames(self.transition_duration_frames);
        self.set_fade_to_black_duration_frames(self.fade_to_black_duration_frames);

        let inputs = state
            .view
            .as_ref()
            .map_or(&[] as &[InputId], |view| view.inputs.as_slice());
        self.reconcile_rename_draft(state);
        let manual_transition_controls = manual_transition_availability(
            ManualTransitionGate::from_state(state),
            state.view.as_ref().is_some_and(|view| {
                matches!(
                    view.switcher.desired_manual_transition,
                    ManualTransitionStatus::Active(_)
                )
            }),
        );
        let mut intents = studio_shortcuts(
            ui,
            TransitionGate::from_state(state),
            state.can_select_preview,
            manual_transition_controls.active_controls,
            inputs,
            self.transition_duration_frames,
            self.fade_to_black_duration_frames,
        );
        let dt_ms = meter_ballistics::frame_dt_ms(ui.input(|input| input.stable_dt));
        self.toasts.observe(state);
        self.toasts.tick(dt_ms);
        let bar_frame = Frame::new()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, BORDER))
            .inner_margin(Margin::symmetric(14, 0));
        Panel::top("studio-menu-bar")
            .exact_size(26.0)
            .frame(
                Frame::new()
                    .fill(theme::GRAPHITE)
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(Margin::symmetric(8, 0)),
            )
            .show(ui, |ui| {
                self.draw_menu_bar(ui, state, manual_transition_controls, &mut intents);
            });
        Panel::top("studio-top-bar")
            .exact_size(46.0)
            .frame(bar_frame)
            .show(ui, |ui| chrome::draw_top_bar(ui, state));
        if self.layout.show_status_bar {
            Panel::bottom("studio-status-bar")
                .exact_size(26.0)
                .frame(bar_frame)
                .show(ui, |ui| chrome::draw_status_bar(ui, state));
        }
        if self.layout.show_side_panel {
            Panel::right("studio-side-panel")
                .resizable(true)
                .default_size(SIDE_PANEL_WIDTH)
                .min_size(300.0)
                .max_size(560.0)
                .frame(
                    Frame::new()
                        .fill(PANEL)
                        .stroke(Stroke::new(1.0, BORDER))
                        .inner_margin(Margin::same(10)),
                )
                .show(ui, |ui| self.draw_side_panel(ui, state, &mut intents));
        }
        let central = CentralPanel::default()
            .frame(Frame::new().fill(GRAPHITE).inner_margin(Margin::same(12)))
            .show(ui, |ui| {
                // Small windows keep every region reachable by scrolling.
                ScrollArea::vertical()
                    .id_salt("studio-console")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let top = ui.cursor().min;
                        self.draw_switcher_row(ui, state, manual_transition_controls, &mut intents);
                        let monitors = egui::Rect::from_min_max(
                            top,
                            egui::pos2(top.x + ui.available_width(), ui.min_rect().max.y),
                        );
                        if let Some(card) = connection_overlay::connection_card(state) {
                            connection_overlay::paint_connection_card(ui, monitors, &card);
                        }
                        if self.layout.show_input_bank || self.layout.show_mixer {
                            ui.add_space(12.0);
                            self.draw_lower_row(ui, state, dt_ms, &mut intents);
                        }
                    });
            });
        self.draw_toasts(ui.ctx(), central.response.rect);
        self.draw_help_windows(ui.ctx());
        intents
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
        intents: &mut Vec<StudioIntent>,
    ) {
        let gap = 12.0;
        let available = ui.available_width();
        let column = transitions::TRANSITION_COLUMN_WIDTH + 2.0 * gap;
        if available >= NARROW_MONITOR_WIDTH {
            let monitor_width = ((available - column) / 2.0).floor();
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                monitors::draw_monitor(
                    ui,
                    monitors::Bus::Preview,
                    state.view.as_ref(),
                    monitor_width,
                );
                ui.vertical(|ui| {
                    transitions::draw_transition_column(ui, self, state, manual, intents);
                });
                monitors::draw_monitor(
                    ui,
                    monitors::Bus::Program,
                    state.view.as_ref(),
                    monitor_width,
                );
            });
        } else {
            let monitor_width = ((available - gap) / 2.0).floor();
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                monitors::draw_monitor(
                    ui,
                    monitors::Bus::Preview,
                    state.view.as_ref(),
                    monitor_width,
                );
                monitors::draw_monitor(
                    ui,
                    monitors::Bus::Program,
                    state.view.as_ref(),
                    monitor_width,
                );
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
            mixer::mixer_width(strips).min(available * 0.55)
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
        recording_status_label, stinger_slot_presentation, stinger_slot_state,
        stream_realized_name, stream_row_text, streams_show_none_configured,
    };
    use crate::theme::{AMBER, ERROR, MUTED, PREVIEW, routing_color};
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
    fn monitor_routing_projects_preview_and_program_names() {
        let inputs = [input(1), input(2), input(3)];
        let names = [
            "Camera A".to_owned(),
            "Camera B".to_owned(),
            "Graphics".to_owned(),
        ];
        let switcher = switcher((3, 2), (1, 2));
        let preview = monitor_routing(switcher, &inputs, &names, |selection| selection.preview);
        let program = monitor_routing(switcher, &inputs, &names, |selection| selection.program);

        assert_eq!(preview.desired, (input(2), "Camera B".to_owned()));
        assert_eq!(preview.realized, (input(2), "Camera B".to_owned()));
        assert_eq!(program.desired, (input(3), "Graphics".to_owned()));
        assert_eq!(program.realized, (input(1), "Camera A".to_owned()));
        assert_eq!(routing_color(preview.desired.0, preview.realized.0), MUTED);
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
        assert_eq!(tally.operator_label(), "PREVIEW / PROGRAM DESIRED");
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
        assert_eq!(STREAMS_NONE_CONFIGURED, "STREAMS | NONE CONFIGURED");
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
        assert_eq!(
            stinger_summary(view, 3),
            "S3: Camera · cut 10 fr · preload · stinger-only audio · fallback fade"
        );
        assert_eq!(stinger_summary(view, 4), "S4 is not configured");
    }

    #[test]
    fn stream_rows_report_desired_only_without_samples_and_counters_with_samples() {
        for (state, expected) in [
            (StreamRealizedState::Stopped, "stopped"),
            (StreamRealizedState::Starting, "starting"),
            (StreamRealizedState::Live, "live"),
            (
                StreamRealizedState::WaitingToReconnect,
                "waiting-to-reconnect",
            ),
            (StreamRealizedState::Congested, "congested"),
            (StreamRealizedState::Failed, "failed"),
            (StreamRealizedState::Unavailable, "unavailable"),
        ] {
            assert_eq!(stream_realized_name(state), expected);
        }

        let idle = stream_roster_entry(40, "RTMP Primary", true);
        assert_eq!(stream_row_text(&idle, None), "RTMP Primary desired=true");

        let sample = stream_sample(40, Some("auth_rejected"));
        assert_eq!(
            stream_row_text(&idle, Some(&sample)),
            "RTMP Primary desired=true realized=live connected=true muxed_bytes=2048 \
             enqueued_pairs=12 dropped_pairs=3 failure=auth_rejected"
        );

        let clean = StreamStatusSample {
            failure: None,
            ..sample
        };
        assert!(stream_row_text(&idle, Some(&clean)).ends_with("failure=none"));

        // Local HLS output health appends one field after the counters; a
        // sample without it keeps the row text above unchanged.
        let local = StreamStatusSample {
            local: Some(fm_protocol::StreamLocalStatus {
                segments: 6,
                target_duration_ms: 6_000,
                free_bytes: 1_073_741_824,
            }),
            ..clean
        };
        assert_eq!(
            stream_row_text(&idle, Some(&local)),
            "RTMP Primary desired=true realized=live connected=true muxed_bytes=2048 \
             enqueued_pairs=12 dropped_pairs=3 failure=none local=6seg/6000ms/1073741824B"
        );

        let stopped = stream_roster_entry(41, "Backup", false);
        assert_eq!(stream_row_text(&stopped, None), "Backup desired=false");

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
                MUTED,
                false,
            ),
            (StingerSlotState::Unconfigured, "UNCONFIGURED", MUTED, false),
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
        assert_eq!(manual_transition_label(active), "WIPE 1->2 @ 2500 BP");
        let alpha_fade = ManualTransitionStatus::Active(ActiveManualTransition {
            kind: ManualTransitionKind::AlphaFade,
            from: input(1),
            to: input(2),
            interval_start: ManualTransitionPosition::new(8_000).unwrap(),
            position: ManualTransitionPosition::new(2_500).unwrap(),
        });
        assert_eq!(
            manual_transition_label(alpha_fade),
            "ALPHA FADE 1->2 @ 2500 BP"
        );
        assert_eq!(
            desired_manual_position(ManualTransitionStatus::Inactive),
            ManualTransitionPosition::START
        );
        assert_eq!(
            manual_transition_label(ManualTransitionStatus::Inactive),
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
}
