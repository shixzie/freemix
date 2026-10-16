//! Operator cue lists (Protocol 2.50, RC-017).
//!
//! A project holds up to [`MAX_CUE_LISTS`] ordered cue lists. Each cue is a
//! name and a bounded list of [`WireCueAction`]s; the authority keeps the
//! standby pointer and run state ([`CueRunStatus`]). List edits
//! (`cue_list_set`, `cue_list_remove`) are undoable project edits; GO,
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
//! changed.

use crate::{
    FadeToBlackState, InputAudioStripStatus, ManualTransitionStatus, OverlayStatus,
    PlaylistRunState, RecordingStatus, StreamStatus, TransitionFavoriteEffect, WireInputId,
    WireOverlayChannelId, WireRecorderSlot, WireStreamTargetId, WireTitleFieldId,
};

/// Most cue lists one project holds.
pub const MAX_CUE_LISTS: usize = 8;
/// Most cues one list holds.
pub const MAX_CUES: usize = 64;
/// Most actions (waits included) one cue holds.
pub const MAX_CUE_ACTIONS: usize = 16;
/// Most actions one list holds across all its cues.
pub const MAX_CUE_LIST_ACTIONS: usize = 256;
/// Longest list or cue name, in UTF-8 bytes.
pub const MAX_CUE_NAME_BYTES: usize = 64;
/// Longest title text one action sets, in UTF-8 bytes.
pub const MAX_CUE_TEXT_BYTES: usize = 128;
/// Most title text bytes one list carries across all its actions.
pub const MAX_CUE_LIST_TEXT_BYTES: usize = 2_048;
/// Longest wait, in frames.
pub const MAX_CUE_WAIT_FRAMES: u32 = 216_000;
/// Longest transition or Fade to Black an action requests, in frames.
pub const MAX_CUE_TRANSITION_FRAMES: u32 = 3_600;

/// One step of a cue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireCueAction {
    /// Puts `input` on Preview.
    Preview {
        input: WireInputId,
    },
    /// Takes `input` (or the current Preview with `None`) to Program: a cut
    /// (zero frames), a mix, or a Stinger slot over `duration_frames`.
    Take {
        input: Option<WireInputId>,
        effect: TransitionFavoriteEffect,
        duration_frames: u32,
    },
    OverlayOn {
        channel: WireOverlayChannelId,
        input: WireInputId,
    },
    OverlayOff {
        channel: WireOverlayChannelId,
    },
    /// Takes title `input` onto overlay `channel`.
    TitleOn {
        input: WireInputId,
        channel: WireOverlayChannelId,
    },
    /// Sets one text field of title `input`.
    TitleText {
        input: WireInputId,
        field: WireTitleFieldId,
        text: String,
    },
    TitleOff {
        input: WireInputId,
    },
    FadeToBlack {
        active: bool,
        duration_frames: u32,
    },
    PlaylistStart,
    PlaylistStop,
    AudioGain {
        input: WireInputId,
        gain_millidb: i32,
    },
    AudioMute {
        input: WireInputId,
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
    /// Defers the cue's remaining actions by `frames` (at least one).
    Wait {
        frames: u32,
    },
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
    /// The `(cue, action)` that was refused when its wait ended.
    pub fault: Option<(u64, u16)>,
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
    /// Fires `cue`, or the standby cue with `None`.
    Go { cue: Option<u64> },
    /// Puts `cue` on standby (jump).
    Standby { cue: u64 },
    /// Moves standby to the previous cue.
    Back,
    /// Moves standby past its cue without firing it.
    Skip,
    /// Drops a waiting cue's remaining actions.
    Stop,
    /// Clears the run state and puts the first cue on standby.
    Reset,
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
        }
    }
}

/// The final text a live operation gave one title field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CueTitleText {
    pub input: WireInputId,
    /// The title's field revision afterwards.
    pub field_revision: u64,
    pub field: WireTitleFieldId,
    pub text: String,
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
