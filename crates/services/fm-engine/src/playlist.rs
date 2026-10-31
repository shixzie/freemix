//! The show's playlist, driven by the engine frame cursor.
//!
//! The playlist engine (`fm_automation::Playlist`) never reads a clock; the
//! engine feeds it frame numbers, so every "millisecond" in that API is one
//! engine frame here and every entry boundary is an exact frame. Durations
//! are always fixed: media-length entries are not offered until media end
//! detection exists.
//!
//! Playlist commands are ordinary engine commands. Editing the list is a
//! project edit; control operations and the clock-driven
//! [`EngineCommand::PlaylistAdvance`] are live operations. When one of them
//! puts an entry on air, the same acceptance selects the entry's input and
//! takes it to Program with the entry's transition, scheduled for the
//! command's target frame, so the cut is realized on exactly that frame.

use fm_automation::{
    EntryDuration, LatePolicy, LoopMode, Playlist, PlaylistCommand, PlaylistConfig, PlaylistEntry,
    PlaylistEntryId, PlaylistError, PlaylistEventKind, PlaylistSnapshot, PlaylistState,
    PlaylistTransition, PlaylistTransitionKind, SnapshotError as PlaylistSnapshotError, StartAt,
};
use fm_command::{Rejection, RejectionCode};
use fm_switcher::{SwitcherCommand, SwitcherError, SwitcherEvent, SwitcherState, TransitionKind};
use fm_types::InputId;

use crate::{EngineCommand, ShowState};

/// The longest program transition a playlist entry may request, in frames.
pub const MAX_PLAYLIST_TRANSITION_FRAMES: u32 = fm_model::MAX_PLAYLIST_TRANSITION_FRAMES;

// The persisted playlist and the live one must agree on every bound.
const _: () = {
    assert!(fm_model::MAX_PLAYLIST_ENTRIES <= fm_automation::MAX_PLAYLIST_ENTRIES);
    assert!(fm_model::MAX_PLAYLIST_ENTRY_FRAMES == fm_automation::MAX_PLAYLIST_ENTRY_MS);
};

/// When a started playlist puts its first entry on air.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnginePlaylistStart {
    /// On the command's target frame.
    Now,
    /// `frames` frames after the command's target frame.
    AfterFrames(u64),
    /// On an absolute frame, which must not already have passed.
    AtFrame(u64),
}

/// An operator control of the playlist's automation state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnginePlaylistOperation {
    Start {
        at: EnginePlaylistStart,
        from: Option<PlaylistEntryId>,
    },
    Stop,
    Pause,
    Resume,
    Next,
    Previous,
    Jump(PlaylistEntryId),
}

impl EnginePlaylistOperation {
    const fn automation(self) -> PlaylistCommand {
        match self {
            Self::Start { at, from } => PlaylistCommand::Start {
                at: match at {
                    EnginePlaylistStart::Now => StartAt::Now,
                    EnginePlaylistStart::AfterFrames(frames) => StartAt::After { delay_ms: frames },
                    EnginePlaylistStart::AtFrame(frame) => StartAt::At { wall_ms: frame },
                },
                from,
            },
            Self::Stop => PlaylistCommand::Stop,
            Self::Pause => PlaylistCommand::Pause,
            Self::Resume => PlaylistCommand::Resume,
            Self::Next => PlaylistCommand::Next,
            Self::Previous => PlaylistCommand::Previous,
            Self::Jump(entry) => PlaylistCommand::JumpTo(entry),
        }
    }
}

/// An entry the playlist put on air: take `input` to Program with
/// `transition` (whose duration is in frames).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaylistCue {
    pub entry: PlaylistEntryId,
    pub take: u64,
    pub input: InputId,
    pub transition: PlaylistTransition,
}

/// The playlist after one accepted command and the airing it started.
#[derive(Clone, Debug)]
pub(crate) struct PlaylistStep {
    pub(crate) playlist: Playlist,
    pub(crate) cue: Option<PlaylistCue>,
}

/// The frame the playlist's next time-driven boundary falls on: an armed
/// start, or the end of the playing entry. `None` while stopped, paused, or
/// finished.
#[must_use]
pub fn playlist_due_frame(playlist: &Playlist) -> Option<u64> {
    match playlist.state() {
        PlaylistState::Armed { start_at_ms, .. } => Some(start_at_ms),
        PlaylistState::Playing {
            entry,
            started_at_ms,
            ..
        } => playlist
            .entries()
            .iter()
            .find(|candidate| candidate.id == entry)
            .and_then(|candidate| candidate.duration.known_ms())
            .map(|duration| started_at_ms.saturating_add(duration)),
        PlaylistState::Stopped | PlaylistState::Paused { .. } | PlaylistState::Finished { .. } => {
            None
        }
    }
}

pub(crate) const fn is_playlist_command(command: &EngineCommand) -> bool {
    matches!(
        command,
        EngineCommand::SetPlaylist { .. }
            | EngineCommand::PlaylistControl(_)
            | EngineCommand::PlaylistAdvance { .. }
    )
}

/// Applies a playlist command to a copy of the show's playlist at `frame`.
pub(crate) fn playlist_step(
    state: &ShowState,
    command: &EngineCommand,
    frame: u64,
) -> Result<PlaylistStep, Rejection> {
    let mut playlist = state.playlist().clone();
    let events = match command {
        EngineCommand::SetPlaylist { entries, loop_mode } => {
            validate_entries(state, entries)?;
            playlist
                .replace_entries(entries.clone())
                .map_err(playlist_rejection)?;
            playlist
                .apply(frame, PlaylistCommand::SetLoopMode(*loop_mode))
                .map_err(playlist_rejection)?
        }
        EngineCommand::PlaylistControl(operation) => playlist
            .apply(frame, operation.automation())
            .map_err(playlist_rejection)?,
        EngineCommand::PlaylistAdvance { .. } => {
            playlist.poll(frame).map_err(playlist_rejection)?
        }
        _ => unreachable!("only playlist commands step the playlist"),
    };
    let cue = events.iter().rev().find_map(|event| match event.kind {
        PlaylistEventKind::EntryOn {
            entry,
            input,
            transition,
            take,
            ..
        } => Some(PlaylistCue {
            entry,
            take,
            input,
            transition,
        }),
        _ => None,
    });
    Ok(PlaylistStep { playlist, cue })
}

fn validate_entries(state: &ShowState, entries: &[PlaylistEntry]) -> Result<(), Rejection> {
    if entries.len() > fm_model::MAX_PLAYLIST_ENTRIES {
        return Err(Rejection::new(
            RejectionCode::InvalidCommand,
            format!(
                "a playlist holds at most {} entries",
                fm_model::MAX_PLAYLIST_ENTRIES
            ),
        ));
    }
    for entry in entries {
        if !state.inputs().contains(&entry.input) {
            return Err(Rejection::new(
                RejectionCode::NotFound,
                format!(
                    "playlist entry {} names input {}, which is not part of this show",
                    entry.id, entry.input
                ),
            ));
        }
        if !matches!(entry.duration, EntryDuration::Fixed { .. }) {
            return Err(Rejection::new(
                RejectionCode::InvalidCommand,
                format!("playlist entry {} must have a fixed duration", entry.id),
            ));
        }
        if entry.transition.kind == PlaylistTransitionKind::Stinger
            || entry.transition.duration_ms > u64::from(MAX_PLAYLIST_TRANSITION_FRAMES)
        {
            return Err(Rejection::new(
                RejectionCode::InvalidCommand,
                format!(
                    "playlist entry {} transition must be a cut, fade, alpha fade, wipe, slide, \
                     or zoom of at most {MAX_PLAYLIST_TRANSITION_FRAMES} frames",
                    entry.id
                ),
            ));
        }
    }
    Ok(())
}

fn playlist_rejection(error: PlaylistError) -> Rejection {
    let code = match error {
        PlaylistError::UnknownEntry(_) => RejectionCode::NotFound,
        PlaylistError::TooManyEntries { .. }
        | PlaylistError::DuplicateEntry(_)
        | PlaylistError::ZeroDuration(_)
        | PlaylistError::DurationTooLong { .. }
        | PlaylistError::InvalidTransition(_)
        | PlaylistError::TransitionLongerThanEntry { .. }
        | PlaylistError::TimeOutOfRange { .. }
        | PlaylistError::NotMediaDuration(_) => RejectionCode::InvalidCommand,
        PlaylistError::ClockRegressed { .. }
        | PlaylistError::Empty
        | PlaylistError::NoEnabledEntries
        | PlaylistError::EntryDisabled(_)
        | PlaylistError::AlreadyActive
        | PlaylistError::NotOnAir
        | PlaylistError::NotPlaying
        | PlaylistError::NoNextEntry
        | PlaylistError::NoPreviousEntry
        | PlaylistError::ScheduleInPast { .. }
        | PlaylistError::StaleTake { .. }
        | PlaylistError::EditWhileActive => RejectionCode::Conflict,
    };
    // The automation engine words time in milliseconds; here it is frames.
    let rejection = Rejection::new(code, error.to_string().replace(" ms", " frames"));
    match error {
        PlaylistError::UnknownEntry(entry) => {
            rejection.with_reason(fm_command::RefusalReason::UnknownId {
                kind: fm_command::IdKind::PlaylistEntry,
                id: u128::from(entry.get()),
            })
        }
        PlaylistError::DuplicateEntry(entry) => {
            rejection.with_reason(fm_command::RefusalReason::DuplicateId {
                kind: fm_command::IdKind::PlaylistEntry,
                id: u128::from(entry.get()),
            })
        }
        PlaylistError::TooManyEntries { max, .. } => {
            rejection.with_reason(fm_command::RefusalReason::bounds("entries", max as u64))
        }
        _ => rejection,
    }
}

/// The program transition a cue starts on `switcher`, if any. A cue for the
/// input already on Program changes nothing.
pub(crate) fn cue_transition_kind(
    switcher: &SwitcherState,
    cue: &PlaylistCue,
) -> Option<TransitionKind> {
    if switcher.program() == cue.input {
        return None;
    }
    match cue.transition.kind {
        PlaylistTransitionKind::Cut | PlaylistTransitionKind::Stinger => None,
        PlaylistTransitionKind::Fade => Some(TransitionKind::Fade),
        PlaylistTransitionKind::AlphaFade => Some(TransitionKind::AlphaFade),
        PlaylistTransitionKind::Wipe => Some(TransitionKind::Wipe),
        PlaylistTransitionKind::Slide => Some(TransitionKind::Slide),
        PlaylistTransitionKind::Zoom => Some(TransitionKind::Zoom),
    }
}

/// Moves the desired switcher to the cue's end state: the cue's input on
/// Program and the previous Program on Preview.
pub(crate) fn apply_desired_cue(
    switcher: &mut SwitcherState,
    cue: &PlaylistCue,
) -> Result<(), SwitcherError> {
    if switcher.program() == cue.input {
        return Ok(());
    }
    if switcher.t_bar().is_some() {
        return Err(SwitcherError::TransitionInProgress);
    }
    switcher.apply(SwitcherCommand::SelectPreview(cue.input))?;
    switcher.apply(SwitcherCommand::Cut)?;
    Ok(())
}

/// Realizes a cue on the realized switcher at its target frame.
pub(crate) fn realize_cue(
    switcher: &mut SwitcherState,
    cue: &PlaylistCue,
) -> Result<Vec<SwitcherEvent>, SwitcherError> {
    if switcher.program() == cue.input {
        return Ok(Vec::new());
    }
    let mut events = switcher.apply(SwitcherCommand::SelectPreview(cue.input))?;
    let duration_frames = u32::try_from(cue.transition.duration_ms)
        .expect("accepted playlist transitions are bounded");
    let take = match cue_transition_kind(switcher, cue) {
        None => SwitcherCommand::Cut,
        Some(TransitionKind::Wipe) => SwitcherCommand::Wipe { duration_frames },
        Some(kind) => SwitcherCommand::Transition {
            kind,
            duration_frames,
        },
    };
    events.extend(switcher.apply(take)?);
    Ok(events)
}

/// Why a persisted playlist could not be restored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlaylistRestoreError {
    Entries(fm_model::PlaylistEntryError),
    State(PlaylistSnapshotError),
}

impl std::fmt::Display for PlaylistRestoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Entries(error) => error.fmt(formatter),
            Self::State(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PlaylistRestoreError {}

/// Rebuilds the live playlist from its persisted form, checking that the
/// state is one the engine could have produced.
///
/// # Errors
///
/// Returns a [`PlaylistRestoreError`] for structurally invalid entries or an
/// inconsistent state.
pub fn playlist_from_model(model: &fm_model::Playlist) -> Result<Playlist, PlaylistRestoreError> {
    fm_model::validate_playlist_entries(&model.entries).map_err(PlaylistRestoreError::Entries)?;
    let entries = model
        .entries
        .iter()
        .map(|entry| {
            PlaylistEntry::new(
                PlaylistEntryId::new(entry.id),
                entry.input,
                if entry.follow_media {
                    EntryDuration::Media { length_ms: None }
                } else {
                    EntryDuration::Fixed {
                        ms: entry.duration_frames,
                    }
                },
            )
            .with_transition(PlaylistTransition::new(
                automation_transition_kind(entry.transition.kind),
                u64::from(entry.transition.duration_frames),
            ))
            .with_enabled(entry.enabled)
        })
        .collect();
    let id = PlaylistEntryId::new;
    let state = match model.state {
        fm_model::PlaylistRunState::Stopped => PlaylistState::Stopped,
        fm_model::PlaylistRunState::Armed { start_frame, from } => PlaylistState::Armed {
            start_at_ms: start_frame,
            from: from.map(id),
        },
        fm_model::PlaylistRunState::Playing {
            entry,
            take,
            cycle,
            started_frame,
        } => PlaylistState::Playing {
            entry: id(entry),
            take,
            cycle,
            started_at_ms: started_frame,
        },
        fm_model::PlaylistRunState::Paused {
            entry,
            take,
            cycle,
            elapsed_frames,
        } => PlaylistState::Paused {
            entry: id(entry),
            take,
            cycle,
            elapsed_ms: elapsed_frames,
        },
        fm_model::PlaylistRunState::Finished { last } => {
            PlaylistState::Finished { last: last.map(id) }
        }
    };
    Playlist::restore(PlaylistSnapshot {
        entries,
        config: PlaylistConfig {
            loop_mode: automation_loop(model.loop_mode),
            late_policy: LatePolicy::Reject,
        },
        observed_ms: model.observed_frame,
        next_take: model.next_take,
        state,
    })
    .map_err(PlaylistRestoreError::State)
}

/// The persisted form of a live playlist.
///
/// # Panics
///
/// Never for a playlist the engine built: its durations are fixed and its
/// transitions bounded.
#[must_use]
pub fn playlist_to_model(playlist: &Playlist) -> fm_model::Playlist {
    let snapshot = playlist.snapshot();
    fm_model::Playlist {
        loop_mode: model_loop(snapshot.config.loop_mode),
        entries: snapshot
            .entries
            .iter()
            .map(|entry| fm_model::PlaylistEntry {
                id: entry.id.get(),
                input: entry.input,
                duration_frames: entry
                    .duration
                    .known_ms()
                    .unwrap_or_else(|| u64::from(entry.transition.duration_ms).max(1)),
                follow_media: matches!(entry.duration, EntryDuration::Media { .. }),
                transition: fm_model::PlaylistTransition {
                    kind: model_transition_kind(entry.transition.kind),
                    duration_frames: u32::try_from(entry.transition.duration_ms)
                        .expect("engine playlist transitions are bounded"),
                },
                enabled: entry.enabled,
            })
            .collect(),
        state: match snapshot.state {
            PlaylistState::Stopped => fm_model::PlaylistRunState::Stopped,
            PlaylistState::Armed { start_at_ms, from } => fm_model::PlaylistRunState::Armed {
                start_frame: start_at_ms,
                from: from.map(PlaylistEntryId::get),
            },
            PlaylistState::Playing {
                entry,
                take,
                cycle,
                started_at_ms,
            } => fm_model::PlaylistRunState::Playing {
                entry: entry.get(),
                take,
                cycle,
                started_frame: started_at_ms,
            },
            PlaylistState::Paused {
                entry,
                take,
                cycle,
                elapsed_ms,
            } => fm_model::PlaylistRunState::Paused {
                entry: entry.get(),
                take,
                cycle,
                elapsed_frames: elapsed_ms,
            },
            PlaylistState::Finished { last } => fm_model::PlaylistRunState::Finished {
                last: last.map(PlaylistEntryId::get),
            },
        },
        next_take: snapshot.next_take,
        observed_frame: snapshot.observed_ms,
    }
}

const fn automation_loop(mode: fm_model::PlaylistLoop) -> LoopMode {
    match mode {
        fm_model::PlaylistLoop::Off => LoopMode::Off,
        fm_model::PlaylistLoop::All => LoopMode::All,
        fm_model::PlaylistLoop::One => LoopMode::One,
    }
}

const fn model_loop(mode: LoopMode) -> fm_model::PlaylistLoop {
    match mode {
        LoopMode::Off => fm_model::PlaylistLoop::Off,
        LoopMode::All => fm_model::PlaylistLoop::All,
        LoopMode::One => fm_model::PlaylistLoop::One,
    }
}

const fn automation_transition_kind(
    kind: fm_model::PlaylistTransitionKind,
) -> PlaylistTransitionKind {
    match kind {
        fm_model::PlaylistTransitionKind::Cut => PlaylistTransitionKind::Cut,
        fm_model::PlaylistTransitionKind::Fade => PlaylistTransitionKind::Fade,
        fm_model::PlaylistTransitionKind::AlphaFade => PlaylistTransitionKind::AlphaFade,
        fm_model::PlaylistTransitionKind::Wipe => PlaylistTransitionKind::Wipe,
        fm_model::PlaylistTransitionKind::Slide => PlaylistTransitionKind::Slide,
        fm_model::PlaylistTransitionKind::Zoom => PlaylistTransitionKind::Zoom,
    }
}

fn model_transition_kind(kind: PlaylistTransitionKind) -> fm_model::PlaylistTransitionKind {
    match kind {
        PlaylistTransitionKind::Cut => fm_model::PlaylistTransitionKind::Cut,
        PlaylistTransitionKind::Fade => fm_model::PlaylistTransitionKind::Fade,
        PlaylistTransitionKind::AlphaFade => fm_model::PlaylistTransitionKind::AlphaFade,
        PlaylistTransitionKind::Wipe => fm_model::PlaylistTransitionKind::Wipe,
        PlaylistTransitionKind::Slide => fm_model::PlaylistTransitionKind::Slide,
        PlaylistTransitionKind::Zoom => fm_model::PlaylistTransitionKind::Zoom,
        PlaylistTransitionKind::Stinger => {
            unreachable!("the engine never accepts a Stinger playlist transition")
        }
    }
}
