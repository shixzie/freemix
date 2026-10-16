//! Operator cue lists (RC-017, schema 47): ordered cues an operator fires
//! with one GO, each a bounded list of typed show actions and frame waits,
//! plus the persisted standby pointer and run state that drive them.
//!
//! A cue's actions up to its first wait run on one engine frame; a wait
//! defers the rest by a whole number of frames. A cue that follows on
//! (`auto_follow`) fires the next cue as soon as its own last action has
//! run, on the same frame when it ends without a wait. Time is measured in
//! engine frames, so every deferred action lands on an exact frame number
//! and the state is persisted so a restarted authority resumes it.
//!
//! References are checked against the owning project: every input a cue
//! names must exist (and so cannot be removed while a cue names it), every
//! stream target must exist, and channel, slot, and recorder numbers are
//! range-checked here.

use core::fmt;

use fm_types::InputId;

use crate::{RecorderSlot, StreamTargetId, TitleFieldId, TransitionEffect};

/// Most cue lists one project holds.
pub const MAX_CUE_LISTS: usize = 8;
/// Most cues one list holds.
pub const MAX_CUES: usize = 64;
/// Most actions (waits included) one cue holds.
pub const MAX_CUE_ACTIONS: usize = 16;
/// Most actions (waits included) one list holds across all its cues, so a
/// whole list always fits one protocol record.
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
/// Overlay channels an action may address (`1..=8`).
pub const CUE_OVERLAY_CHANNELS: u8 = 8;

/// One step of a cue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CueAction {
    /// Puts `input` on Preview.
    Preview { input: InputId },
    /// Takes `input` (or, with `None`, the current Preview) to Program with
    /// `effect`: a cut (zero frames), a mix, or a Stinger slot over
    /// `duration_frames`.
    Take {
        input: Option<InputId>,
        effect: TransitionEffect,
        duration_frames: u32,
    },
    /// Takes `input` onto overlay `channel` (`1..=8`).
    OverlayOn { channel: u8, input: InputId },
    /// Takes overlay `channel` off.
    OverlayOff { channel: u8 },
    /// Takes title `input` onto overlay `channel`, playing its In phase.
    TitleOn { input: InputId, channel: u8 },
    /// Sets one text field of title `input`, on or off air.
    TitleText {
        input: InputId,
        field: TitleFieldId,
        text: String,
    },
    /// Takes title `input` off the overlay channel that shows it.
    TitleOff { input: InputId },
    /// Fades Program to black (`active`) or back over `duration_frames`.
    FadeToBlack { active: bool, duration_frames: u32 },
    /// Starts the project playlist now, from its first entry.
    PlaylistStart,
    /// Stops the project playlist.
    PlaylistStop,
    /// Sets `input`'s mixer strip gain.
    AudioGain { input: InputId, gain_millidb: i32 },
    /// Mutes or unmutes `input`'s mixer strip.
    AudioMute { input: InputId, muted: bool },
    /// Starts or stops recorder `recorder`.
    Record { recorder: RecorderSlot, start: bool },
    /// Starts or stops stream target `target`.
    Stream { target: StreamTargetId, start: bool },
    /// Defers the cue's remaining actions by `frames` (`1..=`
    /// [`MAX_CUE_WAIT_FRAMES`]).
    Wait { frames: u32 },
}

impl CueAction {
    /// Every input the action names.
    #[must_use]
    pub const fn input(&self) -> Option<InputId> {
        match self {
            Self::Preview { input }
            | Self::OverlayOn { input, .. }
            | Self::TitleOn { input, .. }
            | Self::TitleText { input, .. }
            | Self::TitleOff { input }
            | Self::AudioGain { input, .. }
            | Self::AudioMute { input, .. } => Some(*input),
            Self::Take { input, .. } => *input,
            Self::OverlayOff { .. }
            | Self::FadeToBlack { .. }
            | Self::PlaylistStart
            | Self::PlaylistStop
            | Self::Record { .. }
            | Self::Stream { .. }
            | Self::Wait { .. } => None,
        }
    }

    /// Whether the action changes Program (a take or a playlist start).
    #[must_use]
    pub const fn takes_program(&self) -> bool {
        matches!(self, Self::Take { .. } | Self::PlaylistStart)
    }
}

/// One cue: a name, its ordered actions, and whether it fires the next cue
/// once its own last action has run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cue {
    /// Stable identity, unique within the list.
    pub id: u64,
    pub name: String,
    pub actions: Vec<CueAction>,
    pub auto_follow: bool,
}

/// A cue whose remaining actions wait for `due_frame`: `action` is the
/// index of the first action still to run, or the action count when only
/// the cue's auto-follow remains after a trailing wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CuePending {
    pub cue: u64,
    pub action: usize,
    pub due_frame: u64,
}

/// An action that could not run when its wait ended; the cue stopped there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CueFault {
    pub cue: u64,
    pub action: usize,
}

/// The persisted run state of one cue list.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CueRunState {
    /// The cue the next GO fires; `None` once the list has run off its end.
    pub standby: Option<u64>,
    /// The cue fired most recently.
    pub live: Option<u64>,
    /// The cue waiting for a frame, if any.
    pub pending: Option<CuePending>,
    /// Every cue fired since the list was created or reset, ascending.
    pub played: Vec<u64>,
    /// The deferred action that last failed to run.
    pub fault: Option<CueFault>,
}

/// One cue list: its definition and run state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CueList {
    /// Stable identity, unique within the project.
    pub id: u64,
    pub name: String,
    pub cues: Vec<Cue>,
    pub state: CueRunState,
}

impl CueList {
    /// The cue with `id`, if the list holds it.
    #[must_use]
    pub fn cue(&self, id: u64) -> Option<&Cue> {
        self.cues.iter().find(|cue| cue.id == id)
    }

    /// The position of cue `id` in the list.
    #[must_use]
    pub fn position(&self, id: u64) -> Option<usize> {
        self.cues.iter().position(|cue| cue.id == id)
    }
}

/// Why a cue list is structurally invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CueListError {
    /// A list or cue name is blank or longer than [`MAX_CUE_NAME_BYTES`].
    InvalidName {
        cue: Option<u64>,
    },
    TooManyCues {
        count: usize,
    },
    DuplicateCue(u64),
    TooManyActions {
        cue: u64,
    },
    TooManyListActions {
        count: usize,
    },
    TooMuchText {
        bytes: usize,
    },
    /// Action `action` of cue `cue` is out of range.
    InvalidAction {
        cue: u64,
        action: usize,
    },
    /// Cue `cue` takes Program more than once on one frame.
    ConflictingTakes {
        cue: u64,
    },
    /// The run state names a cue the list does not hold, or is inconsistent.
    InvalidState,
}

impl fmt::Display for CueListError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { cue: None } => write!(
                formatter,
                "cue list name must be 1 to {MAX_CUE_NAME_BYTES} bytes and not blank"
            ),
            Self::InvalidName { cue: Some(cue) } => write!(
                formatter,
                "cue {cue} name must be 1 to {MAX_CUE_NAME_BYTES} bytes and not blank"
            ),
            Self::TooManyCues { count } => {
                write!(
                    formatter,
                    "cue list has {count} cues; the limit is {MAX_CUES}"
                )
            }
            Self::DuplicateCue(cue) => write!(formatter, "cue {cue} is duplicated"),
            Self::TooManyActions { cue } => write!(
                formatter,
                "cue {cue} has more than {MAX_CUE_ACTIONS} actions"
            ),
            Self::TooManyListActions { count } => write!(
                formatter,
                "cue list has {count} actions; the limit is {MAX_CUE_LIST_ACTIONS}"
            ),
            Self::TooMuchText { bytes } => write!(
                formatter,
                "cue list carries {bytes} bytes of title text; the limit is \
                 {MAX_CUE_LIST_TEXT_BYTES}"
            ),
            Self::InvalidAction { cue, action } => {
                write!(formatter, "cue {cue} action {action} is out of range")
            }
            Self::ConflictingTakes { cue } => write!(
                formatter,
                "cue {cue} takes Program more than once on one frame"
            ),
            Self::InvalidState => formatter.write_str("cue list run state is inconsistent"),
        }
    }
}

impl std::error::Error for CueListError {}

/// Whether `name` is a valid list or cue name.
#[must_use]
pub fn cue_name_is_valid(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= MAX_CUE_NAME_BYTES
}

fn action_is_valid(action: &CueAction) -> bool {
    let transition = |frames: u32| (1..=MAX_CUE_TRANSITION_FRAMES).contains(&frames);
    let channel = |channel: u8| (1..=CUE_OVERLAY_CHANNELS).contains(&channel);
    match action {
        CueAction::Take {
            effect,
            duration_frames,
            ..
        } => match effect {
            TransitionEffect::Cut => *duration_frames == 0,
            _ => transition(*duration_frames),
        },
        CueAction::OverlayOn {
            channel: number, ..
        }
        | CueAction::OverlayOff { channel: number }
        | CueAction::TitleOn {
            channel: number, ..
        } => channel(*number),
        CueAction::TitleText { text, .. } => text.len() <= MAX_CUE_TEXT_BYTES,
        CueAction::FadeToBlack {
            duration_frames, ..
        } => transition(*duration_frames),
        CueAction::AudioGain { gain_millidb, .. } => {
            crate::InputGainMilliDb::new(*gain_millidb).is_some()
        }
        CueAction::Wait { frames } => (1..=MAX_CUE_WAIT_FRAMES).contains(frames),
        CueAction::Preview { .. }
        | CueAction::TitleOff { .. }
        | CueAction::PlaylistStart
        | CueAction::PlaylistStop
        | CueAction::AudioMute { .. }
        | CueAction::Record { .. }
        | CueAction::Stream { .. } => true,
    }
}

/// Checks the structural bounds of a list definition: its name, cue count,
/// unique cue ids, names, action counts and ranges, its text budget, and
/// that no cue takes Program twice between two waits. Whether each
/// referenced input or stream target exists is checked against the owning
/// project or show.
///
/// # Errors
///
/// Returns the first [`CueListError`] in list order.
pub fn validate_cue_list_definition(name: &str, cues: &[Cue]) -> Result<(), CueListError> {
    if !cue_name_is_valid(name) {
        return Err(CueListError::InvalidName { cue: None });
    }
    if cues.len() > MAX_CUES {
        return Err(CueListError::TooManyCues { count: cues.len() });
    }
    let mut actions = 0_usize;
    let mut text = 0_usize;
    for (index, cue) in cues.iter().enumerate() {
        if cues[..index].iter().any(|other| other.id == cue.id) {
            return Err(CueListError::DuplicateCue(cue.id));
        }
        if !cue_name_is_valid(&cue.name) {
            return Err(CueListError::InvalidName { cue: Some(cue.id) });
        }
        if cue.actions.len() > MAX_CUE_ACTIONS {
            return Err(CueListError::TooManyActions { cue: cue.id });
        }
        actions += cue.actions.len();
        let mut takes = 0_usize;
        for (position, action) in cue.actions.iter().enumerate() {
            if !action_is_valid(action) {
                return Err(CueListError::InvalidAction {
                    cue: cue.id,
                    action: position,
                });
            }
            if let CueAction::TitleText { text: value, .. } = action {
                text += value.len();
            }
            if matches!(action, CueAction::Wait { .. }) {
                takes = 0;
            } else if action.takes_program() {
                takes += 1;
                if takes > 1 {
                    return Err(CueListError::ConflictingTakes { cue: cue.id });
                }
            }
        }
    }
    if actions > MAX_CUE_LIST_ACTIONS {
        return Err(CueListError::TooManyListActions { count: actions });
    }
    if text > MAX_CUE_LIST_TEXT_BYTES {
        return Err(CueListError::TooMuchText { bytes: text });
    }
    Ok(())
}

/// Checks a whole list: its definition and that its run state names only
/// cues it holds (a pending cue resumes at an action it has, after a wait).
///
/// # Errors
///
/// Returns the first [`CueListError`].
pub fn validate_cue_list(list: &CueList) -> Result<(), CueListError> {
    validate_cue_list_definition(&list.name, &list.cues)?;
    let state = &list.state;
    let holds = |cue: u64| list.cue(cue).is_some();
    let pending_valid = state.pending.is_none_or(|pending| {
        list.cue(pending.cue).is_some_and(|cue| {
            pending.action > 0
                && pending.action <= cue.actions.len()
                && (pending.action < cue.actions.len() || cue.auto_follow)
                && matches!(cue.actions[pending.action - 1], CueAction::Wait { .. })
        })
    });
    let fault_valid = state.fault.is_none_or(|fault| {
        list.cue(fault.cue)
            .is_some_and(|cue| fault.action < cue.actions.len())
    });
    let played_valid = state.played.windows(2).all(|pair| pair[0] < pair[1])
        && state.played.iter().all(|cue| holds(*cue));
    if !state.standby.is_none_or(holds)
        || !state.live.is_none_or(holds)
        || !pending_valid
        || !fault_valid
        || !played_valid
    {
        return Err(CueListError::InvalidState);
    }
    Ok(())
}

/// Checks the project-level list rules: at most [`MAX_CUE_LISTS`] lists
/// with unique ids, each structurally valid.
///
/// # Errors
///
/// Returns the first [`CueListError`], or `Err(None)` for too many lists
/// or a duplicate list id.
pub fn validate_cue_lists(lists: &[CueList]) -> Result<(), Option<CueListError>> {
    if lists.len() > MAX_CUE_LISTS {
        return Err(None);
    }
    for (index, list) in lists.iter().enumerate() {
        if lists[..index].iter().any(|other| other.id == list.id) {
            return Err(None);
        }
        validate_cue_list(list).map_err(Some)?;
    }
    Ok(())
}
