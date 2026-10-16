use fm_types::{InputId, OutputId};

use crate::{
    FadeToBlackCurve, FadeToBlackPosition, FadeToBlackTarget, MissingMediaFallback,
    OverlayBorderPreset, OverlayChannelId, OverlayPositionPreset, OverlayTransitionKind,
    RecorderSlot, StingerPreloadState, StingerSlotId, StreamTargetId, TBarPosition, TransitionKind,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SwitcherCommand {
    SelectPreview(InputId),
    Cut,
    Transition {
        kind: TransitionKind,
        duration_frames: u32,
    },
    Wipe {
        duration_frames: u32,
    },
    StartTBar {
        kind: TransitionKind,
    },
    /// Starts a manual Stinger scrubbing a `duration_frames` clip (SW-004).
    StartStingerTBar {
        slot: StingerSlotId,
        duration_frames: u32,
    },
    SetTBarPosition(TBarPosition),
    CommitTBar,
    CancelTBar,
    TakeOverlay {
        channel: OverlayChannelId,
        source: InputId,
    },
    UpdateOverlay {
        channel: OverlayChannelId,
        source: InputId,
    },
    OverlayOff(OverlayChannelId),
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
    TakeNextOverlay(OverlayChannelId),
    SetOverlayOutputInclusion {
        channel: OverlayChannelId,
        output: OutputId,
        included: bool,
    },
    /// Chooses whether Fade to Black fades `output` (SW-004).
    SetFadeToBlackOutput {
        output: OutputId,
        affected: bool,
    },
    /// Gives `output` its own Fade-to-Black duration, or with `None`
    /// returns it to the requested one (SW-004).
    SetFadeToBlackOutputDuration {
        output: OutputId,
        duration_frames: Option<u32>,
    },
    /// Chooses how automatic Fade-to-Black moves ease (SW-004).
    SetFadeToBlackCurve(FadeToBlackCurve),
    SetStreamRunning {
        target: StreamTargetId,
        running: bool,
    },
    SetRecordingDesired {
        slot: RecorderSlot,
        active: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SwitcherEvent {
    PreviewSelected {
        input: InputId,
    },
    ProgramChanged {
        previous: InputId,
        program: InputId,
    },
    TransitionStarted {
        kind: TransitionKind,
        from: InputId,
        to: InputId,
        duration_frames: u32,
    },
    TransitionCompleted {
        kind: TransitionKind,
        program: InputId,
    },
    TBarStarted {
        kind: TransitionKind,
        from: InputId,
        to: InputId,
    },
    TBarPositionChanged {
        position: TBarPosition,
    },
    TBarCancelled,
    FadeToBlackStarted {
        from: FadeToBlackPosition,
        target: FadeToBlackTarget,
        duration_frames: u32,
    },
    FadeToBlackPositionChanged {
        position: FadeToBlackPosition,
    },
    FadeToBlackCompleted {
        active: bool,
    },
    FadeToBlackOutputDurationChanged {
        output: OutputId,
        duration_frames: Option<u32>,
    },
    FadeToBlackCurveChanged {
        curve: FadeToBlackCurve,
    },
    OverlayTaken {
        channel: OverlayChannelId,
        source: InputId,
    },
    OverlayUpdated {
        channel: OverlayChannelId,
        source: InputId,
    },
    OverlayTurnedOff {
        channel: OverlayChannelId,
    },
    OverlayTransitionConfigured {
        channel: OverlayChannelId,
        transition: OverlayTransitionKind,
        duration_frames: u32,
    },
    OverlayAppearanceConfigured {
        channel: OverlayChannelId,
        position: OverlayPositionPreset,
        border: OverlayBorderPreset,
    },
    OverlayQueued {
        channel: OverlayChannelId,
        source: InputId,
        depth: usize,
    },
    OverlayQueueAdvanced {
        channel: OverlayChannelId,
        source: InputId,
        remaining: usize,
    },
    OverlayOpacityChanged {
        channel: OverlayChannelId,
        opacity: u8,
    },
    OverlayTransitionCompleted {
        channel: OverlayChannelId,
        active: bool,
    },
    OverlayOutputInclusionChanged {
        channel: OverlayChannelId,
        output: OutputId,
        included: bool,
    },
    FadeToBlackOutputChanged {
        output: OutputId,
        affected: bool,
    },
    StingerPreloadChanged {
        slot: StingerSlotId,
        state: StingerPreloadState,
    },
    StingerFallbackApplied {
        slot: StingerSlotId,
        fallback: MissingMediaFallback,
    },
    StreamRunningChanged {
        target: StreamTargetId,
        running: bool,
    },
    RecordingDesiredChanged {
        slot: RecorderSlot,
        active: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SwitcherError {
    UnknownInput(InputId),
    /// The requested Preview input is already the Program source. The project
    /// model requires the two to differ, so accepting this would leave a
    /// state no checkpoint could persist.
    PreviewEqualsProgram(InputId),
    TransitionInProgress,
    UnsupportedManualTransitionKind,
    InvalidManualTransitionRoute,
    ZeroDuration,
    InvalidOverlayTransitionDuration {
        duration_frames: u32,
        maximum: u32,
    },
    OverlayQueueFull {
        channel: OverlayChannelId,
        maximum: usize,
    },
    OverlayQueueEmpty(OverlayChannelId),
    UnconfiguredStinger(StingerSlotId),
    /// A manual Stinger needs its media preloaded; it has no fallback.
    StingerMediaUnavailable(StingerSlotId),
    StingerCutPointOutOfRange {
        slot: StingerSlotId,
        cut_point_frames: u32,
        duration_frames: u32,
    },
    UnknownStreamTarget(StreamTargetId),
    DuplicateStreamTarget(StreamTargetId),
    InvalidStreamName,
    TooManyStreams {
        requested: usize,
        maximum: usize,
    },
    /// An added input already belongs to this mix.
    DuplicateInput(InputId),
    /// A removed input is still referenced by switcher state.
    InputInUse {
        input: InputId,
        usage: InputUsage,
    },
}

/// The first switcher reference that keeps an input from being removed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputUsage {
    Program,
    Preview,
    /// An endpoint of an automatic transition in flight.
    Transition,
    /// An endpoint of an active manual (T-bar) transition.
    ManualTransition,
    /// The source of an overlay channel, on air or fading.
    Overlay(OverlayChannelId),
    /// A queued source of an overlay channel.
    OverlayQueue(OverlayChannelId),
    /// The media input of a configured Stinger slot.
    Stinger(StingerSlotId),
}

impl core::fmt::Display for InputUsage {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Program => formatter.write_str("on Program"),
            Self::Preview => formatter.write_str("on Preview"),
            Self::Transition => formatter.write_str("in a transition in flight"),
            Self::ManualTransition => formatter.write_str("in the active manual transition"),
            Self::Overlay(channel) => {
                write!(formatter, "the source of overlay {}", channel.number())
            }
            Self::OverlayQueue(channel) => {
                write!(formatter, "queued on overlay {}", channel.number())
            }
            Self::Stinger(slot) => write!(formatter, "the media of Stinger {}", slot.number()),
        }
    }
}

impl core::fmt::Display for SwitcherError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownInput(id) => write!(formatter, "input {id} is not part of this mix"),
            Self::PreviewEqualsProgram(id) => write!(
                formatter,
                "input {id} is already the Program source; Preview and Program must differ"
            ),
            Self::TransitionInProgress => {
                formatter.write_str("a transition is already in progress")
            }
            Self::UnsupportedManualTransitionKind => {
                formatter.write_str("a manual Stinger needs its clip length in frames")
            }
            Self::InvalidManualTransitionRoute => {
                formatter.write_str("manual transition endpoints must match Program and Preview")
            }
            Self::ZeroDuration => formatter.write_str("transition duration must be nonzero"),
            Self::InvalidOverlayTransitionDuration {
                duration_frames,
                maximum,
            } => write!(
                formatter,
                "overlay transition duration {duration_frames} is outside 1..={maximum}"
            ),
            Self::OverlayQueueFull { channel, maximum } => write!(
                formatter,
                "overlay channel {} queue is full (maximum {maximum})",
                channel.number()
            ),
            Self::OverlayQueueEmpty(channel) => {
                write!(
                    formatter,
                    "overlay channel {} queue is empty",
                    channel.number()
                )
            }
            Self::StingerMediaUnavailable(slot) => write!(
                formatter,
                "stinger slot {} media is not preloaded; a manual Stinger has no fallback",
                slot.number()
            ),
            Self::UnconfiguredStinger(slot) => {
                write!(
                    formatter,
                    "stinger slot {} is not configured",
                    slot.number()
                )
            }
            Self::StingerCutPointOutOfRange {
                slot,
                cut_point_frames,
                duration_frames,
            } => write!(
                formatter,
                "stinger slot {} cut point {cut_point_frames} exceeds duration {duration_frames}",
                slot.number()
            ),
            Self::UnknownStreamTarget(target) => {
                write!(formatter, "stream target {target} is not part of this mix")
            }
            Self::DuplicateStreamTarget(target) => {
                write!(formatter, "stream target {target} occurs more than once")
            }
            Self::DuplicateInput(input) => {
                write!(formatter, "input {input} is already part of this mix")
            }
            Self::InputInUse { input, usage } => {
                write!(formatter, "input {input} cannot be removed: it is {usage}")
            }
            Self::InvalidStreamName => {
                formatter.write_str("stream name must be nonblank and within the byte limit")
            }
            Self::TooManyStreams { requested, maximum } => write!(
                formatter,
                "stream inventory requests {requested} targets; maximum is {maximum}"
            ),
        }
    }
}

impl std::error::Error for SwitcherError {}
