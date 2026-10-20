//! Transport-free phone and tablet operator surface: Preview and Program
//! buses, Cut and automatic transitions, Fade to Black, the two program
//! recorders, stream destinations, per-input audio mute, and a button per
//! project shortcut (RC-004), fired with the input selected on the page.
//!
//! Every control derives its state from the replicated [`ClientView`] and the
//! session's granted permissions, and a disabled control always carries an
//! [`OperatorBlock`] whose [`OperatorBlock::reason`] is the text the served
//! `/operator` page shows beside it. The page's JavaScript mirrors this model;
//! the daemon still authorizes every command.

use core::num::NonZeroU128;

use fm_protocol::{
    CommandPayload, FadeToBlackPosition, ShortcutStatus, StreamRealizedState, WireCueAction,
    WireInputId, WireRecorderSlot, WireShortcutAction, WireStreamTargetId,
};
use fm_types::InputId;
use fm_ui_model::ClientView;

/// Permission that gates Preview selection.
pub const OPERATOR_PREVIEW_PERMISSION: &str = "select_preview";
/// Permission that gates Cut, automatic transitions, Fade to Black,
/// recorders, and streams.
pub const OPERATOR_TRANSITION_PERMISSION: &str = "transition";
/// Permission that gates audio mute.
pub const OPERATOR_AUDIO_PERMISSION: &str = "control_audio";
/// Permission a shortcut that sets title text or numbers also needs.
pub const OPERATOR_EDIT_PERMISSION: &str = "edit_project";

/// The permission the command equivalent to one cue action needs (as the
/// daemon's authority classes it): `None` for a wait or delay, which does
/// nothing, and for a script step, whose verbs only the daemon resolves.
#[must_use]
pub const fn cue_action_permission(action: &WireCueAction) -> Option<&'static str> {
    Some(match action {
        WireCueAction::Preview { .. } => OPERATOR_PREVIEW_PERMISSION,
        WireCueAction::TitleText { .. } | WireCueAction::TitleNumber { .. } => {
            OPERATOR_EDIT_PERMISSION
        }
        WireCueAction::AudioGain { .. } | WireCueAction::AudioMute { .. } => {
            OPERATOR_AUDIO_PERMISSION
        }
        WireCueAction::Wait { .. }
        | WireCueAction::Delay { .. }
        | WireCueAction::RunScript { .. } => return None,
        WireCueAction::Take { .. }
        | WireCueAction::OverlayOn { .. }
        | WireCueAction::OverlayOff { .. }
        | WireCueAction::TitleOn { .. }
        | WireCueAction::TitleOff { .. }
        | WireCueAction::FadeToBlack { .. }
        | WireCueAction::PlaylistStart
        | WireCueAction::PlaylistStop
        | WireCueAction::Record { .. }
        | WireCueAction::Stream { .. }
        | WireCueAction::Media { .. }
        | WireCueAction::Page { .. } => OPERATOR_TRANSITION_PERMISSION,
    })
}

/// An automatic Preview-to-Program transition the operator page can take.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperatorTransition {
    Fade,
    AlphaFade,
    Slide,
    Zoom,
    Wipe,
}

impl OperatorTransition {
    /// Every selectable transition, in the order the page lists them.
    pub const ALL: [Self; 5] = [
        Self::Fade,
        Self::AlphaFade,
        Self::Slide,
        Self::Zoom,
        Self::Wipe,
    ];

    /// The wire command payload token the page sends.
    #[must_use]
    pub const fn payload_token(self) -> &'static str {
        match self {
            Self::Fade => "fade",
            Self::AlphaFade => "alpha_fade",
            Self::Slide => "slide",
            Self::Zoom => "zoom",
            Self::Wipe => "wipe",
        }
    }

    /// Visible label of the transition picker entry.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fade => "Fade",
            Self::AlphaFade => "Alpha fade",
            Self::Slide => "Slide",
            Self::Zoom => "Zoom",
            Self::Wipe => "Wipe",
        }
    }

    #[must_use]
    pub const fn payload(self, duration_frames: u32) -> CommandPayload {
        match self {
            Self::Fade => CommandPayload::Fade { duration_frames },
            Self::AlphaFade => CommandPayload::AlphaFade { duration_frames },
            Self::Slide => CommandPayload::Slide { duration_frames },
            Self::Zoom => CommandPayload::Zoom { duration_frames },
            Self::Wipe => CommandPayload::Wipe { duration_frames },
        }
    }
}

/// One operator control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperatorControl {
    /// Put an input on Preview.
    Preview(InputId),
    Cut,
    /// Take Preview to Program with the selected automatic transition.
    Auto(OperatorTransition),
    /// Toggle Fade to Black toward the opposite of its desired target.
    FadeToBlack,
    /// Start or stop program recorder `1` or `2`.
    Recorder(u8),
    /// Start or stop one stream destination.
    Stream(NonZeroU128),
    /// Mute or unmute an input's audio strip.
    Mute(InputId),
}

impl OperatorControl {
    /// The granted permission the control needs.
    #[must_use]
    pub const fn required_permission(self) -> &'static str {
        match self {
            Self::Preview(_) => OPERATOR_PREVIEW_PERMISSION,
            Self::Mute(_) => OPERATOR_AUDIO_PERMISSION,
            Self::Cut | Self::Auto(_) | Self::FadeToBlack | Self::Recorder(_) | Self::Stream(_) => {
                OPERATOR_TRANSITION_PERMISSION
            }
        }
    }
}

/// Why a control is disabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperatorBlock {
    /// No live session: the page is connecting, reconnecting, or stopped.
    NotLive,
    /// The granted role lacks the control's permission.
    MissingPermission(&'static str),
    /// The input is already the desired Preview source.
    AlreadyOnPreview,
    /// The input, recorder, or stream is not in the replicated show.
    NotInShow,
    /// The input has no replicated audio strip yet (an input added live is
    /// replicated before the next switcher event carries its strip).
    NoAudioStrip,
    /// The project shortcut is disabled.
    ShortcutDisabled,
    /// The shortcut acts on the selected input and the page selects none
    /// (RC-004).
    NoSelection,
}

impl OperatorBlock {
    /// The exact disabled reason the operator page shows.
    #[must_use]
    pub fn reason(self) -> String {
        match self {
            Self::NotLive => "Not connected to the show".to_owned(),
            Self::MissingPermission(permission) => {
                format!("This session lacks the {permission} permission")
            }
            Self::AlreadyOnPreview => "Already on Preview".to_owned(),
            Self::NotInShow => "No longer in the show".to_owned(),
            Self::NoAudioStrip => "Waiting for this input's audio state".to_owned(),
            Self::ShortcutDisabled => "This shortcut is disabled".to_owned(),
            Self::NoSelection => {
                "This shortcut acts on the selected input: select an input first".to_owned()
            }
        }
    }
}

/// The Fade-to-Black phase the page badges.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FadeToBlackPhase {
    Live,
    FadingToBlack,
    Black,
    FadingUp,
}

impl FadeToBlackPhase {
    /// Derives the phase from the desired target and the realized position.
    #[must_use]
    pub const fn from_state(target_black: bool, realized: FadeToBlackPosition) -> Self {
        let numerator = realized.numerator();
        match (target_black, numerator) {
            (true, u16::MAX) => Self::Black,
            (true, _) => Self::FadingToBlack,
            (false, 0) => Self::Live,
            (false, _) => Self::FadingUp,
        }
    }

    /// Badge text.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Live => "LIVE",
            Self::FadingToBlack => "FADING TO BLACK",
            Self::Black => "BLACK",
            Self::FadingUp => "FADING UP",
        }
    }
}

/// Status text of a stream destination's realized state.
#[must_use]
pub const fn stream_state_label(state: StreamRealizedState) -> &'static str {
    match state {
        StreamRealizedState::Stopped => "Stopped",
        StreamRealizedState::Starting => "Starting",
        StreamRealizedState::Live => "LIVE",
        StreamRealizedState::WaitingToReconnect => "Reconnecting",
        StreamRealizedState::Congested => "Congested",
        StreamRealizedState::Failed => "Failed",
        StreamRealizedState::Unavailable => "Unavailable",
    }
}

/// Converts a transition duration in milliseconds to whole frames at
/// `numerator/denominator` frames per second, at least one frame.
#[must_use]
pub fn duration_frames(milliseconds: u32, numerator: u32, denominator: u32) -> u32 {
    let denominator = u64::from(denominator.max(1)) * 1_000;
    let frames = (u64::from(milliseconds) * u64::from(numerator) + denominator / 2) / denominator;
    u32::try_from(frames).unwrap_or(u32::MAX).max(1)
}

/// One evaluation of the operator surface over replicated state.
#[derive(Clone, Copy, Debug)]
pub struct OperatorSurface<'a> {
    view: Option<&'a ClientView>,
    permissions: &'a [String],
    /// The input selected on the page (RC-004).
    selected: Option<InputId>,
}

impl<'a> OperatorSurface<'a> {
    /// `view` is the live session's replicated state, `None` while not live;
    /// `permissions` are the session's granted permissions.
    #[must_use]
    pub const fn new(view: Option<&'a ClientView>, permissions: &'a [String]) -> Self {
        Self {
            view,
            permissions,
            selected: None,
        }
    }

    /// The surface with `selected` as the input selected on the page
    /// (RC-004): the input a shortcut action on the selected input acts on,
    /// sent with every shortcut fire. The page selects an input by tapping
    /// its tile in the Shortcuts section, and again to clear it.
    #[must_use]
    pub const fn with_selection(mut self, selected: Option<InputId>) -> Self {
        self.selected = selected;
        self
    }

    /// The page's selected input while it is still in the show; a removed
    /// input is no longer selected.
    #[must_use]
    pub fn selection(&self) -> Option<InputId> {
        let view = self.view?;
        self.selected.filter(|input| view.inputs.contains(input))
    }

    /// Whether the control is enabled, or why it is not: a lost session
    /// first, then the role's permission, then the replicated state.
    ///
    /// # Errors
    ///
    /// Returns the [`OperatorBlock`] the page shows beside the control.
    pub fn gate(&self, control: OperatorControl) -> Result<(), OperatorBlock> {
        let view = self.view.ok_or(OperatorBlock::NotLive)?;
        let permission = control.required_permission();
        if !self.permissions.iter().any(|granted| granted == permission) {
            return Err(OperatorBlock::MissingPermission(permission));
        }
        match control {
            OperatorControl::Preview(input) => {
                if !view.inputs.contains(&input) {
                    return Err(OperatorBlock::NotInShow);
                }
                if view.switcher.desired.preview == input {
                    return Err(OperatorBlock::AlreadyOnPreview);
                }
            }
            OperatorControl::Recorder(recorder) => {
                if !view
                    .recordings
                    .iter()
                    .any(|status| status.recorder.get() == recorder)
                {
                    return Err(OperatorBlock::NotInShow);
                }
            }
            OperatorControl::Stream(target) => {
                if !view.streams.iter().any(|status| status.target == target) {
                    return Err(OperatorBlock::NotInShow);
                }
            }
            OperatorControl::Mute(input) => {
                if !view.inputs.contains(&input) {
                    return Err(OperatorBlock::NotInShow);
                }
                if !view
                    .input_audio_strips
                    .iter()
                    .any(|strip| strip.input == input)
                {
                    return Err(OperatorBlock::NoAudioStrip);
                }
            }
            OperatorControl::Cut | OperatorControl::Auto(_) | OperatorControl::FadeToBlack => {}
        }
        Ok(())
    }

    /// The command an enabled control sends; toggles target the opposite of
    /// the replicated desired state. A mute keeps every other strip value.
    #[must_use]
    pub fn command(
        &self,
        control: OperatorControl,
        duration_frames: u32,
    ) -> Option<CommandPayload> {
        self.gate(control).ok()?;
        let view = self.view?;
        Some(match control {
            OperatorControl::Preview(input) => CommandPayload::SelectPreview {
                input: WireInputId::from_domain(input),
            },
            OperatorControl::Cut => CommandPayload::Cut,
            OperatorControl::Auto(transition) => transition.payload(duration_frames),
            OperatorControl::FadeToBlack => CommandPayload::FadeToBlack {
                active: !view.switcher.desired_fade_to_black.target_active,
                duration_frames,
            },
            OperatorControl::Recorder(recorder) => {
                let slot = WireRecorderSlot::new(recorder)?;
                let active = view
                    .recordings
                    .iter()
                    .find(|status| status.recorder == slot)?
                    .desired_active;
                if active {
                    CommandPayload::RecordStop { recorder: slot }
                } else {
                    CommandPayload::RecordStart { recorder: slot }
                }
            }
            OperatorControl::Stream(target) => {
                let running = view
                    .streams
                    .iter()
                    .find(|status| status.target == target)?
                    .desired_running;
                let target = WireStreamTargetId::new(target);
                if running {
                    CommandPayload::StreamStop { target }
                } else {
                    CommandPayload::StreamStart { target }
                }
            }
            OperatorControl::Mute(input) => {
                let strip = view
                    .input_audio_strips
                    .iter()
                    .find(|strip| strip.input == input)?;
                CommandPayload::SetInputAudioStrip {
                    input: WireInputId::from_domain(input),
                    gain_millidb: strip.gain_millidb,
                    balance_basis_points: strip.balance_basis_points,
                    pan_basis_points: strip.pan_basis_points,
                    muted: !strip.muted,
                    soloed: strip.soloed,
                    follow_video: strip.follow_video,
                    delay_samples: strip.delay_samples,
                }
            }
        })
    }

    /// Whether project shortcut `shortcut`'s button is enabled, or why it
    /// is not: a lost session, the `transition` permission every fire
    /// needs, a disabled shortcut, then the permission of each cue action
    /// it runs, in order, then a selection when an action acts on the
    /// selected input (RC-004). A cue list GO or a script also needs
    /// whatever its cues or verbs need (and a GO a selection when its cues
    /// act on the selected input), which the daemon checks when it is
    /// fired, refusing the whole fire.
    ///
    /// # Errors
    ///
    /// Returns the [`OperatorBlock`] the page shows on the button.
    pub fn shortcut_gate(&self, shortcut: &ShortcutStatus) -> Result<(), OperatorBlock> {
        self.view.ok_or(OperatorBlock::NotLive)?;
        let granted = |permission: &str| self.permissions.iter().any(|found| found == permission);
        if !granted(OPERATOR_TRANSITION_PERMISSION) {
            return Err(OperatorBlock::MissingPermission(
                OPERATOR_TRANSITION_PERMISSION,
            ));
        }
        if !shortcut.spec.enabled {
            return Err(OperatorBlock::ShortcutDisabled);
        }
        if let WireShortcutAction::Actions(actions) = &shortcut.spec.action
            && let Some(missing) = actions
                .iter()
                .filter_map(cue_action_permission)
                .find(|permission| !granted(permission))
        {
            return Err(OperatorBlock::MissingPermission(missing));
        }
        if let WireShortcutAction::Actions(actions) = &shortcut.spec.action
            && self.selection().is_none()
            && actions
                .iter()
                .any(|action| action.target() == Some(fm_types::InputTarget::Selected))
        {
            return Err(OperatorBlock::NoSelection);
        }
        Ok(())
    }

    /// The `shortcut_fire` an enabled shortcut button sends, carrying the
    /// page's selected input (RC-004) when it has one.
    #[must_use]
    pub fn shortcut_command(&self, shortcut: &ShortcutStatus) -> Option<CommandPayload> {
        self.shortcut_gate(shortcut).ok()?;
        Some(CommandPayload::FireShortcut {
            shortcut: shortcut.id,
            targets: fm_protocol::WireTargetBindings {
                selected: self.selection().map(WireInputId::from_domain),
                ..fm_protocol::WireTargetBindings::NONE
            },
        })
    }

    /// The Fade-to-Black phase, `None` while not live.
    #[must_use]
    pub fn fade_to_black(&self) -> Option<FadeToBlackPhase> {
        self.view.map(|view| {
            FadeToBlackPhase::from_state(
                view.switcher.desired_fade_to_black.target_active,
                view.switcher.realized_fade_to_black.position,
            )
        })
    }
}
