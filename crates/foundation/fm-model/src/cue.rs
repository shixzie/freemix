//! Operator cue lists (RC-017, schema 48): ordered cues an operator fires
//! with one GO, each a bounded list of typed show actions and waits, plus
//! the persisted standby pointer and run state that drive them.
//!
//! A cue's actions up to its first wait run on one engine frame; a wait
//! defers the rest by a whole number of frames. A millisecond delay
//! ([`CueAction::Delay`]) is converted to frames at the show's frame rate
//! when its cue fires, rounded up to the next whole frame (so a delay never
//! ends early) and at least one frame; see [`delay_frames`]. A cue that follows on
//! (`auto_follow`) fires the next cue as soon as its own last action has
//! run, on the same frame when it ends without a wait. That frame's
//! actions are one frame's actions across both cues: a list whose
//! waitless follow would take Program twice on one frame is refused
//! ([`CueListError::ConflictingFollowTakes`]) rather than deferring the
//! second take. Time is measured in
//! engine frames, so every deferred action lands on an exact frame number
//! and the state is persisted so a restarted authority resumes it.
//!
//! References are checked against the owning project: every input a cue
//! names must exist (and so cannot be removed while a cue names it), every
//! stream target and project script must exist (schema 55: a script a cue
//! runs cannot be removed while a cue names it), and channel, slot, and
//! recorder numbers are range-checked here.
//!
//! A [`CueAction::RunScript`] runs one project script (RC-011) as one step
//! of its cue, on the frame the step runs: the script's actions are that
//! step's actions, planned on the show as it stands after the earlier
//! actions of the frame, all or nothing. A script cannot run a cue or
//! another script, so the nesting is at most one level deep. Whether the
//! step takes Program depends on what the script does when it runs, so it
//! is not counted by [`validate_cue_list_definition`]; the engine still
//! refuses a second take of Program on one frame when the step runs.

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
/// Most script steps ([`CueAction::RunScript`]) one list holds (schema 55),
/// so one frame never runs more than this many scripts.
pub const MAX_CUE_LIST_SCRIPT_STEPS: usize = 8;
/// Longest list or cue name, in UTF-8 bytes.
pub const MAX_CUE_NAME_BYTES: usize = 64;
/// Longest title text one action sets, in UTF-8 bytes.
pub const MAX_CUE_TEXT_BYTES: usize = 128;
/// Most title text bytes one list carries across all its actions.
pub const MAX_CUE_LIST_TEXT_BYTES: usize = 2_048;
/// Largest magnitude a title number action sets or adds (schema 59), and
/// largest magnitude an added value may reach.
pub const MAX_CUE_TITLE_NUMBER: i64 = 999_999_999_999;
/// Longest wait, in frames.
pub const MAX_CUE_WAIT_FRAMES: u32 = 216_000;
/// Longest millisecond delay (one hour).
pub const MAX_CUE_DELAY_MS: u32 = 3_600_000;
/// Longest an armed GO waits, in milliseconds (24 hours, schema 58).
pub const MAX_CUE_ARM_MS: u32 = 86_400_000;
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
    /// Sets or steps one number field of title `input`, on or off air
    /// (schema 59): a data update such as a score. A text field receives
    /// decimal text; stepping it needs it to hold a plain decimal integer
    /// (or nothing, read as zero; see [`title_text_number`]).
    TitleNumber {
        input: InputId,
        field: TitleFieldId,
        update: CueNumberUpdate,
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
    /// Runs `operation` on media input `input`'s transport.
    Media {
        input: InputId,
        operation: CueMediaOperation,
    },
    /// Steps paged input `input` (a slideshow) by `navigation` (IN-009,
    /// schema 60).
    Page {
        input: InputId,
        navigation: fm_types::paged::PagedNavigation,
    },
    /// Defers the cue's remaining actions by `frames` (`1..=`
    /// [`MAX_CUE_WAIT_FRAMES`]).
    Wait { frames: u32 },
    /// Defers the cue's remaining actions by `millis` milliseconds (`1..=`
    /// [`MAX_CUE_DELAY_MS`]), converted to frames by [`delay_frames`].
    Delay { millis: u32 },
    /// Runs project script `script` (nonzero) as one step (schema 55).
    RunScript { script: u64 },
}

/// How a [`CueAction::TitleNumber`] changes its field (schema 59).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CueNumberUpdate {
    /// Sets the field to the value (`-`[`MAX_CUE_TITLE_NUMBER`]`..=`
    /// [`MAX_CUE_TITLE_NUMBER`]).
    Set(i64),
    /// Adds the nonzero delta (same magnitude bound) to the field's value
    /// when the action runs; a result beyond [`MAX_CUE_TITLE_NUMBER`] is
    /// refused.
    Add(i64),
}

impl CueNumberUpdate {
    /// Whether the value or delta is in range.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        match self {
            Self::Set(value) => value.unsigned_abs() <= MAX_CUE_TITLE_NUMBER.unsigned_abs(),
            Self::Add(delta) => {
                delta != 0 && delta.unsigned_abs() <= MAX_CUE_TITLE_NUMBER.unsigned_abs()
            }
        }
    }

    /// The field's value after the update of `current`, or `None` when it
    /// leaves `-`[`MAX_CUE_TITLE_NUMBER`]`..=`[`MAX_CUE_TITLE_NUMBER`].
    #[must_use]
    pub const fn apply(self, current: i64) -> Option<i64> {
        let value = match self {
            Self::Set(value) => value,
            Self::Add(delta) => match current.checked_add(delta) {
                Some(value) => value,
                None => return None,
            },
        };
        if value.unsigned_abs() <= MAX_CUE_TITLE_NUMBER.unsigned_abs() {
            Some(value)
        } else {
            None
        }
    }
}

/// The integer a title text field holds for a number update (schema 59):
/// blank text is zero, otherwise an optional `-` and 1 to 19 ASCII digits,
/// surrounding whitespace ignored; anything else is `None`.
#[must_use]
pub fn title_text_number(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || digits.len() > 19 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A media transport operation a cue runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CueMediaOperation {
    Play,
    Pause,
    /// Seeks to source frame `frame` (clamped to the trimmed range).
    Seek {
        frame: u64,
    },
    /// Seeks to the start of the trimmed range and plays.
    Restart,
}

/// The whole frames a delay of `millis` milliseconds lasts at `rate`:
/// `ceil(millis * rate / 1000)`, at least one, so a delay never ends before
/// its time and never collapses to zero frames.
#[must_use]
pub fn delay_frames(millis: u32, rate: fm_types::FrameRate) -> u32 {
    let numerator = u128::from(millis) * u128::from(rate.numerator());
    let denominator = u128::from(rate.denominator()) * 1_000;
    u32::try_from(numerator.div_ceil(denominator))
        .unwrap_or(u32::MAX)
        .max(1)
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

    /// The project script the action runs.
    #[must_use]
    pub const fn script(&self) -> Option<u64> {
        match self {
            Self::RunScript { script } => Some(*script),
            _ => None,
        }
    }

    /// Whether the action is a wait (in frames or milliseconds).
    #[must_use]
    pub const fn is_wait(&self) -> bool {
        matches!(self, Self::Wait { .. } | Self::Delay { .. })
    }

    /// Whether the action changes Program (a take or a playlist start). A
    /// script step is not counted: what it takes is known only when it runs.
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CueFault {
    pub cue: u64,
    pub action: usize,
    /// Why the action was refused (schema 57).
    pub detail: fm_command::FaultDetail,
}

/// A GO of `cue` armed to fire on `due_frame` (schema 58): the cue clock
/// fires it on that engine frame exactly as a GO would.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CueArmed {
    pub cue: u64,
    pub due_frame: u64,
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
    /// The deferred action (or armed GO action) that last failed to run.
    pub fault: Option<CueFault>,
    /// The GO armed for a later frame (schema 58).
    pub armed: Option<CueArmed>,
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

/// Every action a run starting at action `first_action` of cue `cue` in
/// `cues` can perform, in order: the cue's actions from `first_action` on
/// (across its waits), then, while the cue that ends auto-follows, every
/// action of the next cue in list order. This is exactly what a GO of `cue`
/// (with `first_action` zero), or a waiting cue resumed at `first_action`,
/// runs over all its later clock advances, unless a stop, reset, another
/// GO, or a refused action ends it earlier. Empty when `cues` holds no such
/// cue.
#[must_use]
pub fn cue_chain_actions(cues: &[Cue], cue: u64, first_action: usize) -> Vec<&CueAction> {
    let Some(start) = cues.iter().position(|candidate| candidate.id == cue) else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    let mut skip = first_action;
    for cue in &cues[start..] {
        actions.extend(cue.actions.iter().skip(skip));
        skip = 0;
        if !cue.auto_follow {
            break;
        }
    }
    actions
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
    /// The list runs more than [`MAX_CUE_LIST_SCRIPT_STEPS`] scripts.
    TooManyScriptSteps {
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
    /// Cue `cue` takes Program and auto-follows, with no wait between, into
    /// cue `follower`, which takes Program again before its first wait:
    /// both takes would land on one frame.
    ConflictingFollowTakes {
        cue: u64,
        follower: u64,
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
            Self::TooManyScriptSteps { count } => write!(
                formatter,
                "cue list runs {count} scripts; the limit is {MAX_CUE_LIST_SCRIPT_STEPS}"
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
            Self::ConflictingFollowTakes { cue, follower } => write!(
                formatter,
                "cue {cue} auto-follows into cue {follower} and both take Program on one frame"
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

/// Whether one action is within its ranges (durations, channels, text,
/// gain, waits); references are checked against the owning show.
#[must_use]
pub fn cue_action_is_valid(action: &CueAction) -> bool {
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
        CueAction::TitleNumber { update, .. } => update.is_valid(),
        CueAction::FadeToBlack {
            duration_frames, ..
        } => transition(*duration_frames),
        CueAction::AudioGain { gain_millidb, .. } => {
            crate::InputGainMilliDb::new(*gain_millidb).is_some()
        }
        CueAction::Wait { frames } => (1..=MAX_CUE_WAIT_FRAMES).contains(frames),
        CueAction::Delay { millis } => (1..=MAX_CUE_DELAY_MS).contains(millis),
        CueAction::RunScript { script } => *script != 0,
        CueAction::Page { navigation, .. } => match navigation {
            fm_types::paged::PagedNavigation::Goto(page) => {
                *page < fm_types::paged::MAX_PAGED_PAGES
            }
            _ => true,
        },
        CueAction::Preview { .. }
        | CueAction::Media { .. }
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
/// that Program is taken at most once per frame. The actions up to a
/// cue's first wait run on one frame, and so do the actions after its last
/// wait together with the cue it auto-follows into (up to that cue's first
/// wait, and on through further waitless follows): one take per such run.
/// Whether each
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
    let mut scripts = 0_usize;
    // The cue that took Program on the frame an auto-follow carries into
    // the next cue, if any.
    let mut carried: Option<u64> = None;
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
        let mut taker = carried;
        for (position, action) in cue.actions.iter().enumerate() {
            if !cue_action_is_valid(action) {
                return Err(CueListError::InvalidAction {
                    cue: cue.id,
                    action: position,
                });
            }
            if let CueAction::TitleText { text: value, .. } = action {
                text += value.len();
            }
            if action.script().is_some() {
                scripts += 1;
            }
            if action.is_wait() {
                taker = None;
            } else if action.takes_program() {
                match taker {
                    Some(leader) if leader == cue.id => {
                        return Err(CueListError::ConflictingTakes { cue: cue.id });
                    }
                    Some(leader) => {
                        return Err(CueListError::ConflictingFollowTakes {
                            cue: leader,
                            follower: cue.id,
                        });
                    }
                    None => taker = Some(cue.id),
                }
            }
        }
        carried = if cue.auto_follow { taker } else { None };
    }
    if actions > MAX_CUE_LIST_ACTIONS {
        return Err(CueListError::TooManyListActions { count: actions });
    }
    if text > MAX_CUE_LIST_TEXT_BYTES {
        return Err(CueListError::TooMuchText { bytes: text });
    }
    if scripts > MAX_CUE_LIST_SCRIPT_STEPS {
        return Err(CueListError::TooManyScriptSteps { count: scripts });
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
                && cue.actions[pending.action - 1].is_wait()
        })
    });
    let fault_valid = state.fault.as_ref().is_none_or(|fault| {
        fault.detail.is_valid()
            && list
                .cue(fault.cue)
                .is_some_and(|cue| fault.action < cue.actions.len())
    });
    let played_valid = state.played.windows(2).all(|pair| pair[0] < pair[1])
        && state.played.iter().all(|cue| holds(*cue));
    if !state.standby.is_none_or(holds)
        || !state.live.is_none_or(holds)
        || !pending_valid
        || !fault_valid
        || !played_valid
        || !state.armed.is_none_or(|armed| holds(armed.cue))
    {
        return Err(CueListError::InvalidState);
    }
    Ok(())
}

/// Checks the project-level list rules: at most [`MAX_CUE_LISTS`] lists
/// with unique nonzero ids (the cue clock's list `0` is the waiting script
/// run, schema 56), each structurally valid.
///
/// # Errors
///
/// Returns the first [`CueListError`], or `Err(None)` for too many lists,
/// a zero list id, or a duplicate list id.
pub fn validate_cue_lists(lists: &[CueList]) -> Result<(), Option<CueListError>> {
    if lists.len() > MAX_CUE_LISTS || lists.iter().any(|list| list.id == 0) {
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
