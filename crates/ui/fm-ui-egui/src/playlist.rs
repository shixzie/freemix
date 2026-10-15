//! Playlist side tab: the frame-driven project playlist (`OR-015`).
//!
//! The tab lists the replicated playlist ([`fm_ui_model::ProjectState::playlist`])
//! with each entry's input, transition, and duration as timecode, highlights
//! the entry on air (red) and the one that follows it (green), and counts
//! down the on-air entry's remaining time. Transport keys (Start, Stop,
//! Pause/Resume, Previous, Next, and Jump on a row) need the transition
//! permission; editing the list (add, remove, reorder, durations,
//! transitions, enable, loop mode) needs `edit_project` and a stopped or
//! finished playlist, because the engine refuses list changes while it runs.
//!
//! Every edit sends the complete list built from the replicated playlist, so
//! nothing is drafted locally except the entry form. Durations are engine
//! frames, shown as `HH:MM:SS:FF` at the show's nominal integer frame rate.
//!
//! The countdown extrapolates from the frame the latest playlist status was
//! observed at (its `frame`), anchored to when Studio first saw it, at the
//! snapshot's frame rate.

use core::time::Duration;
use std::time::Instant;

use egui::{
    Button, Color32, ComboBox, CornerRadius, FontFamily, FontId, Frame, Key, RichText, Sense,
    Stroke, StrokeKind, TextEdit, Ui, Vec2, pos2,
};
use fm_protocol::{
    MAX_PLAYLIST_ENTRIES, MAX_PLAYLIST_ENTRY_FRAMES, MAX_PLAYLIST_TRANSITION_FRAMES,
    PlaylistEntrySpec, PlaylistLoopMode, PlaylistRunState, PlaylistTransitionKind, WireInputId,
};
use fm_types::{FrameRate, InputId};
use fm_ui_model::{ClientView, PlaylistStatus};

use crate::edit_history::stinger_edit_lock_reason;
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::monitors::input_name;
use crate::theme::{
    ACCENT_FILL, AMBER, ERROR, PREVIEW, PROGRAM, PROGRAM_FILL, Tone, accent_color, border_color,
    border_strong_color, caption, elided, empty_state, field_label, graphite_color, mono,
    muted_color, raised_color, section_label, status_chip, text_color, text_dim_color,
    widget_color, widget_hover_color,
};
use crate::{StudioConnectionStatus, StudioIntent, StudioUiState};

/// A playlist operator action. Studio maps each to one protocol command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlaylistIntent {
    /// Replaces the list and loop mode (`playlist_set`): a project edit,
    /// accepted only while the playlist is stopped or finished.
    Set {
        loop_mode: PlaylistLoopMode,
        entries: Vec<PlaylistEntrySpec>,
    },
    /// Starts now, from `from` or else the first enabled entry.
    Start {
        from: Option<u64>,
    },
    Stop,
    Pause,
    Resume,
    Next,
    Previous,
    /// Takes one enabled entry to air while the playlist runs.
    Jump {
        entry: u64,
    },
}

/// The playlist's run phase, from its replicated state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistPhase {
    Stopped,
    Armed,
    Playing,
    Paused,
    Finished,
}

impl PlaylistPhase {
    #[must_use]
    pub const fn of(state: PlaylistRunState) -> Self {
        match state {
            PlaylistRunState::Stopped => Self::Stopped,
            PlaylistRunState::Armed { .. } => Self::Armed,
            PlaylistRunState::Playing { .. } => Self::Playing,
            PlaylistRunState::Paused { .. } => Self::Paused,
            PlaylistRunState::Finished { .. } => Self::Finished,
        }
    }

    /// Whether the engine refuses list edits in this phase.
    #[must_use]
    pub const fn running(self) -> bool {
        matches!(self, Self::Armed | Self::Playing | Self::Paused)
    }

    /// The phase chip's catalog word (English; drawing translates it).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stopped => Msg::Stopped.en(),
            Self::Armed => Msg::PlaylistStateArmed.en(),
            Self::Playing => Msg::PlaylistStatePlaying.en(),
            Self::Paused => Msg::PlaylistStatePaused.en(),
            Self::Finished => Msg::PlaylistStateFinished.en(),
        }
    }

    pub(crate) const fn tone(self) -> Tone {
        match self {
            Self::Stopped | Self::Finished => Tone::Idle,
            Self::Armed | Self::Paused => Tone::Pending,
            Self::Playing => Tone::OnAir,
        }
    }
}

/// The entry on air: playing or paused.
#[must_use]
pub const fn on_air_entry(playlist: &PlaylistStatus) -> Option<u64> {
    match playlist.state {
        PlaylistRunState::Playing { entry, .. } | PlaylistRunState::Paused { entry, .. } => {
            Some(entry)
        }
        _ => None,
    }
}

fn index_of(playlist: &PlaylistStatus, id: u64) -> Option<usize> {
    playlist.entries.iter().position(|entry| entry.id == id)
}

/// The entry that goes on air when the current one ends (or, for an armed
/// playlist, when it starts), following the loop mode. `None` when the
/// playlist ends after the on-air entry or is not running.
#[must_use]
pub fn next_entry(playlist: &PlaylistStatus) -> Option<u64> {
    let entries = &playlist.entries;
    match playlist.state {
        PlaylistRunState::Armed {
            from: Some(from), ..
        } => Some(from),
        PlaylistRunState::Armed { from: None, .. } => entries
            .iter()
            .find(|entry| entry.enabled)
            .map(|entry| entry.id),
        PlaylistRunState::Playing { entry, .. } | PlaylistRunState::Paused { entry, .. } => {
            let index = index_of(playlist, entry)?;
            match playlist.loop_mode {
                PlaylistLoopMode::One if entries[index].enabled => Some(entry),
                PlaylistLoopMode::All => entries[index + 1..]
                    .iter()
                    .chain(&entries[..=index])
                    .find(|candidate| candidate.enabled)
                    .map(|candidate| candidate.id),
                PlaylistLoopMode::Off | PlaylistLoopMode::One => entries[index + 1..]
                    .iter()
                    .find(|candidate| candidate.enabled)
                    .map(|candidate| candidate.id),
            }
        }
        PlaylistRunState::Stopped | PlaylistRunState::Finished { .. } => None,
    }
}

/// The entry the Next key takes to air: the next enabled entry, wrapping
/// only with loop All (the engine's rule, also under loop One).
fn next_command_target(playlist: &PlaylistStatus) -> Option<u64> {
    let index = index_of(playlist, on_air_entry(playlist)?)?;
    let entries = &playlist.entries;
    entries[index + 1..]
        .iter()
        .find(|entry| entry.enabled)
        .or_else(|| {
            (playlist.loop_mode == PlaylistLoopMode::All)
                .then(|| entries.iter().find(|entry| entry.enabled))
                .flatten()
        })
        .map(|entry| entry.id)
}

/// The entry the Previous key takes to air, wrapping only with loop All.
fn previous_command_target(playlist: &PlaylistStatus) -> Option<u64> {
    let index = index_of(playlist, on_air_entry(playlist)?)?;
    let entries = &playlist.entries;
    entries[..index]
        .iter()
        .rev()
        .find(|entry| entry.enabled)
        .or_else(|| {
            (playlist.loop_mode == PlaylistLoopMode::All)
                .then(|| entries.iter().rev().find(|entry| entry.enabled))
                .flatten()
        })
        .map(|entry| entry.id)
}

/// How one row is highlighted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryRole {
    /// On air (red), playing or paused.
    OnAir,
    /// Goes on air next (green).
    Next,
    Idle,
}

#[must_use]
pub fn entry_role(playlist: &PlaylistStatus, id: u64) -> EntryRole {
    if on_air_entry(playlist) == Some(id) {
        EntryRole::OnAir
    } else if next_entry(playlist) == Some(id) {
        EntryRole::Next
    } else {
        EntryRole::Idle
    }
}

/// The countdown the tab shows, in frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Countdown {
    /// Frames left of the on-air entry, out of its `duration`.
    Remaining { frames: u64, duration: u64 },
    /// Frames until an armed playlist puts its first entry on air.
    StartsIn(u64),
}

/// The countdown at engine frame `now`: a playing entry counts down from
/// its start, a paused one holds, and an armed playlist counts to its start.
#[must_use]
pub fn countdown(playlist: &PlaylistStatus, now: u64) -> Option<Countdown> {
    let duration =
        |entry: u64| index_of(playlist, entry).map(|index| playlist.entries[index].duration_frames);
    match playlist.state {
        PlaylistRunState::Playing {
            entry,
            started_frame,
            ..
        } => {
            let duration = duration(entry)?;
            let end = started_frame.saturating_add(duration);
            Some(Countdown::Remaining {
                frames: end.saturating_sub(now).min(duration),
                duration,
            })
        }
        PlaylistRunState::Paused {
            entry,
            elapsed_frames,
            ..
        } => {
            let duration = duration(entry)?;
            Some(Countdown::Remaining {
                frames: duration.saturating_sub(elapsed_frames),
                duration,
            })
        }
        PlaylistRunState::Armed { start_frame, .. } => {
            Some(Countdown::StartsIn(start_frame.saturating_sub(now)))
        }
        PlaylistRunState::Stopped | PlaylistRunState::Finished { .. } => None,
    }
}

/// The engine frame `elapsed` after `anchor_frame` at `rate`.
#[must_use]
pub fn estimated_frame(anchor_frame: u64, elapsed: Duration, rate: FrameRate) -> u64 {
    let frames = elapsed.as_nanos() * u128::from(rate.numerator())
        / (u128::from(rate.denominator()) * 1_000_000_000);
    anchor_frame.saturating_add(u64::try_from(frames).unwrap_or(u64::MAX))
}

/// Whole frames per timecode second: the rate rounded up (30 for 29.97).
#[must_use]
pub fn nominal_fps(rate: FrameRate) -> u64 {
    u64::from(rate.numerator())
        .div_ceil(u64::from(rate.denominator()))
        .max(1)
}

/// Formats a frame count as non-drop `HH:MM:SS:FF` at the nominal rate.
/// Hours grow past 99 instead of wrapping; rates above 100 fps use three
/// frame digits.
#[must_use]
pub fn timecode(frames: u64, rate: FrameRate) -> String {
    let fps = nominal_fps(rate);
    let width = if fps > 100 { 3 } else { 2 };
    let seconds = frames / fps;
    format!(
        "{:02}:{:02}:{:02}:{:0width$}",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60,
        frames % fps
    )
}

/// Formats a countdown: the timecode of the frames left, with a leading
/// minus.
///
/// Convention: as in broadcast playout clocks, time still to run is signed
/// negative and a duration is unsigned. Every countdown on the tab uses this
/// one form: the status card's `REMAINING` (and `STARTS IN`) clock and the
/// on-air row read the same `-HH:MM:SS:FF`, while entry durations, the
/// card's `of HH:MM:SS:FF`, and the form's duration field never carry a
/// sign.
#[must_use]
pub fn countdown_timecode(frames: u64, rate: FrameRate) -> String {
    format!("-{}", timecode(frames, rate))
}

/// Parses an operator duration: `SS` (any number of seconds), `MM:SS`,
/// `HH:MM:SS`, or `HH:MM:SS:FF`, at the nominal rate. Returns `None`
/// unless it is between one frame and [`MAX_PLAYLIST_ENTRY_FRAMES`].
#[must_use]
pub fn parse_duration(text: &str, rate: FrameRate) -> Option<u64> {
    let parts = text
        .trim()
        .split(':')
        .map(|part| {
            (!part.is_empty() && part.len() <= 12 && part.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| part.parse::<u64>().ok())
                .flatten()
        })
        .collect::<Option<Vec<_>>>()?;
    let fps = nominal_fps(rate);
    let (hours, minutes, seconds, frames) = match parts[..] {
        [seconds] => (0, 0, seconds, 0),
        [minutes, seconds] => (0, minutes, seconds, 0),
        [hours, minutes, seconds] => (hours, minutes, seconds, 0),
        [hours, minutes, seconds, frames] => (hours, minutes, seconds, frames),
        _ => return None,
    };
    if (parts.len() > 1 && seconds >= 60) || (parts.len() > 2 && minutes >= 60) || frames >= fps {
        return None;
    }
    let total = hours
        .checked_mul(3_600)?
        .checked_add(minutes.checked_mul(60)?)?
        .checked_add(seconds)?
        .checked_mul(fps)?
        .checked_add(frames)?;
    (1..=MAX_PLAYLIST_ENTRY_FRAMES)
        .contains(&total)
        .then_some(total)
}

/// The transition frames an entry carries: zero for a cut, otherwise
/// `frames` clamped to the protocol bound and the entry's duration.
#[must_use]
pub fn transition_frames_for(kind: PlaylistTransitionKind, frames: u32, duration: u64) -> u32 {
    if kind == PlaylistTransitionKind::Cut {
        return 0;
    }
    let longest = u32::try_from(duration)
        .unwrap_or(u32::MAX)
        .min(MAX_PLAYLIST_TRANSITION_FRAMES);
    frames.clamp(1, longest.max(1))
}

/// An unused entry identifier: one past the largest, or the smallest free
/// one once the largest is `u64::MAX`.
#[must_use]
pub fn new_entry_id(entries: &[PlaylistEntrySpec]) -> u64 {
    match entries.iter().map(|entry| entry.id).max() {
        None => 1,
        Some(u64::MAX) => (1..u64::MAX)
            .find(|id| entries.iter().all(|entry| entry.id != *id))
            .unwrap_or(1),
        Some(largest) => largest + 1,
    }
}

/// The catalog word for a transition kind.
#[must_use]
pub const fn transition_label(kind: PlaylistTransitionKind) -> &'static str {
    match kind {
        PlaylistTransitionKind::Cut => Msg::TransitionCut.en(),
        PlaylistTransitionKind::Fade => Msg::TransitionFade.en(),
        PlaylistTransitionKind::AlphaFade => Msg::TransitionAlphaFade.en(),
        PlaylistTransitionKind::Wipe => Msg::TransitionWipe.en(),
        PlaylistTransitionKind::Slide => Msg::TransitionSlide.en(),
        PlaylistTransitionKind::Zoom => Msg::TransitionZoom.en(),
    }
}

const TRANSITIONS: [PlaylistTransitionKind; 6] = [
    PlaylistTransitionKind::Cut,
    PlaylistTransitionKind::Fade,
    PlaylistTransitionKind::AlphaFade,
    PlaylistTransitionKind::Wipe,
    PlaylistTransitionKind::Slide,
    PlaylistTransitionKind::Zoom,
];

/// `Cut`, or `Fade 15 fr`, in the console language.
#[must_use]
pub fn entry_transition_text(l: Locale, entry: &PlaylistEntrySpec) -> String {
    let name = l.tr(transition_label(entry.transition));
    if entry.transition == PlaylistTransitionKind::Cut {
        name.to_owned()
    } else {
        l.format(
            Msg::PlaylistTransitionFrames,
            &[
                ("transition", Arg::Str(name)),
                ("frames", Arg::Count(u64::from(entry.transition_frames))),
            ],
        )
    }
}

/// Why playlist transport is unavailable for the session, if it is.
#[must_use]
pub fn control_block(state: &StudioUiState) -> Option<&'static str> {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.view.is_some()
        && state.can_transition;
    (!ready).then(|| unavailable_reason(state, Capability::Transition))
}

/// Why the list cannot be edited, if it cannot: the session gates, the
/// project padlock, then a running playlist.
#[must_use]
pub fn edit_block(
    state: &StudioUiState,
    playlist: Option<&PlaylistStatus>,
) -> Option<&'static str> {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.view.is_some()
        && state.can_edit_project;
    if !ready {
        return Some(unavailable_reason(state, Capability::EditProject));
    }
    if let Some(reason) = stinger_edit_lock_reason(state) {
        return Some(reason);
    }
    match playlist {
        None => Some(Msg::PlaylistWaiting.en()),
        Some(playlist) if PlaylistPhase::of(playlist.state).running() => {
            Some(Msg::PlaylistReasonRunning.en())
        }
        Some(_) => None,
    }
}

/// Each transport key's intent, or why it is unavailable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportAvailability {
    pub start: Result<PlaylistIntent, &'static str>,
    pub stop: Result<PlaylistIntent, &'static str>,
    /// Pause while playing, Resume while paused.
    pub pause_resume: Result<PlaylistIntent, &'static str>,
    pub previous: Result<PlaylistIntent, &'static str>,
    pub next: Result<PlaylistIntent, &'static str>,
}

/// The transport keys for `playlist` under the session `block`.
#[must_use]
pub fn transport_availability(
    playlist: &PlaylistStatus,
    block: Option<&'static str>,
) -> TransportAvailability {
    let phase = PlaylistPhase::of(playlist.state);
    let gate = |result: Result<PlaylistIntent, &'static str>| match block {
        Some(reason) => Err(reason),
        None => result,
    };
    let start = if phase.running() {
        Err(Msg::PlaylistReasonActive.en())
    } else if playlist.entries.iter().any(|entry| entry.enabled) {
        Ok(PlaylistIntent::Start { from: None })
    } else {
        Err(Msg::PlaylistReasonNoEnabled.en())
    };
    let stop = if phase.running() {
        Ok(PlaylistIntent::Stop)
    } else {
        Err(Msg::PlaylistReasonNotOnAir.en())
    };
    let pause_resume = match phase {
        PlaylistPhase::Playing => Ok(PlaylistIntent::Pause),
        PlaylistPhase::Paused => Ok(PlaylistIntent::Resume),
        _ => Err(Msg::PlaylistReasonNotOnAir.en()),
    };
    let on_air = on_air_entry(playlist).is_some();
    let previous = match previous_command_target(playlist) {
        Some(_) => Ok(PlaylistIntent::Previous),
        None if on_air => Err(Msg::PlaylistReasonNoPrevious.en()),
        None => Err(Msg::PlaylistReasonNotOnAir.en()),
    };
    let next = match next_command_target(playlist) {
        Some(_) => Ok(PlaylistIntent::Next),
        None if on_air => Err(Msg::PlaylistReasonNoNext.en()),
        None => Err(Msg::PlaylistReasonNotOnAir.en()),
    };
    TransportAvailability {
        start: gate(start),
        stop: gate(stop),
        pause_resume: gate(pause_resume),
        previous: gate(previous),
        next: gate(next),
    }
}

/// A row's play key: Jump while an entry is on air, Start from the entry
/// while stopped or finished.
///
/// # Errors
///
/// Returns why the key is unavailable: the session `block`, a disabled or
/// already on-air entry, or an armed playlist.
pub fn row_take(
    playlist: &PlaylistStatus,
    id: u64,
    block: Option<&'static str>,
) -> Result<PlaylistIntent, &'static str> {
    if let Some(reason) = block {
        return Err(reason);
    }
    let entry = playlist
        .entries
        .iter()
        .find(|entry| entry.id == id)
        .ok_or(Msg::ReasonStateDisallows.en())?;
    if !entry.enabled {
        return Err(Msg::PlaylistReasonEntryDisabled.en());
    }
    match PlaylistPhase::of(playlist.state) {
        PlaylistPhase::Playing | PlaylistPhase::Paused if on_air_entry(playlist) == Some(id) => {
            Err(Msg::PlaylistReasonOnAir.en())
        }
        PlaylistPhase::Playing | PlaylistPhase::Paused => Ok(PlaylistIntent::Jump { entry: id }),
        PlaylistPhase::Stopped | PlaylistPhase::Finished => {
            Ok(PlaylistIntent::Start { from: Some(id) })
        }
        PlaylistPhase::Armed => Err(Msg::PlaylistReasonActive.en()),
    }
}

/// A list edit on the replicated playlist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListEdit {
    MoveUp(u64),
    MoveDown(u64),
    Remove(u64),
    ToggleEnabled(u64),
    Loop(PlaylistLoopMode),
}

/// The `playlist_set` an edit sends, or why it cannot.
///
/// # Errors
///
/// Returns `block` when set, and a reason for a move past either end, an
/// unknown entry, or an unchanged loop mode.
pub fn list_edit(
    playlist: &PlaylistStatus,
    edit: ListEdit,
    block: Option<&'static str>,
) -> Result<PlaylistIntent, &'static str> {
    if let Some(reason) = block {
        return Err(reason);
    }
    let mut entries = playlist.entries.clone();
    let mut loop_mode = playlist.loop_mode;
    let position = |id: u64| index_of(playlist, id).ok_or(Msg::ReasonStateDisallows.en());
    match edit {
        ListEdit::MoveUp(id) => {
            let index = position(id)?;
            if index == 0 {
                return Err(Msg::PlaylistReasonFirst.en());
            }
            entries.swap(index - 1, index);
        }
        ListEdit::MoveDown(id) => {
            let index = position(id)?;
            if index + 1 == entries.len() {
                return Err(Msg::PlaylistReasonLast.en());
            }
            entries.swap(index, index + 1);
        }
        ListEdit::Remove(id) => {
            entries.remove(position(id)?);
        }
        ListEdit::ToggleEnabled(id) => {
            let index = position(id)?;
            entries[index].enabled = !entries[index].enabled;
        }
        ListEdit::Loop(mode) => {
            if mode == loop_mode {
                return Err(Msg::ReasonStateDisallows.en());
            }
            loop_mode = mode;
        }
    }
    Ok(PlaylistIntent::Set { loop_mode, entries })
}

/// The entry form under the list: a new entry, or one loaded for update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryForm {
    /// The entry being edited; `None` adds a new one.
    pub editing: Option<u64>,
    pub input: Option<InputId>,
    /// Duration text as typed (see [`parse_duration`]).
    pub duration: String,
    pub transition: PlaylistTransitionKind,
    /// Transition frames for every kind but Cut.
    pub transition_frames: u32,
    pub enabled: bool,
}

impl EntryForm {
    /// A ten-second cut with half-second transition frames ready for Fade.
    #[must_use]
    pub fn new(rate: FrameRate) -> Self {
        let fps = nominal_fps(rate);
        Self {
            editing: None,
            input: None,
            duration: timecode(10 * fps, rate),
            transition: PlaylistTransitionKind::Cut,
            transition_frames: u32::try_from(fps.div_ceil(2)).unwrap_or(15),
            enabled: true,
        }
    }

    /// The form loaded with one entry's values for update.
    #[must_use]
    pub fn load(entry: &PlaylistEntrySpec, rate: FrameRate) -> Self {
        let defaults = Self::new(rate);
        Self {
            editing: Some(entry.id),
            input: Some(entry.input.to_domain()),
            duration: timecode(entry.duration_frames, rate),
            transition: entry.transition,
            transition_frames: if entry.transition == PlaylistTransitionKind::Cut {
                defaults.transition_frames
            } else {
                entry.transition_frames
            },
            enabled: entry.enabled,
        }
    }

    /// The entry these values describe under `id`.
    ///
    /// # Errors
    ///
    /// Returns why the form is incomplete: no input, or an invalid duration.
    pub fn spec(&self, id: u64, rate: FrameRate) -> Result<PlaylistEntrySpec, &'static str> {
        let input = self.input.ok_or(Msg::PlaylistReasonNoInput.en())?;
        let duration_frames =
            parse_duration(&self.duration, rate).ok_or(Msg::PlaylistReasonInvalid.en())?;
        Ok(PlaylistEntrySpec {
            id,
            input: WireInputId::from_domain(input),
            duration_frames,
            transition: self.transition,
            transition_frames: transition_frames_for(
                self.transition,
                self.transition_frames,
                duration_frames,
            ),
            enabled: self.enabled,
        })
    }

    /// The `playlist_set` that appends this entry.
    ///
    /// # Errors
    ///
    /// Returns `block`, a full playlist, or an incomplete form.
    pub fn add_intent(
        &self,
        playlist: &PlaylistStatus,
        rate: FrameRate,
        block: Option<&'static str>,
    ) -> Result<PlaylistIntent, &'static str> {
        if let Some(reason) = block {
            return Err(reason);
        }
        if playlist.entries.len() >= MAX_PLAYLIST_ENTRIES {
            return Err(Msg::PlaylistReasonFull.en());
        }
        let mut entries = playlist.entries.clone();
        entries.push(self.spec(new_entry_id(&entries), rate)?);
        Ok(PlaylistIntent::Set {
            loop_mode: playlist.loop_mode,
            entries,
        })
    }

    /// The `playlist_set` that replaces the edited entry in place.
    ///
    /// # Errors
    ///
    /// Returns `block`, no edited entry, an incomplete form, or unchanged
    /// values.
    pub fn update_intent(
        &self,
        playlist: &PlaylistStatus,
        rate: FrameRate,
        block: Option<&'static str>,
    ) -> Result<PlaylistIntent, &'static str> {
        if let Some(reason) = block {
            return Err(reason);
        }
        let index = self
            .editing
            .and_then(|id| index_of(playlist, id))
            .ok_or(Msg::ReasonStateDisallows.en())?;
        let entry = self.spec(playlist.entries[index].id, rate)?;
        if entry == playlist.entries[index] {
            return Err(Msg::PlaylistReasonNoChange.en());
        }
        let mut entries = playlist.entries.clone();
        entries[index] = entry;
        Ok(PlaylistIntent::Set {
            loop_mode: playlist.loop_mode,
            entries,
        })
    }
}

/// When Studio first saw a playlist status, for the countdown.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Anchor {
    frame: u64,
    state: PlaylistRunState,
    at: Instant,
}

/// The Playlist tab's local state: the selected row, the entry form, and
/// the countdown anchor.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PlaylistPanel {
    selected: Option<u64>,
    form: Option<EntryForm>,
    anchor: Option<Anchor>,
    /// Move keyboard focus to the form's first field (the input picker) on
    /// the next draw, after a row was selected.
    focus_form: bool,
}

impl PlaylistPanel {
    /// Re-anchors the countdown whenever the replicated status changes and
    /// drops a selection whose entry is gone. Called every frame, whichever
    /// tab is shown, so the anchor is the moment the status arrived.
    pub(crate) fn sync(&mut self, state: &StudioUiState, now: Instant) {
        let Some(playlist) = state
            .project_state
            .as_ref()
            .and_then(fm_ui_model::ProjectState::playlist)
        else {
            self.anchor = None;
            return;
        };
        if self
            .anchor
            .as_ref()
            .is_none_or(|anchor| anchor.frame != playlist.frame || anchor.state != playlist.state)
        {
            self.anchor = Some(Anchor {
                frame: playlist.frame,
                state: playlist.state,
                at: now,
            });
        }
        if self
            .selected
            .is_some_and(|selected| index_of(playlist, selected).is_none())
        {
            self.selected = None;
            if let Some(form) = &mut self.form {
                form.editing = None;
            }
        }
    }

    /// Selects `entry`, loads it into the form, and moves keyboard focus to
    /// the form's first field so the operator can edit it from the keyboard.
    pub(crate) fn select(&mut self, entry: &PlaylistEntrySpec, rate: FrameRate) {
        self.selected = Some(entry.id);
        self.form = Some(EntryForm::load(entry, rate));
        self.focus_form = true;
    }

    /// The engine frame now, extrapolated from the anchor.
    fn now_frame(&self, rate: FrameRate, now: Instant) -> Option<u64> {
        self.anchor.as_ref().map(|anchor| {
            estimated_frame(anchor.frame, now.saturating_duration_since(anchor.at), rate)
        })
    }

    #[allow(clippy::too_many_lines)] // one immediate-mode tab
    pub(crate) fn draw(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let l = l10n::locale(ui.ctx());
        ui.add(egui::Label::new(caption(l.text(Msg::PlaylistIntro))).wrap());
        ui.add_space(2.0);
        let (Some(view), Some(project)) = (state.view.as_ref(), state.project_state.as_ref())
        else {
            empty_state(ui, l.text(Msg::TabPlaylist), l.text(Msg::PlaylistNotSynced));
            return;
        };
        let Some(playlist) = project.playlist() else {
            empty_state(ui, l.text(Msg::TabPlaylist), l.text(Msg::PlaylistWaiting));
            return;
        };
        let rate = project.frame_rate();
        let now = Instant::now();
        let now_frame = self.now_frame(rate, now).unwrap_or(playlist.frame);
        let phase = PlaylistPhase::of(playlist.state);
        if matches!(phase, PlaylistPhase::Playing | PlaylistPhase::Armed) {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        let control = control_block(state);
        let edit = edit_block(state, Some(playlist));
        let names = Names { l, view };

        draw_status_card(ui, playlist, phase, rate, now_frame, &names, edit, intents);
        ui.add_space(4.0);
        draw_transport(ui, &transport_availability(playlist, control), intents);
        ui.add_space(6.0);

        if playlist.entries.is_empty() {
            empty_state(
                ui,
                l.text(Msg::PlaylistNone),
                l.text(Msg::PlaylistNoneDetail),
            );
        } else {
            ui.horizontal(|ui| {
                ui.label(section_label(&l.format(
                    Msg::PlaylistEntryCount,
                    &[(
                        "count",
                        Arg::Count(u64::try_from(playlist.entries.len()).unwrap_or(u64::MAX)),
                    )],
                )));
            });
            for (index, entry) in playlist.entries.iter().enumerate() {
                let row = EntryRow {
                    position: index + 1,
                    entry,
                    name: names.input(entry.input.to_domain()),
                    role: entry_role(playlist, entry.id),
                    selected: self.selected == Some(entry.id),
                    countdown: (on_air_entry(playlist) == Some(entry.id))
                        .then(|| countdown(playlist, now_frame))
                        .flatten(),
                    rate,
                    take: row_take(playlist, entry.id, control),
                };
                match draw_entry_row(ui, &row) {
                    RowClick::Select => self.select(entry, rate),
                    RowClick::Take => {
                        if let Ok(intent) = row.take {
                            intents.push(StudioIntent::Playlist(intent));
                        }
                    }
                    RowClick::None => {}
                }
            }
            if let Some(selected) = self.selected {
                draw_row_actions(ui, playlist, selected, edit, intents);
            }
        }
        ui.add_space(6.0);
        let form = self.form.get_or_insert_with(|| EntryForm::new(rate));
        if form.editing.is_some() && self.selected.is_none() {
            form.editing = None;
        }
        let focus = std::mem::take(&mut self.focus_form);
        if draw_entry_form(ui, form, playlist, rate, &names, edit, focus, intents) {
            self.selected = None;
            *form = EntryForm {
                editing: None,
                ..form.clone()
            };
        }
    }
}

/// Input names in the console language.
struct Names<'a> {
    l: Locale,
    view: &'a ClientView,
}

impl Names<'_> {
    fn input(&self, input: InputId) -> String {
        input_name(self.l, &self.view.inputs, &self.view.input_names, input)
    }

    fn entry(&self, playlist: &PlaylistStatus, id: u64) -> Option<String> {
        index_of(playlist, id).map(|index| self.input(playlist.entries[index].input.to_domain()))
    }
}

fn key_button(label: &str, tone: Option<Color32>, width: f32) -> Button<'static> {
    let (fill, stroke) = tone.map_or((widget_color(), border_strong_color()), |color| {
        (color.gamma_multiply(0.18), color.gamma_multiply(0.7))
    });
    Button::new(
        RichText::new(label.to_owned())
            .size(crate::theme::legible(10.5))
            .strong()
            .color(text_color()),
    )
    .fill(fill)
    .stroke(Stroke::new(crate::theme::line_width(), stroke))
    .truncate()
    .min_size(Vec2::new(width, 24.0))
}

/// Phase chip, on-air countdown, next entry, and loop mode.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // one card
fn draw_status_card(
    ui: &mut Ui,
    playlist: &PlaylistStatus,
    phase: PlaylistPhase,
    rate: FrameRate,
    now_frame: u64,
    names: &Names<'_>,
    edit: Option<&'static str>,
    intents: &mut Vec<StudioIntent>,
) {
    let l = names.l;
    let on_air = on_air_entry(playlist).is_some();
    Frame::new()
        .fill(if phase == PlaylistPhase::Playing {
            PROGRAM_FILL.gamma_multiply(0.6)
        } else {
            raised_color()
        })
        .stroke(Stroke::new(
            crate::theme::line_width(),
            if on_air {
                PROGRAM.gamma_multiply(0.6)
            } else {
                border_color()
            },
        ))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                ui.label(section_label(&l.text(Msg::TabPlaylist).to_uppercase()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    status_chip(ui, phase.label(), phase.tone(), 96.0);
                });
            });
            match countdown(playlist, now_frame) {
                Some(Countdown::Remaining { frames, duration }) => {
                    let name = on_air_entry(playlist)
                        .and_then(|id| names.entry(playlist, id))
                        .unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.label(field_label(l.text(Msg::PlaylistRemaining)));
                        ui.label(
                            mono(countdown_timecode(frames, rate))
                                .size(20.0)
                                .strong()
                                .color(if phase == PlaylistPhase::Paused {
                                    AMBER
                                } else {
                                    PROGRAM
                                }),
                        );
                        ui.label(
                            mono(l.format(
                                Msg::PlaylistElapsedOf,
                                &[("duration", Arg::Str(&timecode(duration, rate)))],
                            ))
                            .size(crate::theme::legible(10.5))
                            .color(text_dim_color()),
                        );
                    });
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("{} · {name}", l.text(Msg::OverlayOnAir)))
                                .size(crate::theme::legible(11.5))
                                .strong()
                                .color(text_color()),
                        )
                        .truncate(),
                    );
                }
                Some(Countdown::StartsIn(frames)) => {
                    ui.horizontal(|ui| {
                        ui.label(field_label(l.text(Msg::PlaylistStartsIn)));
                        ui.label(
                            mono(countdown_timecode(frames, rate))
                                .size(20.0)
                                .strong()
                                .color(AMBER),
                        );
                    });
                }
                None => {}
            }
            if phase.running() {
                let next = next_entry(playlist);
                let text = match next {
                    Some(id) if Some(id) == on_air_entry(playlist) => l.format(
                        Msg::PlaylistNextRepeat,
                        &[(
                            "name",
                            Arg::Str(&names.entry(playlist, id).unwrap_or_default()),
                        )],
                    ),
                    Some(id) => l.format(
                        Msg::PlaylistNextName,
                        &[(
                            "name",
                            Arg::Str(&names.entry(playlist, id).unwrap_or_default()),
                        )],
                    ),
                    None => l.text(Msg::PlaylistNextEnd).to_owned(),
                };
                ui.add(
                    egui::Label::new(RichText::new(text).size(crate::theme::legible(11.0)).color(
                        if next.is_some() {
                            PREVIEW
                        } else {
                            text_dim_color()
                        },
                    ))
                    .truncate(),
                );
            }
            ui.horizontal(|ui| {
                ui.label(field_label(l.text(Msg::PlaylistLoop)));
                for (mode, label, hover) in [
                    (
                        PlaylistLoopMode::Off,
                        Msg::PlaylistLoopOff,
                        Msg::PlaylistLoopOffHover,
                    ),
                    (
                        PlaylistLoopMode::All,
                        Msg::PlaylistLoopAll,
                        Msg::PlaylistLoopAllHover,
                    ),
                    (
                        PlaylistLoopMode::One,
                        Msg::PlaylistLoopOne,
                        Msg::PlaylistLoopOneHover,
                    ),
                ] {
                    let selected = playlist.loop_mode == mode;
                    let intent = list_edit(playlist, ListEdit::Loop(mode), edit);
                    let button = Button::new(
                        RichText::new(l.text(label).to_uppercase())
                            .size(crate::theme::legible(10.0))
                            .strong()
                            .color(if selected {
                                text_color()
                            } else {
                                text_dim_color()
                            }),
                    )
                    .fill(if selected {
                        ACCENT_FILL
                    } else {
                        widget_color()
                    })
                    .stroke(Stroke::new(
                        crate::theme::line_width(),
                        if selected {
                            accent_color().gamma_multiply(0.7)
                        } else {
                            border_strong_color()
                        },
                    ))
                    .min_size(Vec2::new(48.0, 20.0));
                    let response = ui.add_enabled(selected || intent.is_ok(), button).hint(
                        l.text(hover),
                        edit.unwrap_or(Msg::ReasonStateDisallows.en()),
                    );
                    if response.clicked()
                        && let Ok(intent) = intent
                    {
                        intents.push(StudioIntent::Playlist(intent));
                    }
                }
            });
        });
}

/// One transport key: its intent or reason, label, hover, and tone.
type TransportKey<'a> = (
    &'a Result<PlaylistIntent, &'static str>,
    Msg,
    Msg,
    Option<Color32>,
);

/// Start, Stop, Pause/Resume, Previous, and Next keys.
fn draw_transport(ui: &mut Ui, transport: &TransportAvailability, intents: &mut Vec<StudioIntent>) {
    let l = l10n::locale(ui.ctx());
    let resume = transport.pause_resume == Ok(PlaylistIntent::Resume);
    let keys: [TransportKey<'_>; 5] = [
        (
            &transport.start,
            Msg::PlaylistStart,
            Msg::PlaylistStartHover,
            Some(PREVIEW),
        ),
        (
            &transport.stop,
            Msg::PlaylistStop,
            Msg::PlaylistStopHover,
            Some(PROGRAM),
        ),
        if resume {
            (
                &transport.pause_resume,
                Msg::PlaylistResume,
                Msg::PlaylistResumeHover,
                Some(AMBER),
            )
        } else {
            (
                &transport.pause_resume,
                Msg::PlaylistPause,
                Msg::PlaylistPauseHover,
                None,
            )
        },
        (
            &transport.previous,
            Msg::PlaylistPrevious,
            Msg::PlaylistPreviousHover,
            None,
        ),
        (
            &transport.next,
            Msg::PlaylistNext,
            Msg::PlaylistNextHover,
            None,
        ),
    ];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.spacing_mut().button_padding.x = 3.0;
        #[allow(clippy::cast_precision_loss)] // five keys
        let width = ((ui.available_width() - 4.0 * (keys.len() - 1) as f32) / keys.len() as f32)
            .floor()
            .max(48.0);
        for (result, label, hover, tone) in keys {
            let enabled = result.is_ok();
            // A fixed width keeps the five keys inside the panel; longer
            // words truncate with the full label in the tooltip.
            let response = ui
                .add_enabled_ui(enabled, |ui| {
                    ui.add_sized(
                        Vec2::new(width, 24.0),
                        key_button(
                            &l.text(label).to_uppercase(),
                            enabled.then_some(tone).flatten(),
                            width,
                        ),
                    )
                })
                .inner
                .hint(l.text(hover), result.as_ref().err().copied().unwrap_or(""));
            if response.clicked()
                && let Ok(intent) = result
            {
                intents.push(StudioIntent::Playlist(intent.clone()));
            }
        }
    });
}

struct EntryRow<'a> {
    position: usize,
    entry: &'a PlaylistEntrySpec,
    name: String,
    role: EntryRole,
    selected: bool,
    countdown: Option<Countdown>,
    rate: FrameRate,
    take: Result<PlaylistIntent, &'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowClick {
    None,
    Select,
    Take,
}

const ROW_HEIGHT: f32 = 36.0;
/// Id salt of the entry form's duration field.
const DURATION_FIELD_ID: &str = "studio-playlist-duration";
const TAKE_WIDTH: f32 = 26.0;

/// One entry row: position, play key, name, transition, the role badge, and
/// the duration (the countdown on the on-air row).
#[allow(clippy::too_many_lines)] // one painted row
fn draw_entry_row(ui: &mut Ui, row: &EntryRow<'_>) -> RowClick {
    let l = l10n::locale(ui.ctx());
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, ROW_HEIGHT), Sense::click());
    let take_rect = egui::Rect::from_min_size(
        pos2(rect.min.x + 6.0, rect.center().y - 11.0),
        Vec2::new(TAKE_WIDTH, 22.0),
    );
    let take_response = ui
        .interact(
            take_rect,
            ui.id().with(("playlist-take", row.entry.id)),
            if row.take.is_ok() {
                Sense::click()
            } else {
                Sense::hover()
            },
        )
        .hint(
            &l.format(
                if matches!(row.take, Ok(PlaylistIntent::Jump { .. })) {
                    Msg::PlaylistJumpHover
                } else {
                    Msg::PlaylistStartFromHover
                },
                &[("name", Arg::Str(&row.name))],
            ),
            row.take.as_ref().err().copied().unwrap_or(""),
        );
    let transition = entry_transition_text(l, row.entry);
    let duration = timecode(row.entry.duration_frames, row.rate);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let (accent, fill) = match row.role {
            EntryRole::OnAir => (PROGRAM, PROGRAM_FILL),
            EntryRole::Next => (PREVIEW, PREVIEW.gamma_multiply(0.10)),
            EntryRole::Idle => (border_color(), graphite_color()),
        };
        let fill = if response.hovered() && row.role == EntryRole::Idle {
            widget_hover_color()
        } else {
            fill
        };
        painter.rect_filled(rect, CornerRadius::same(4), fill);
        let stroke = if row.selected {
            Stroke::new(1.5, accent_color())
        } else if row.role == EntryRole::Idle {
            Stroke::new(crate::theme::line_width(), border_color())
        } else {
            Stroke::new(1.0, accent.gamma_multiply(0.8))
        };
        painter.rect_stroke(rect, CornerRadius::same(4), stroke, StrokeKind::Inside);
        if row.role != EntryRole::Idle {
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
                CornerRadius::same(2),
                accent,
            );
        }
        // Play key.
        let take_fill = if take_response.hovered() && row.take.is_ok() {
            PREVIEW.gamma_multiply(0.35)
        } else if row.take.is_ok() {
            widget_color()
        } else {
            raised_color()
        };
        painter.rect_filled(take_rect, CornerRadius::same(3), take_fill);
        let triangle_color = if row.take.is_ok() {
            text_color()
        } else {
            muted_color()
        };
        let center = take_rect.center();
        painter.add(egui::Shape::convex_polygon(
            vec![
                pos2(center.x - 4.0, center.y - 6.0),
                pos2(center.x + 6.0, center.y),
                pos2(center.x - 4.0, center.y + 6.0),
            ],
            triangle_color,
            Stroke::NONE,
        ));
        // Position and name.
        let text_left = take_rect.max.x + 8.0;
        painter.text(
            pos2(text_left, rect.min.y + 11.0),
            egui::Align2::LEFT_CENTER,
            format!("{:02}", row.position),
            FontId::new(10.5, FontFamily::Monospace),
            text_dim_color(),
        );
        let badge_width = 62.0;
        let duration_width = 100.0;
        let name_left = text_left + 22.0;
        let name_color = if row.entry.enabled {
            text_color()
        } else {
            muted_color()
        };
        let name = elided(
            painter,
            &row.name,
            FontId::new(12.0, FontFamily::Proportional),
            name_color,
            (rect.max.x - duration_width - 8.0 - name_left).max(20.0),
        );
        painter.galley(
            pos2(name_left, rect.min.y + 11.0 - name.size().y / 2.0),
            name,
            name_color,
        );
        let detail = elided(
            painter,
            &transition,
            FontId::new(10.0, FontFamily::Proportional),
            text_dim_color(),
            (rect.max.x - duration_width - badge_width - 12.0 - text_left).max(20.0),
        );
        painter.galley(
            pos2(text_left, rect.max.y - 10.0 - detail.size().y / 2.0),
            detail,
            text_dim_color(),
        );
        // Role badge left of the duration column.
        let badge = match row.role {
            EntryRole::OnAir => Some((Msg::OverlayOnAir.en(), Tone::OnAir)),
            EntryRole::Next => Some((Msg::NextKey.en(), Tone::Ready)),
            EntryRole::Idle if !row.entry.enabled => {
                Some((Msg::PlaylistDisabledBadge.en(), Tone::Idle))
            }
            EntryRole::Idle => None,
        };
        if let Some((label, tone)) = badge {
            let badge_rect = egui::Rect::from_min_size(
                pos2(
                    rect.max.x - duration_width - badge_width - 4.0,
                    rect.max.y - 18.0,
                ),
                Vec2::new(badge_width, 14.0),
            );
            crate::theme::status_chip_painted(painter, badge_rect, label, tone);
        }
        // Duration, or the on-air countdown.
        let (text, color) = match row.countdown {
            Some(Countdown::Remaining { frames, .. }) => (
                countdown_timecode(frames, row.rate),
                if matches!(row.role, EntryRole::OnAir) {
                    PROGRAM
                } else {
                    text_color()
                },
            ),
            _ => (
                duration,
                if row.entry.enabled {
                    text_color()
                } else {
                    muted_color()
                },
            ),
        };
        painter.text(
            pos2(rect.max.x - 8.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            text,
            FontId::new(12.0, FontFamily::Monospace),
            color,
        );
    }
    if take_response.clicked() && row.take.is_ok() {
        return RowClick::Take;
    }
    let response = response.on_hover_text(l.format(
        Msg::PlaylistRowHover,
        &[
            (
                "position",
                Arg::Plain(u64::try_from(row.position).unwrap_or(0)),
            ),
            ("name", Arg::Str(&row.name)),
            ("transition", Arg::Str(&transition)),
            (
                "duration",
                Arg::Str(&timecode(row.entry.duration_frames, row.rate)),
            ),
        ],
    ));
    ui.add_space(2.0);
    if response.clicked() {
        RowClick::Select
    } else {
        RowClick::None
    }
}

/// Earlier, Later, Enable/Skip, and Remove keys for the selected row.
fn draw_row_actions(
    ui: &mut Ui,
    playlist: &PlaylistStatus,
    selected: u64,
    edit: Option<&'static str>,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let Some(index) = index_of(playlist, selected) else {
        return;
    };
    let enabled = playlist.entries[index].enabled;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let actions = [
            (
                "▲",
                ListEdit::MoveUp(selected),
                Msg::PlaylistMoveUpHover,
                None,
            ),
            (
                "▼",
                ListEdit::MoveDown(selected),
                Msg::PlaylistMoveDownHover,
                None,
            ),
            (
                if enabled { "⏸" } else { "✔" },
                ListEdit::ToggleEnabled(selected),
                if enabled {
                    Msg::PlaylistDisableHover
                } else {
                    Msg::PlaylistEnableHover
                },
                None,
            ),
            (
                "✕",
                ListEdit::Remove(selected),
                Msg::PlaylistRemoveHover,
                Some(ERROR),
            ),
        ];
        for (glyph, edit_kind, hover, tone) in actions {
            let intent = list_edit(playlist, edit_kind, edit);
            let response = ui
                .add_enabled(intent.is_ok(), key_button(glyph, tone, 40.0))
                .hint(l.text(hover), intent.as_ref().err().copied().unwrap_or(""));
            if response.clicked()
                && let Ok(intent) = intent
            {
                intents.push(StudioIntent::Playlist(intent));
            }
        }
    });
}

/// The entry form: input, duration, transition, and frames, with Add and
/// Update keys. `focus` moves keyboard focus to the input picker; Enter in
/// the duration field submits the form (Update while an entry is loaded,
/// else Add). Returns whether the operator asked for a fresh form.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)] // one form card
fn draw_entry_form(
    ui: &mut Ui,
    form: &mut EntryForm,
    playlist: &PlaylistStatus,
    rate: FrameRate,
    names: &Names<'_>,
    edit: Option<&'static str>,
    focus: bool,
    intents: &mut Vec<StudioIntent>,
) -> bool {
    let l = names.l;
    let mut fresh = false;
    let mut submit = false;
    crate::theme::card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 5.0;
        let position = form
            .editing
            .and_then(|id| index_of(playlist, id))
            .map(|index| index + 1);
        ui.horizontal(|ui| {
            let heading = position.map_or_else(
                || l.text(Msg::PlaylistEditorNew).to_owned(),
                |position| {
                    l.format(
                        Msg::PlaylistEditorEdit,
                        &[("position", Arg::Plain(u64::try_from(position).unwrap_or(0)))],
                    )
                },
            );
            ui.label(section_label(&heading.to_uppercase()));
            if position.is_some() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(crate::theme::small_button(l.text(Msg::PlaylistNewEntry)))
                        .on_hover_text(l.text(Msg::PlaylistNewEntryHover))
                        .clicked()
                    {
                        fresh = true;
                    }
                });
            }
        });
        egui::Grid::new("studio-playlist-form")
            .num_columns(2)
            .spacing(Vec2::new(8.0, 4.0))
            .show(ui, |ui| {
                ui.label(field_label(l.text(Msg::PlaylistInput)));
                let picker = ComboBox::from_id_salt("studio-playlist-input")
                    .width(ui.available_width().max(120.0))
                    .truncate()
                    .selected_text(
                        RichText::new(form.input.map_or_else(
                            || l.text(Msg::PlaylistInputPick).to_owned(),
                            |input| names.input(input),
                        ))
                        .size(crate::theme::legible(11.0)),
                    )
                    .show_ui(ui, |ui| {
                        for (input, name) in names.view.inputs.iter().zip(&names.view.input_names) {
                            if ui
                                .selectable_label(form.input == Some(*input), name)
                                .on_hover_text(l.format(
                                    Msg::PlaylistInputOptionHover,
                                    &[("name", Arg::Str(name))],
                                ))
                                .clicked()
                            {
                                form.input = Some(*input);
                            }
                        }
                    })
                    .response
                    .on_hover_text(l.text(Msg::PlaylistInputPickHover));
                if focus {
                    picker.request_focus();
                }
                ui.end_row();

                ui.label(field_label(l.text(Msg::Duration)));
                let valid = parse_duration(&form.duration, rate).is_some();
                let duration = ui
                    .add(
                        TextEdit::singleline(&mut form.duration)
                            .id(egui::Id::new(DURATION_FIELD_ID))
                            .font(FontId::new(12.0, FontFamily::Monospace))
                            .text_color(if valid { text_color() } else { ERROR })
                            .hint_text(l.text(Msg::PlaylistDurationHint))
                            .desired_width(110.0),
                    )
                    .on_hover_text(l.text(Msg::PlaylistDurationHover));
                // A single-line field gives up focus on Enter.
                submit = duration.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
                ui.end_row();

                ui.label(field_label(l.text(Msg::TransitionSection)));
                ui.horizontal(|ui| {
                    ComboBox::from_id_salt("studio-playlist-transition")
                        .width(96.0)
                        .selected_text(
                            RichText::new(l.tr(transition_label(form.transition)))
                                .size(crate::theme::legible(11.0)),
                        )
                        .show_ui(ui, |ui| {
                            for kind in TRANSITIONS {
                                ui.selectable_value(
                                    &mut form.transition,
                                    kind,
                                    l.tr(transition_label(kind)),
                                )
                                .on_hover_text(l.format(
                                    Msg::PlaylistTransitionOptionHover,
                                    &[("kind", Arg::Str(l.tr(transition_label(kind))))],
                                ));
                            }
                        })
                        .response
                        .on_hover_text(l.text(Msg::PlaylistTransitionPickHover));
                    ui.add_enabled(
                        form.transition != PlaylistTransitionKind::Cut,
                        egui::DragValue::new(&mut form.transition_frames)
                            .range(1..=MAX_PLAYLIST_TRANSITION_FRAMES)
                            .suffix(" fr"),
                    )
                    .hint(
                        l.text(Msg::PlaylistTransitionFramesHover),
                        Msg::PlaylistReasonCutHasNoLength.en(),
                    );
                });
                ui.end_row();
            });
        if parse_duration(&form.duration, rate).is_none() {
            ui.label(
                RichText::new(l.format(
                    Msg::PlaylistErrDuration,
                    &[("max", Arg::Str(&timecode(MAX_PLAYLIST_ENTRY_FRAMES, rate)))],
                ))
                .size(crate::theme::legible(10.5))
                .color(ERROR),
            );
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let width = ((ui.available_width() - 6.0) / 2.0).floor().max(80.0);
            let add = form.add_intent(playlist, rate, edit);
            let update = form.update_intent(playlist, rate, edit);
            if submit {
                // Enter updates the loaded entry, or adds a new one.
                let intent = if position.is_some() { &update } else { &add };
                if let Ok(intent) = intent {
                    intents.push(StudioIntent::Playlist(intent.clone()));
                }
            }
            if ui
                .add_enabled(
                    add.is_ok(),
                    key_button(
                        &l.text(Msg::PlaylistAdd).to_uppercase(),
                        Some(accent_color()),
                        width,
                    ),
                )
                .hint(
                    l.text(Msg::PlaylistAddHover),
                    add.as_ref().err().copied().unwrap_or(""),
                )
                .clicked()
                && let Ok(intent) = add
            {
                intents.push(StudioIntent::Playlist(intent));
            }
            let update_hover = l.format(
                Msg::PlaylistUpdateHover,
                &[(
                    "position",
                    Arg::Plain(position.map_or(0, |position| u64::try_from(position).unwrap_or(0))),
                )],
            );
            if position.is_some()
                && ui
                    .add_enabled(
                        update.is_ok(),
                        key_button(&l.text(Msg::PlaylistUpdate).to_uppercase(), None, width),
                    )
                    .hint(&update_hover, update.as_ref().err().copied().unwrap_or(""))
                    .clicked()
                && let Ok(intent) = update
            {
                intents.push(StudioIntent::Playlist(intent));
            }
        });
        if let Some(reason) = edit {
            ui.add(
                egui::Label::new(
                    RichText::new(l10n::unavailable(l, reason))
                        .size(crate::theme::legible(10.5))
                        .color(muted_color()),
                )
                .wrap(),
            );
        }
    });
    fresh
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_protocol::{
        EngineIdentity, FadeToBlackPosition, FadeToBlackState, InputStatus, OverlayBorderPreset,
        OverlayPositionPreset, OverlayTransitionKind, PlaylistStateMessage, RecordingStatus,
        SnapshotMessage, WireRecorderSlot,
    };
    use fm_types::ProjectId;
    use fm_ui_model::i18n::Language;
    use fm_ui_model::{ClientModel, ProjectSnapshot};

    use super::*;
    use crate::StudioShell;
    use crate::panels::SideTab;

    const ES: Locale = Locale::new(Language::Spanish);

    fn rate() -> FrameRate {
        FrameRate::new(25, 1).unwrap()
    }

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn entry(id: u64, input_value: u128, duration: u64, enabled: bool) -> PlaylistEntrySpec {
        PlaylistEntrySpec {
            id,
            input: WireInputId::from_domain(input(input_value)),
            duration_frames: duration,
            transition: if id.is_multiple_of(2) {
                PlaylistTransitionKind::Fade
            } else {
                PlaylistTransitionKind::Cut
            },
            transition_frames: if id.is_multiple_of(2) { 12 } else { 0 },
            enabled,
        }
    }

    /// Entries 1 (Camera, 10 s), 2 (Slides, 5 s, Fade 12), 3 (skipped), 4.
    fn list(loop_mode: PlaylistLoopMode, state: PlaylistRunState) -> PlaylistStatus {
        PlaylistStatus {
            frame: 100,
            loop_mode,
            entries: vec![
                entry(1, 1, 250, true),
                entry(2, 2, 125, true),
                entry(3, 1, 50, false),
                entry(4, 2, 75, true),
            ],
            state,
        }
    }

    const fn playing(entry: u64, started_frame: u64) -> PlaylistRunState {
        PlaylistRunState::Playing {
            entry,
            take: 1,
            cycle: 0,
            started_frame,
        }
    }

    #[test]
    fn timecode_and_durations_use_the_nominal_rate() {
        assert_eq!(timecode(0, rate()), "00:00:00:00");
        assert_eq!(timecode(25 * 3_661 + 7, rate()), "01:01:01:07");
        let ntsc = FrameRate::new(30_000, 1_001).unwrap();
        assert_eq!(nominal_fps(ntsc), 30);
        assert_eq!(timecode(30 * 60 + 29, ntsc), "00:01:00:29");
        assert_eq!(
            timecode(125, FrameRate::new(120, 1).unwrap()),
            "00:00:01:005"
        );

        assert_eq!(parse_duration("10", rate()), Some(250));
        assert_eq!(parse_duration(" 1:30 ", rate()), Some(90 * 25));
        assert_eq!(parse_duration("01:00:00", rate()), Some(3_600 * 25));
        assert_eq!(parse_duration("00:00:01:12", rate()), Some(37));
        assert_eq!(parse_duration("90", rate()), Some(90 * 25));
        for invalid in [
            "",
            "0",
            "1:60",
            "1:60:00",
            "00:00:00:25",
            "a",
            "1::2",
            "-1",
            "1:2:3:4:5",
        ] {
            assert_eq!(parse_duration(invalid, rate()), None, "{invalid:?}");
        }
        assert_eq!(
            parse_duration(&timecode(MAX_PLAYLIST_ENTRY_FRAMES, rate()), rate()),
            Some(MAX_PLAYLIST_ENTRY_FRAMES)
        );
        assert_eq!(
            parse_duration(&timecode(MAX_PLAYLIST_ENTRY_FRAMES + 1, rate()), rate()),
            None
        );

        assert_eq!(
            transition_frames_for(PlaylistTransitionKind::Cut, 30, 250),
            0
        );
        assert_eq!(
            transition_frames_for(PlaylistTransitionKind::Fade, 30, 10),
            10
        );
        assert_eq!(
            transition_frames_for(PlaylistTransitionKind::Wipe, 0, 250),
            1
        );
        assert_eq!(
            transition_frames_for(PlaylistTransitionKind::Zoom, u32::MAX, u64::MAX),
            MAX_PLAYLIST_TRANSITION_FRAMES
        );
    }

    #[test]
    fn countdown_runs_while_playing_holds_while_paused_and_counts_to_a_start() {
        let playlist = list(PlaylistLoopMode::Off, playing(2, 100));
        assert_eq!(
            countdown(&playlist, 100),
            Some(Countdown::Remaining {
                frames: 125,
                duration: 125
            })
        );
        assert_eq!(
            countdown(&playlist, 200),
            Some(Countdown::Remaining {
                frames: 25,
                duration: 125
            })
        );
        // Past the boundary it rests at zero; before the start it is full.
        assert_eq!(
            countdown(&playlist, 400),
            Some(Countdown::Remaining {
                frames: 0,
                duration: 125
            })
        );
        assert_eq!(
            countdown(&playlist, 50),
            Some(Countdown::Remaining {
                frames: 125,
                duration: 125
            })
        );
        let paused = list(
            PlaylistLoopMode::Off,
            PlaylistRunState::Paused {
                entry: 1,
                take: 1,
                cycle: 0,
                elapsed_frames: 50,
            },
        );
        assert_eq!(
            countdown(&paused, 10_000),
            Some(Countdown::Remaining {
                frames: 200,
                duration: 250
            })
        );
        let armed = list(
            PlaylistLoopMode::Off,
            PlaylistRunState::Armed {
                start_frame: 130,
                from: None,
            },
        );
        assert_eq!(countdown(&armed, 100), Some(Countdown::StartsIn(30)));
        assert_eq!(
            countdown(&list(PlaylistLoopMode::Off, PlaylistRunState::Stopped), 0),
            None
        );

        let start = Duration::from_millis(1_000);
        assert_eq!(estimated_frame(100, start, rate()), 125);
        assert_eq!(
            estimated_frame(0, start, FrameRate::new(30_000, 1_001).unwrap()),
            29
        );
    }

    #[test]
    fn roles_mark_the_on_air_entry_and_the_next_by_loop_mode() {
        let off = list(PlaylistLoopMode::Off, playing(2, 0));
        assert_eq!(entry_role(&off, 2), EntryRole::OnAir);
        // The disabled entry 3 is skipped.
        assert_eq!(next_entry(&off), Some(4));
        assert_eq!(entry_role(&off, 4), EntryRole::Next);
        assert_eq!(entry_role(&off, 1), EntryRole::Idle);
        let last = list(PlaylistLoopMode::Off, playing(4, 0));
        assert_eq!(next_entry(&last), None);
        assert_eq!(
            next_entry(&list(PlaylistLoopMode::All, playing(4, 0))),
            Some(1)
        );
        assert_eq!(
            next_entry(&list(PlaylistLoopMode::One, playing(4, 0))),
            Some(4)
        );
        let armed = |from| {
            list(
                PlaylistLoopMode::Off,
                PlaylistRunState::Armed {
                    start_frame: 0,
                    from,
                },
            )
        };
        assert_eq!(next_entry(&armed(None)), Some(1));
        assert_eq!(next_entry(&armed(Some(4))), Some(4));
        assert_eq!(
            next_entry(&list(
                PlaylistLoopMode::All,
                PlaylistRunState::Finished { last: Some(4) }
            )),
            None
        );
    }

    #[test]
    fn transport_follows_the_engine_rules_and_the_session_gate() {
        let stopped = list(PlaylistLoopMode::Off, PlaylistRunState::Stopped);
        let keys = transport_availability(&stopped, None);
        assert_eq!(keys.start, Ok(PlaylistIntent::Start { from: None }));
        assert_eq!(keys.stop, Err(Msg::PlaylistReasonNotOnAir.en()));
        assert_eq!(keys.pause_resume, Err(Msg::PlaylistReasonNotOnAir.en()));
        assert_eq!(keys.next, Err(Msg::PlaylistReasonNotOnAir.en()));

        let first = list(PlaylistLoopMode::Off, playing(1, 0));
        let keys = transport_availability(&first, None);
        assert_eq!(keys.start, Err(Msg::PlaylistReasonActive.en()));
        assert_eq!(keys.stop, Ok(PlaylistIntent::Stop));
        assert_eq!(keys.pause_resume, Ok(PlaylistIntent::Pause));
        assert_eq!(keys.previous, Err(Msg::PlaylistReasonNoPrevious.en()));
        assert_eq!(keys.next, Ok(PlaylistIntent::Next));
        // Loop All wraps Previous; loop One does not wrap Next.
        let keys = transport_availability(&list(PlaylistLoopMode::All, playing(1, 0)), None);
        assert_eq!(keys.previous, Ok(PlaylistIntent::Previous));
        let keys = transport_availability(&list(PlaylistLoopMode::One, playing(4, 0)), None);
        assert_eq!(keys.next, Err(Msg::PlaylistReasonNoNext.en()));

        let paused = list(
            PlaylistLoopMode::Off,
            PlaylistRunState::Paused {
                entry: 2,
                take: 1,
                cycle: 0,
                elapsed_frames: 1,
            },
        );
        assert_eq!(
            transport_availability(&paused, None).pause_resume,
            Ok(PlaylistIntent::Resume)
        );

        let mut none_enabled = stopped.clone();
        for entry in &mut none_enabled.entries {
            entry.enabled = false;
        }
        assert_eq!(
            transport_availability(&none_enabled, None).start,
            Err(Msg::PlaylistReasonNoEnabled.en())
        );

        // The session gate overrides every key.
        let gate = Msg::ReasonNoTransition.en();
        let keys = transport_availability(&first, Some(gate));
        for key in [
            keys.start,
            keys.stop,
            keys.pause_resume,
            keys.previous,
            keys.next,
        ] {
            assert_eq!(key, Err(gate));
        }

        // Row keys: Jump while on air, Start from a stopped list.
        assert_eq!(
            row_take(&first, 4, None),
            Ok(PlaylistIntent::Jump { entry: 4 })
        );
        assert_eq!(
            row_take(&first, 1, None),
            Err(Msg::PlaylistReasonOnAir.en())
        );
        assert_eq!(
            row_take(&first, 3, None),
            Err(Msg::PlaylistReasonEntryDisabled.en())
        );
        assert_eq!(
            row_take(&stopped, 2, None),
            Ok(PlaylistIntent::Start { from: Some(2) })
        );
        assert_eq!(row_take(&stopped, 2, Some(gate)), Err(gate));
    }

    #[test]
    fn list_edits_send_the_whole_list_and_respect_the_ends() {
        let stopped = list(PlaylistLoopMode::Off, PlaylistRunState::Stopped);
        let ids = |intent: Result<PlaylistIntent, &'static str>| match intent.unwrap() {
            PlaylistIntent::Set { entries, .. } => {
                entries.iter().map(|entry| entry.id).collect::<Vec<_>>()
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(
            ids(list_edit(&stopped, ListEdit::MoveUp(2), None)),
            [2, 1, 3, 4]
        );
        assert_eq!(
            ids(list_edit(&stopped, ListEdit::MoveDown(2), None)),
            [1, 3, 2, 4]
        );
        assert_eq!(
            ids(list_edit(&stopped, ListEdit::Remove(3), None)),
            [1, 2, 4]
        );
        assert_eq!(
            list_edit(&stopped, ListEdit::MoveUp(1), None),
            Err(Msg::PlaylistReasonFirst.en())
        );
        assert_eq!(
            list_edit(&stopped, ListEdit::MoveDown(4), None),
            Err(Msg::PlaylistReasonLast.en())
        );
        let Ok(PlaylistIntent::Set { entries, loop_mode }) =
            list_edit(&stopped, ListEdit::ToggleEnabled(3), None)
        else {
            panic!("toggle")
        };
        assert!(entries[2].enabled);
        assert_eq!(loop_mode, PlaylistLoopMode::Off);
        assert!(matches!(
            list_edit(&stopped, ListEdit::Loop(PlaylistLoopMode::All), None),
            Ok(PlaylistIntent::Set {
                loop_mode: PlaylistLoopMode::All,
                ..
            })
        ));
        assert!(list_edit(&stopped, ListEdit::Loop(PlaylistLoopMode::Off), None).is_err());
        let gate = Msg::PlaylistReasonRunning.en();
        assert_eq!(
            list_edit(&stopped, ListEdit::Remove(1), Some(gate)),
            Err(gate)
        );

        // The form appends with a fresh id and updates in place.
        let mut form = EntryForm::new(rate());
        assert_eq!(form.duration, "00:00:10:00");
        assert_eq!(
            form.add_intent(&stopped, rate(), None),
            Err(Msg::PlaylistReasonNoInput.en())
        );
        form.input = Some(input(2));
        form.transition = PlaylistTransitionKind::Wipe;
        let Ok(PlaylistIntent::Set { entries, .. }) = form.add_intent(&stopped, rate(), None)
        else {
            panic!("add")
        };
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[4].id, 5);
        assert_eq!(entries[4].duration_frames, 250);
        assert_eq!(entries[4].transition_frames, 13);
        form.duration = "bad".into();
        assert_eq!(
            form.add_intent(&stopped, rate(), None),
            Err(Msg::PlaylistReasonInvalid.en())
        );

        let mut form = EntryForm::load(&stopped.entries[1], rate());
        assert_eq!(form.duration, "00:00:05:00");
        assert_eq!(
            form.update_intent(&stopped, rate(), None),
            Err(Msg::PlaylistReasonNoChange.en())
        );
        form.duration = "8".into();
        let Ok(PlaylistIntent::Set { entries, .. }) = form.update_intent(&stopped, rate(), None)
        else {
            panic!("update")
        };
        assert_eq!(entries[1].id, 2);
        assert_eq!(entries[1].duration_frames, 200);
        assert_eq!(entries.len(), 4);

        let full = PlaylistStatus {
            entries: (1..=u64::try_from(MAX_PLAYLIST_ENTRIES).unwrap())
                .map(|id| entry(id, 1, 25, true))
                .collect(),
            ..stopped
        };
        form.editing = None;
        form.duration = "1".into();
        assert_eq!(
            form.add_intent(&full, rate(), None),
            Err(Msg::PlaylistReasonFull.en())
        );
        assert_eq!(new_entry_id(&[]), 1);
        assert_eq!(
            new_entry_id(&[entry(u64::MAX, 1, 1, true), entry(1, 1, 1, true)]),
            2
        );
    }

    /// A synchronized model with inputs Camera and Slides and `playlist`.
    fn model(playlist: Option<PlaylistStatus>) -> ClientModel {
        let project = ProjectId::new(NonZeroU128::new(1).unwrap());
        let fade = FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        };
        let wire = WireInputId::from_domain;
        let snapshot = SnapshotMessage {
            engine: EngineIdentity {
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 10,
            show_name: "Show".into(),
            inputs: [(1, "Camera"), (2, "Slides")]
                .map(|(value, name)| InputStatus {
                    input: wire(input(value)),
                    name: name.into(),
                })
                .to_vec(),
            outputs: Vec::new(),
            input_audio_strips: [1, 2]
                .map(|value| fm_protocol::InputAudioStripStatus {
                    input: wire(input(value)),
                    gain_millidb: 0,
                    balance_basis_points: 0,
                    muted: false,
                    soloed: false,
                    follow_video: true,
                    delay_samples: 0,
                })
                .to_vec(),
            desired_program: wire(input(1)),
            desired_preview: wire(input(2)),
            realized_program: wire(input(1)),
            realized_preview: wire(input(2)),
            desired_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            realized_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            desired_fade_to_black: fade,
            realized_fade_to_black: fade,
            stingers: Vec::new(),
            streams: Vec::new(),
            recordings: [1, 2]
                .map(|slot| RecordingStatus {
                    recorder: WireRecorderSlot::new(slot).unwrap(),
                    desired_active: false,
                })
                .to_vec(),
            desired_overlays: overlays(),
            realized_overlays: overlays(),
            edit_locks: fm_protocol::EditLockStatus::default(),
            edit_history: fm_protocol::EditHistoryStatus::EMPTY,
            titles: Vec::new(),
            scenes: Vec::new(),
            frame_rate: rate(),
            playlist_follows: playlist.is_some(),
            grade_states: 0,
        };
        let mut model = ClientModel::new(project);
        model
            .install_snapshot(ProjectSnapshot::from_protocol(project, snapshot))
            .unwrap();
        if let Some(playlist) = playlist {
            model
                .apply_playlist_state(PlaylistStateMessage {
                    revision: 10,
                    frame: playlist.frame,
                    loop_mode: playlist.loop_mode,
                    entries: playlist.entries,
                    state: playlist.state,
                })
                .unwrap();
        }
        model
    }

    fn overlays() -> Vec<fm_protocol::OverlayStatus> {
        (1..=8)
            .map(|channel| fm_protocol::OverlayStatus {
                channel: fm_protocol::WireOverlayChannelId::new(channel).unwrap(),
                source: None,
                active: false,
                opacity: 0,
                transition: OverlayTransitionKind::Cut,
                duration_frames: 1,
                position: OverlayPositionPreset::FullFrame,
                border: OverlayBorderPreset::None,
                queued_sources: Vec::new(),
                included_outputs: Vec::new(),
            })
            .collect()
    }

    fn ready_state(model: &ClientModel, edit: bool, air: bool) -> StudioUiState {
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_view(model.view().unwrap())
            .with_switcher_permissions(true, air)
            .with_edit_project_permission(edit);
        state.project_state = model.state().cloned();
        state
    }

    #[test]
    fn session_gates_block_transport_and_editing() {
        let stopped = list(PlaylistLoopMode::Off, PlaylistRunState::Stopped);
        let model = model(Some(stopped.clone()));
        let operator = ready_state(&model, true, true);
        assert_eq!(control_block(&operator), None);
        assert_eq!(edit_block(&operator, Some(&stopped)), None);
        assert_eq!(
            edit_block(&operator, Some(&list(PlaylistLoopMode::Off, playing(1, 0)))),
            Some(Msg::PlaylistReasonRunning.en())
        );
        assert_eq!(edit_block(&operator, None), Some(Msg::PlaylistWaiting.en()));
        let viewer = ready_state(&model, false, false);
        assert_eq!(control_block(&viewer), Some(Msg::ReasonNoTransition.en()));
        assert_eq!(
            edit_block(&viewer, Some(&stopped)),
            Some(Msg::ReasonNoEditProject.en())
        );
        let connecting = StudioUiState::new(StudioConnectionStatus::Connecting);
        assert!(control_block(&connecting).is_some());
        assert!(edit_block(&connecting, Some(&stopped)).is_some());
    }

    #[test]
    fn labels_word_phases_and_transitions_in_both_languages() {
        assert_eq!(PlaylistPhase::of(playing(1, 0)), PlaylistPhase::Playing);
        assert_eq!(PlaylistPhase::Playing.label(), "PLAYING");
        assert_eq!(PlaylistPhase::Stopped.tone(), Tone::Idle);
        assert_eq!(PlaylistPhase::Playing.tone(), Tone::OnAir);
        assert!(PlaylistPhase::Armed.running() && !PlaylistPhase::Finished.running());
        let fade = entry(2, 2, 125, true);
        assert_eq!(entry_transition_text(Locale::ENGLISH, &fade), "Fade 12 fr");
        assert_eq!(
            entry_transition_text(Locale::ENGLISH, &entry(1, 1, 1, true)),
            "Cut"
        );
        assert_ne!(entry_transition_text(ES, &fade), "Fade 12 fr");
        assert_eq!(ES.tr(PlaylistPhase::Playing.label()), "REPRODUCIENDO");
    }

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

    fn draw(
        context: &egui::Context,
        shell: &mut StudioShell,
        state: &StudioUiState,
    ) -> Vec<String> {
        context.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1600.0, 1000.0),
            )),
            ..egui::RawInput::default()
        });
        let mut ui = Ui::new(
            context.clone(),
            egui::Id::new("fm-ui-egui-playlist-test"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        let intents = shell.draw(&mut ui, state);
        assert!(intents.is_empty(), "{intents:?}");
        painted_text(&context.end_pass())
    }

    #[test]
    fn countdowns_carry_a_leading_minus_and_durations_do_not() {
        assert_eq!(countdown_timecode(37 * 25 + 24, rate()), "-00:00:37:24");
        assert_eq!(countdown_timecode(0, rate()), "-00:00:00:00");
        assert_eq!(
            countdown_timecode(250, rate()),
            format!("-{}", timecode(250, rate()))
        );
        assert!(!timecode(250, rate()).starts_with('-'));
    }

    /// One frame of the shell with `events`, returning its intents.
    fn frame(
        context: &egui::Context,
        shell: &mut StudioShell,
        state: &StudioUiState,
        events: Vec<egui::Event>,
    ) -> Vec<StudioIntent> {
        context.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1600.0, 1000.0),
            )),
            events,
            ..egui::RawInput::default()
        });
        let mut ui = Ui::new(
            context.clone(),
            egui::Id::new("fm-ui-egui-playlist-test"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        let intents = shell.draw(&mut ui, state);
        let _ = context.end_pass();
        intents
    }

    /// One frame without input, which must send nothing.
    fn idle(context: &egui::Context, shell: &mut StudioShell, state: &StudioUiState) {
        let intents = frame(context, shell, state, Vec::new());
        assert!(intents.is_empty(), "{intents:?}");
    }

    fn enter() -> egui::Event {
        egui::Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn selecting_a_row_focuses_the_form_and_enter_submits_it() {
        let stopped = list(PlaylistLoopMode::Off, PlaylistRunState::Stopped);
        let synced = model(Some(stopped.clone()));
        let state = ready_state(&synced, true, true);
        let context = egui::Context::default();
        let mut shell = StudioShell {
            side_tab: SideTab::Playlist,
            ..StudioShell::default()
        };
        idle(&context, &mut shell, &state);
        assert_eq!(context.memory(egui::Memory::focused), None);

        // Selecting entry 2 loads it and puts the keyboard on the input
        // picker, the form's first field, once.
        shell.playlist.select(&stopped.entries[1], rate());
        idle(&context, &mut shell, &state);
        let duration = egui::Id::new(DURATION_FIELD_ID);
        let picker = context.memory(egui::Memory::focused);
        assert!(picker.is_some_and(|id| id != duration), "{picker:?}");
        assert!(!shell.playlist.focus_form);

        // Enter in the duration field updates the loaded entry.
        context.memory_mut(|memory| memory.request_focus(duration));
        idle(&context, &mut shell, &state);
        if let Some(form) = &mut shell.playlist.form {
            form.duration = "7".into();
        }
        let intents = frame(&context, &mut shell, &state, vec![enter()]);
        match intents.as_slice() {
            [StudioIntent::Playlist(PlaylistIntent::Set { entries, .. })] => {
                assert_eq!(entries[1].id, 2);
                assert_eq!(entries[1].duration_frames, 7 * 25);
                assert_eq!(entries.len(), stopped.entries.len());
            }
            other => panic!("{other:?}"),
        }

        // With no entry loaded, Enter appends a new one.
        shell.playlist.selected = None;
        shell.playlist.form = Some(EntryForm {
            input: Some(input(1)),
            ..EntryForm::new(rate())
        });
        context.memory_mut(|memory| memory.request_focus(duration));
        idle(&context, &mut shell, &state);
        let intents = frame(&context, &mut shell, &state, vec![enter()]);
        match intents.as_slice() {
            [StudioIntent::Playlist(PlaylistIntent::Set { entries, .. })] => {
                assert_eq!(entries.len(), stopped.entries.len() + 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_playlist_tab_draws_a_running_list_in_both_languages() {
        let mut running = list(PlaylistLoopMode::All, playing(2, 100));
        running.frame = 100;
        let synced = model(Some(running));
        let state = ready_state(&synced, true, true);
        let context = egui::Context::default();
        let mut shell = StudioShell {
            side_tab: SideTab::Playlist,
            ..StudioShell::default()
        };
        let english = draw(&context, &mut shell, &state);
        for expected in [
            "PLAYLIST",
            "PLAYING",
            "Camera",
            "Slides",
            "ON AIR",
            "NEXT",
            "SKIPPED",
            "Fade 12 fr",
            "00:00:10:00",
            "START",
            "STOP",
            "PAUSE",
            "PREVIOUS",
            "Next: Slides",
            "4 ENTRIES",
        ] {
            assert!(
                english.iter().any(|text| text == expected),
                "missing {expected:?} in {english:?}"
            );
        }
        // The status card's REMAINING clock and the on-air row show the
        // same signed countdown; durations stay unsigned.
        let countdowns: Vec<_> = english
            .iter()
            .filter(|text| text.starts_with("-00:00:0"))
            .collect();
        assert_eq!(countdowns.len(), 2, "{english:?}");
        assert_eq!(countdowns[0], countdowns[1]);
        assert!(english.iter().any(|text| text == "REMAINING"));

        shell.set_locale(ES);
        let spanish = draw(&context, &mut shell, &state);
        for expected in [
            "REPRODUCIENDO",
            "AL AIRE",
            "SIGUIENTE",
            "INICIAR",
            "DETENER",
        ] {
            assert!(
                spanish.iter().any(|text| text == expected),
                "missing {expected:?} in {spanish:?}"
            );
        }

        // Before the playlist arrives the tab waits for it.
        let mut waiting = state.clone();
        if let Some(project) = &mut waiting.project_state {
            *project = model(None).state().cloned().unwrap();
        }
        shell.set_locale(Locale::ENGLISH);
        let english = draw(&context, &mut shell, &waiting);
        assert!(
            english.iter().any(|text| text == "NO ENTRIES"),
            "{english:?}"
        );
    }
}
