//! Operator cue lists (Protocol 2.50, RC-017; media actions and millisecond
//! delays Protocol 2.51).
//!
//! A project holds up to [`MAX_CUE_LISTS`] ordered cue lists. Each cue is a
//! name and a bounded list of [`WireCueAction`]s; the authority keeps the
//! standby pointer and run state ([`CueRunStatus`]). List edits
//! (`cue_list_set`, `cue_list_remove`) are undoable project edits (since
//! Protocol 2.79 `cue_list_set` carries an optional `position` below
//! [`MAX_CUE_LISTS`], so an undone removal returns to its place); GO,
//! standby moves, stop, and reset (`cue_go`, `cue_standby`, `cue_back`,
//! `cue_skip`, `cue_stop`, `cue_reset`) are live operations. A GO runs the
//! cue's actions up to its first wait on the command's target frame; the
//! authority's cue clock (`cue_advance`, refused from clients) runs the rest
//! on their due frames.
//!
//! A snapshot names how many `cue_list_state` records follow it
//! ([`crate::SnapshotMessage::cue_list_states`]), one per list in project
//! order. `cue_list_changed` carries a whole list after an edit,
//! `cue_list_removed` a removed list, and `cue_run_changed` the run state
//! after a live operation together with every desired state its actions
//! changed and, since Protocol 2.61, what each script step it ran did.
//!
//! Protocol 2.64 arms a GO for later (`cue_arm`, `cue_disarm`): the armed
//! cue fires `in_ms` milliseconds after the command's frame, rounded up to
//! whole frames at the show rate, on the authority's cue clock exactly as a
//! GO would; the run state carries it as [`CueArmedStatus`].
//!
//! Protocol 2.65 adds the data update action [`WireCueAction::TitleNumber`]
//! (set or step one number field of a title, such as a score) and carries
//! the value each title field received as a [`CueTitleValue`]: text for a
//! text field, an integer for a number field.

use crate::{
    FadeToBlackState, InputAudioStripStatus, ManualTransitionStatus, MediaTime,
    MediaTransportState, OverlayStatus, PlaylistRunState, RecordingStatus, StreamStatus,
    TransitionFavoriteEffect, WireInputId, WireOverlayChannelId, WireRecorderSlot,
    WireStreamTargetId, WireTitleFieldId,
};

/// Most cue lists one project holds.
pub const MAX_CUE_LISTS: usize = 8;
/// Most cues one list holds.
pub const MAX_CUES: usize = 64;
/// Most actions (waits included) one cue holds.
pub const MAX_CUE_ACTIONS: usize = 16;
/// Most actions one list holds across all its cues.
pub const MAX_CUE_LIST_ACTIONS: usize = 256;
/// Most script steps one list holds (Protocol 2.60).
pub const MAX_CUE_LIST_SCRIPT_STEPS: usize = 8;
/// Longest list or cue name, in UTF-8 bytes.
pub const MAX_CUE_NAME_BYTES: usize = 64;
/// Longest title text one action sets, in UTF-8 bytes.
pub const MAX_CUE_TEXT_BYTES: usize = 128;
/// Most title text bytes one list carries across all its actions.
pub const MAX_CUE_LIST_TEXT_BYTES: usize = 2_048;
/// Longest wait, in frames.
pub const MAX_CUE_WAIT_FRAMES: u32 = 216_000;
/// Longest millisecond delay (one hour).
pub const MAX_CUE_DELAY_MS: u32 = 3_600_000;
/// Longest an armed GO waits, in milliseconds (24 hours, Protocol 2.64).
pub const MAX_CUE_ARM_MS: u32 = 86_400_000;
/// Longest transition or Fade to Black an action requests, in frames.
pub const MAX_CUE_TRANSITION_FRAMES: u32 = 3_600;
/// Largest magnitude a title number action sets or adds, and largest an
/// added value may reach (Protocol 2.65).
pub const MAX_CUE_TITLE_NUMBER: i64 = 999_999_999_999;

/// One step of a cue.
///
/// Since Protocol 2.88 (RC-004) an action names its input with a
/// [`WireInputTarget`]: an input id, or `preview`, `program`, or `selected`,
/// resolved when the action is fired.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireCueAction {
    /// Puts `input` on Preview.
    Preview {
        input: WireInputTarget,
    },
    /// Takes `input` (or the current Preview with `None`) to Program: a cut
    /// (zero frames), a mix, or a Stinger slot over `duration_frames`.
    Take {
        input: Option<WireInputTarget>,
        effect: TransitionFavoriteEffect,
        duration_frames: u32,
    },
    OverlayOn {
        channel: WireOverlayChannelId,
        input: WireInputTarget,
    },
    OverlayOff {
        channel: WireOverlayChannelId,
    },
    /// Takes title `input` onto overlay `channel`.
    TitleOn {
        input: WireInputTarget,
        channel: WireOverlayChannelId,
    },
    /// Sets one text field of title `input`.
    TitleText {
        input: WireInputTarget,
        field: WireTitleFieldId,
        text: String,
    },
    /// Sets (`add` false) or adds the nonzero `value` to (`add` true) one
    /// number field of title `input`, or a text field holding a plain
    /// decimal integer (Protocol 2.65): `value` at most
    /// [`MAX_CUE_TITLE_NUMBER`] in magnitude.
    TitleNumber {
        input: WireInputTarget,
        field: WireTitleFieldId,
        add: bool,
        value: i64,
    },
    TitleOff {
        input: WireInputTarget,
    },
    FadeToBlack {
        active: bool,
        duration_frames: u32,
    },
    PlaylistStart,
    PlaylistStop,
    AudioGain {
        input: WireInputTarget,
        gain_millidb: i32,
    },
    AudioMute {
        input: WireInputTarget,
        muted: bool,
    },
    Record {
        recorder: WireRecorderSlot,
        start: bool,
    },
    Stream {
        target: WireStreamTargetId,
        start: bool,
    },
    /// Runs `operation` on media input `input`'s transport (Protocol 2.51).
    Media {
        input: WireInputTarget,
        operation: WireCueMediaOperation,
    },
    /// Steps paged input `input` (a slideshow, IN-009): token
    /// `page.<input>.<next|previous|first|last>` or
    /// `page.<input>.goto.<page>`.
    Page {
        input: WireInputTarget,
        navigation: crate::PagedNavigation,
    },
    /// Defers the cue's remaining actions by `frames` (at least one).
    Wait {
        frames: u32,
    },
    /// Defers the cue's remaining actions by `millis` milliseconds (at
    /// least one), converted to whole frames at the show rate when the cue
    /// fires, rounded up (Protocol 2.51).
    Delay {
        millis: u32,
    },
    /// Runs project script `script` (nonzero) as one step (Protocol 2.60):
    /// its actions are the step's actions, all or nothing.
    RunScript {
        script: u64,
    },
}

/// The input a cue or shortcut action acts on (Protocol 2.88, RC-004): a
/// fixed input or a role resolved when the action is fired. Its token is
/// the input id or `preview`, `program`, or `selected`.
pub type WireInputTarget = fm_types::InputTarget<WireInputId>;

/// The inputs the dynamic targets of one fire resolve to (Protocol 2.88,
/// RC-004). A client sends `selected`, the input selected on its console;
/// the authority binds `preview` and `program` and journals all three, so
/// a replay acts on the same inputs. Encoded as the optional command fields
/// `selected`, `bound_preview`, and `bound_program`.
pub type WireTargetBindings = fm_types::TargetBindings<WireInputId>;

impl WireCueAction {
    /// The action naming `target` in place of its input; an action naming
    /// no input (and a take of the current Preview) is unchanged.
    #[must_use]
    pub fn with_target(mut self, target: WireInputTarget) -> Self {
        match &mut self {
            Self::Preview { input }
            | Self::OverlayOn { input, .. }
            | Self::TitleOn { input, .. }
            | Self::TitleText { input, .. }
            | Self::TitleNumber { input, .. }
            | Self::TitleOff { input }
            | Self::AudioGain { input, .. }
            | Self::AudioMute { input, .. }
            | Self::Media { input, .. }
            | Self::Page { input, .. }
            | Self::Take {
                input: Some(input), ..
            } => *input = target,
            _ => {}
        }
        self
    }

    /// The input the action names, as it names it.
    #[must_use]
    pub const fn target(&self) -> Option<WireInputTarget> {
        match self {
            Self::Preview { input }
            | Self::OverlayOn { input, .. }
            | Self::TitleOn { input, .. }
            | Self::TitleText { input, .. }
            | Self::TitleNumber { input, .. }
            | Self::TitleOff { input }
            | Self::AudioGain { input, .. }
            | Self::AudioMute { input, .. }
            | Self::Media { input, .. }
            | Self::Page { input, .. } => Some(*input),
            Self::Take { input, .. } => *input,
            Self::OverlayOff { .. }
            | Self::FadeToBlack { .. }
            | Self::PlaylistStart
            | Self::PlaylistStop
            | Self::Record { .. }
            | Self::Stream { .. }
            | Self::Wait { .. }
            | Self::Delay { .. }
            | Self::RunScript { .. } => None,
        }
    }
}

/// A media transport operation a cue runs (Protocol 2.51).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireCueMediaOperation {
    Play,
    Pause,
    /// Seeks to source frame `frame`, clamped to the trimmed range.
    Seek {
        frame: u64,
    },
    /// Seeks to the start of the trimmed range and plays.
    Restart,
}

/// One cue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireCue {
    pub id: u64,
    pub name: String,
    /// Whether the next cue fires once this cue's last action has run.
    pub auto_follow: bool,
    pub actions: Vec<WireCueAction>,
}

/// A list's definition: its name and ordered cues.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CueListSpec {
    pub name: String,
    pub cues: Vec<WireCue>,
}

/// A cue whose action `action` and later wait for `due_frame`; `action`
/// equals the cue's action count when only its auto-follow remains.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CuePendingStatus {
    pub cue: u64,
    pub action: u16,
    pub due_frame: u64,
}

/// A GO of `cue` armed to fire on engine frame `due_frame` (Protocol 2.64).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CueArmedStatus {
    pub cue: u64,
    pub due_frame: u64,
}

/// A list's run state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CueRunStatus {
    /// The cue the next GO fires; `None` past the end.
    pub standby: Option<u64>,
    /// The cue fired most recently.
    pub live: Option<u64>,
    pub pending: Option<CuePendingStatus>,
    /// Every cue fired since the list was created or reset, ascending.
    pub played: Vec<u64>,
    /// The action that was refused when its wait ended or its armed GO
    /// fired.
    pub fault: Option<CueFaultStatus>,
    /// The GO armed for a later frame (Protocol 2.64).
    pub armed: Option<CueArmedStatus>,
}

/// Action `action` of cue `cue` was refused when its wait ended; the cue
/// stopped there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CueFaultStatus {
    pub cue: u64,
    pub action: u16,
    /// What the action was refused with (Protocol 2.63).
    pub detail: fm_command::FaultDetail,
}

/// One whole list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CueListStatus {
    pub id: u64,
    pub spec: CueListSpec,
    pub state: CueRunStatus,
}

/// A live cue list operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CueOperation {
    /// Fires `cue`, or the standby cue with `None`; its dynamic targets
    /// resolve under `targets` (Protocol 2.88).
    Go {
        cue: Option<u64>,
        targets: WireTargetBindings,
    },
    /// Puts `cue` on standby (jump).
    Standby { cue: u64 },
    /// Moves standby to the previous cue.
    Back,
    /// Moves standby past its cue without firing it.
    Skip,
    /// Drops a waiting cue's remaining actions and the armed GO.
    Stop,
    /// Clears the run state and puts the first cue on standby.
    Reset,
    /// Arms a GO of `cue`, or of the standby cue with `None`, to fire
    /// `in_ms` (`1..=`[`MAX_CUE_ARM_MS`]) milliseconds after the command's
    /// frame (Protocol 2.64), replacing a GO armed before. Its dynamic
    /// targets resolve under `targets`, bound when armed (Protocol 2.88).
    Arm {
        cue: Option<u64>,
        in_ms: u32,
        targets: WireTargetBindings,
    },
    /// Drops the armed GO (Protocol 2.64).
    Disarm,
}

impl CueOperation {
    /// The command payload token.
    #[must_use]
    pub const fn payload(self) -> &'static str {
        match self {
            Self::Go { .. } => "cue_go",
            Self::Standby { .. } => "cue_standby",
            Self::Back => "cue_back",
            Self::Skip => "cue_skip",
            Self::Stop => "cue_stop",
            Self::Reset => "cue_reset",
            Self::Arm { .. } => "cue_arm",
            Self::Disarm => "cue_disarm",
        }
    }
}

/// The final value a live operation gave one title field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CueTitleText {
    pub input: WireInputId,
    /// The title's field revision afterwards.
    pub field_revision: u64,
    pub field: WireTitleFieldId,
    pub value: CueTitleValue,
}

/// A value a live operation gave a title field (Protocol 2.65).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CueTitleValue {
    Text(String),
    Number(i64),
}

/// A live operation was accepted for `frame`: the list's run state, the
/// cues it fired, and every desired state afterwards that the fired
/// actions may have changed. The actions reach Program on exactly `frame`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CueRunChange {
    pub list: u64,
    pub frame: u64,
    pub state: CueRunStatus,
    pub fired: Vec<u64>,
    pub program: WireInputId,
    pub preview: WireInputId,
    pub manual_transition: ManualTransitionStatus,
    pub fade_to_black: FadeToBlackState,
    pub overlays: Vec<OverlayStatus>,
    pub input_audio_strips: Vec<InputAudioStripStatus>,
    pub streams: Vec<StreamStatus>,
    pub recordings: Vec<RecordingStatus>,
    pub playlist: PlaylistRunState,
    pub titles: Vec<CueTitleText>,
    /// The transport of every media input the fired actions addressed,
    /// once they have run, ascending by input (Protocol 2.51).
    pub media: Vec<CueMediaTransport>,
    /// Every script step that ran on `frame`, in the order it ran
    /// (Protocol 2.61): at most [`MAX_CUE_LIST_SCRIPT_STEPS`], a script that
    /// two steps run appearing twice.
    pub scripts: Vec<CueScriptRun>,
}

/// What one script step of a cue run did (Protocol 2.61): the counts a
/// `script_ran` of the same script on the same show reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CueScriptRun {
    /// The project script the step ran (nonzero).
    pub script: u64,
    /// Actions the run performed.
    pub actions: u32,
    /// Instructions the run used.
    pub instructions: u32,
}

/// A media input's transport after a cue's media actions (Protocol 2.51):
/// the parts of its [`crate::MediaTransportStatus`] a transport operation
/// changes, projected onto the run's frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CueMediaTransport {
    pub input: WireInputId,
    pub state: MediaTransportState,
    pub position: u64,
    pub position_time: MediaTime,
}

/// One cue list at a snapshot's revision. A session writes exactly
/// [`crate::SnapshotMessage::cue_list_states`] of these after the
/// snapshot's media states, in project list order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CueListStateMessage {
    pub revision: u64,
    /// The engine's frame cursor at the snapshot.
    pub frame: u64,
    pub list: CueListStatus,
}
