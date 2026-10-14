//! Replay shortcut/controller surface: typed operator actions bound through
//! two independent controller profiles with conflict detection.
//!
//! Keyboard keys are one shared physical surface, so binding the same key in
//! both profiles is a conflict. Buttons and MIDI notes belong to each
//! profile's own device and never conflict across profiles.

use core::fmt;
use std::collections::BTreeMap;

use fm_frame::NormalizedTimestamp;

use crate::{
    CameraId, ChannelMode, EventDatabase, EventId, MAX_REPLAY_SOURCES, PlaybackRate,
    ReplayChannelId, ReplayDecks, ReplayError, ReplayMarks, types::check_name,
};

/// Maximum bindings in one controller profile.
pub const MAX_BINDINGS_PER_PROFILE: usize = 256;
/// Largest single jog step in frames.
pub const MAX_JOG_FRAMES: i16 = 100;

/// Nonzero, bounded frame step for jog controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JogStep(i16);

impl JogStep {
    /// # Errors
    ///
    /// Zero and steps beyond ±[`MAX_JOG_FRAMES`] are rejected.
    pub const fn new(frames: i16) -> Result<Self, ControllerError> {
        if frames == 0 || frames > MAX_JOG_FRAMES || frames < -MAX_JOG_FRAMES {
            Err(ControllerError::InvalidJogStep(frames))
        } else {
            Ok(Self(frames))
        }
    }

    #[must_use]
    pub const fn frames(self) -> i16 {
        self.0
    }
}

/// One-based angle button (1 through 8) mapped through the operator's angle bank.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AngleSlot(u8);

impl AngleSlot {
    /// # Errors
    ///
    /// Slots outside `1..=8` are rejected.
    pub const fn new(slot: u8) -> Result<Self, ControllerError> {
        if slot == 0 || slot as usize > MAX_REPLAY_SOURCES {
            Err(ControllerError::InvalidAngleSlot(slot))
        } else {
            Ok(Self(slot))
        }
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Typed replay operator action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayAction {
    MarkIn,
    MarkOut,
    /// Play at a preset speed (e.g. 25 %, 50 %, 100 %).
    Play(PlaybackRate),
    Pause,
    Jog(JogStep),
    Shuttle(PlaybackRate),
    SelectAngle(AngleSlot),
    /// Retargets the controller to channel A or B.
    SelectChannel(ReplayChannelId),
    /// Recorded → live edge; live → cue the recall event.
    ToggleLive,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
}

/// Physical input that triggers an action.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ControlInput {
    /// Shared keyboard key code plus modifiers.
    Key { code: u16, modifiers: Modifiers },
    /// Device-local button index.
    Button(u16),
    /// Device-local MIDI note (channel `0..16`, note `0..128`).
    MidiNote { channel: u8, note: u8 },
}

impl ControlInput {
    #[must_use]
    pub const fn is_shared(self) -> bool {
        matches!(self, Self::Key { .. })
    }

    const fn is_valid(self) -> bool {
        match self {
            Self::Key { .. } | Self::Button(_) => true,
            Self::MidiNote { channel, note } => channel < 16 && note < 128,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ControllerSlot {
    Primary,
    Secondary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControllerProfile {
    name: String,
    default_channel: ReplayChannelId,
    bindings: BTreeMap<ControlInput, ReplayAction>,
}

impl ControllerProfile {
    /// # Errors
    ///
    /// Names must be non-empty and bounded; bindings are bounded by
    /// [`MAX_BINDINGS_PER_PROFILE`], inputs must be valid, and each input may
    /// be bound once.
    pub fn new(
        name: impl Into<String>,
        default_channel: ReplayChannelId,
        bindings: Vec<(ControlInput, ReplayAction)>,
    ) -> Result<Self, ControllerError> {
        let name = name.into();
        check_name(&name).map_err(ControllerError::Replay)?;
        if bindings.len() > MAX_BINDINGS_PER_PROFILE {
            return Err(ControllerError::TooManyBindings(bindings.len()));
        }
        let mut map = BTreeMap::new();
        for (input, action) in bindings {
            if !input.is_valid() {
                return Err(ControllerError::InvalidInput(input));
            }
            if map.insert(input, action).is_some() {
                return Err(ControllerError::DuplicateBinding(input));
            }
        }
        Ok(Self {
            name,
            default_channel,
            bindings: map,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn default_channel(&self) -> ReplayChannelId {
        self.default_channel
    }

    #[must_use]
    pub fn action(&self, input: ControlInput) -> Option<ReplayAction> {
        self.bindings.get(&input).copied()
    }

    pub fn bindings(&self) -> impl Iterator<Item = (ControlInput, ReplayAction)> + '_ {
        self.bindings
            .iter()
            .map(|(input, action)| (*input, *action))
    }
}

/// A shared input bound by both profiles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BindingConflict {
    pub input: ControlInput,
    pub primary: ReplayAction,
    pub secondary: ReplayAction,
}

/// Lists every shared (keyboard) input bound in both profiles, in input order.
#[must_use]
pub fn binding_conflicts(
    primary: &ControllerProfile,
    secondary: &ControllerProfile,
) -> Vec<BindingConflict> {
    primary
        .bindings()
        .filter(|(input, _)| input.is_shared())
        .filter_map(|(input, action)| {
            secondary.action(input).map(|other| BindingConflict {
                input,
                primary: action,
                secondary: other,
            })
        })
        .collect()
}

/// Action resolved for one controller and its current target channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControllerCommand {
    pub slot: ControllerSlot,
    pub channel: ReplayChannelId,
    pub action: ReplayAction,
}

/// Inputs needed to apply a command to the replay decks.
#[derive(Clone, Copy, Debug)]
pub struct ControllerContext<'a> {
    pub live_edge: NormalizedTimestamp,
    pub frame_duration_nanos: u64,
    /// Angle bank: slot `n` selects `angles[n - 1]`.
    pub angles: &'a [CameraId],
    pub events: &'a EventDatabase,
    /// Event cued when toggling a live channel back to replay.
    pub recall: Option<EventId>,
}

/// Two independent controller profiles, each with its own target channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControllerSurface {
    primary: ControllerProfile,
    secondary: ControllerProfile,
    primary_channel: ReplayChannelId,
    secondary_channel: ReplayChannelId,
}

impl ControllerSurface {
    /// # Errors
    ///
    /// Returns the first shared-key conflict between the profiles.
    pub fn new(
        primary: ControllerProfile,
        secondary: ControllerProfile,
    ) -> Result<Self, ControllerError> {
        if let Some(conflict) = binding_conflicts(&primary, &secondary).first() {
            return Err(ControllerError::SharedInputConflict(*conflict));
        }
        Ok(Self {
            primary_channel: primary.default_channel,
            secondary_channel: secondary.default_channel,
            primary,
            secondary,
        })
    }

    #[must_use]
    pub const fn profile(&self, slot: ControllerSlot) -> &ControllerProfile {
        match slot {
            ControllerSlot::Primary => &self.primary,
            ControllerSlot::Secondary => &self.secondary,
        }
    }

    #[must_use]
    pub const fn target_channel(&self, slot: ControllerSlot) -> ReplayChannelId {
        match slot {
            ControllerSlot::Primary => self.primary_channel,
            ControllerSlot::Secondary => self.secondary_channel,
        }
    }

    /// Resolves an input from one controller. `SelectChannel` retargets that
    /// controller only. Unbound inputs return `None`.
    pub fn dispatch(
        &mut self,
        slot: ControllerSlot,
        input: ControlInput,
    ) -> Option<ControllerCommand> {
        let action = self.profile(slot).action(input)?;
        if let ReplayAction::SelectChannel(channel) = action {
            match slot {
                ControllerSlot::Primary => self.primary_channel = channel,
                ControllerSlot::Secondary => self.secondary_channel = channel,
            }
        }
        Some(ControllerCommand {
            slot,
            channel: self.target_channel(slot),
            action,
        })
    }
}

impl ControllerCommand {
    /// Applies the command to the decks and marks. Marks use the channel's
    /// presented position (cursor, or the live edge in live mode).
    ///
    /// # Errors
    ///
    /// Unassigned angle slots, angles outside the cued event, zero frame
    /// durations, and live toggles without a known recall event are rejected
    /// without changing the decks.
    pub fn apply(
        self,
        decks: &mut ReplayDecks,
        marks: &mut ReplayMarks,
        context: &ControllerContext<'_>,
    ) -> Result<(), ControllerError> {
        let channel = self.channel;
        let mode = decks.channel(channel).mode;
        let position = match mode {
            ChannelMode::Live => context.live_edge,
            ChannelMode::Recorded { .. } => decks.channel(channel).cursor,
        };
        match self.action {
            ReplayAction::MarkIn => marks.set_in(position),
            ReplayAction::MarkOut => marks.set_out(position),
            ReplayAction::Play(rate) => decks.play(channel, rate),
            ReplayAction::Pause => decks.pause(channel),
            ReplayAction::Jog(step) => decks.jog(
                channel,
                i64::from(step.frames()),
                context.frame_duration_nanos,
            )?,
            ReplayAction::Shuttle(rate) => decks.shuttle(channel, rate),
            ReplayAction::SelectAngle(slot) => {
                let camera = context
                    .angles
                    .get(usize::from(slot.get()) - 1)
                    .copied()
                    .ok_or(ControllerError::UnassignedAngle(slot))?;
                decks.select_event_angle(channel, camera, context.events)?;
            }
            ReplayAction::SelectChannel(_) => {}
            ReplayAction::ToggleLive => match mode {
                ChannelMode::Recorded { .. } => decks.go_live(channel, context.live_edge),
                ChannelMode::Live => {
                    let id = context.recall.ok_or(ControllerError::NothingToRecall)?;
                    let event = context
                        .events
                        .event(id)
                        .ok_or(ReplayError::UnknownEvent(id))?;
                    decks.cue_event(channel, event);
                }
            },
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControllerError {
    InvalidJogStep(i16),
    InvalidAngleSlot(u8),
    InvalidInput(ControlInput),
    TooManyBindings(usize),
    DuplicateBinding(ControlInput),
    SharedInputConflict(BindingConflict),
    UnassignedAngle(AngleSlot),
    NothingToRecall,
    Replay(ReplayError),
}

impl From<ReplayError> for ControllerError {
    fn from(error: ReplayError) -> Self {
        Self::Replay(error)
    }
}

impl fmt::Display for ControllerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJogStep(frames) => write!(
                formatter,
                "jog step {frames} must be nonzero and within ±{MAX_JOG_FRAMES} frames"
            ),
            Self::InvalidAngleSlot(slot) => write!(
                formatter,
                "angle slot {slot} is outside 1..={MAX_REPLAY_SOURCES}"
            ),
            Self::InvalidInput(input) => write!(formatter, "invalid control input {input:?}"),
            Self::TooManyBindings(count) => write!(
                formatter,
                "{count} bindings exceed the limit of {MAX_BINDINGS_PER_PROFILE}"
            ),
            Self::DuplicateBinding(input) => {
                write!(formatter, "control input {input:?} is bound more than once")
            }
            Self::SharedInputConflict(conflict) => write!(
                formatter,
                "shared input {:?} is bound by both controller profiles",
                conflict.input
            ),
            Self::UnassignedAngle(slot) => {
                write!(
                    formatter,
                    "angle slot {} has no camera assigned",
                    slot.get()
                )
            }
            Self::NothingToRecall => formatter.write_str("live channel has no recall event to cue"),
            Self::Replay(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ControllerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Replay(error) => Some(error),
            _ => None,
        }
    }
}
