//! Playlist entry lists, run states, start times, and loop modes.
//!
//! * `loop` is `off`, `all`, or `one`.
//! * `entries` is a comma-separated list (at most [`MAX_PLAYLIST_ENTRIES`])
//!   of `<id>:<input>:<duration frames>:<transition>:<transition frames>:<0|1>`
//!   where `<transition>` is `cut`, `fade`, `alpha_fade`, `wipe`, `slide`, or
//!   `zoom`, and the last item is the enabled flag. Identifiers are unique.
//! * `state` is `stopped`, `armed:<start frame>:<from|->`,
//!   `playing:<entry>:<take>:<cycle>:<started frame>`,
//!   `paused:<entry>:<take>:<cycle>:<elapsed frames>`, or
//!   `finished:<last|->`.
//! * `start` is `now`, `after:<frames>`, or `at:<frame>`.
//!
//! Every number is a canonical unsigned decimal.

use crate::{
    CodecError, MAX_PLAYLIST_ENTRIES, MAX_PLAYLIST_ENTRY_FRAMES, MAX_PLAYLIST_TRANSITION_FRAMES,
    PlaylistEntrySpec, PlaylistLoopMode, PlaylistRunState, PlaylistStart, PlaylistTransitionKind,
};

use super::value::{parse_canonical_input, parse_canonical_u64};

const ENTRIES: &str = "entries";

/// Enforces the entry-list invariants shared by the encoder and decoder.
pub(super) fn validate_playlist_entries(entries: &[PlaylistEntrySpec]) -> Result<(), CodecError> {
    if entries.len() > MAX_PLAYLIST_ENTRIES {
        return Err(CodecError::TooManyItems(ENTRIES));
    }
    for (index, entry) in entries.iter().enumerate() {
        let invalid = || CodecError::InvalidField {
            field: ENTRIES,
            value: entry.id.to_string(),
        };
        if entries[..index].iter().any(|other| other.id == entry.id) {
            return Err(invalid());
        }
        if entry.duration_frames == 0 || entry.duration_frames > MAX_PLAYLIST_ENTRY_FRAMES {
            return Err(invalid());
        }
        let valid_transition = match entry.transition {
            PlaylistTransitionKind::Cut => entry.transition_frames == 0,
            _ => {
                entry.transition_frames != 0
                    && entry.transition_frames <= MAX_PLAYLIST_TRANSITION_FRAMES
                    && u64::from(entry.transition_frames) <= entry.duration_frames
            }
        };
        if !valid_transition {
            return Err(invalid());
        }
    }
    Ok(())
}

const fn transition_name(kind: PlaylistTransitionKind) -> &'static str {
    match kind {
        PlaylistTransitionKind::Cut => "cut",
        PlaylistTransitionKind::Fade => "fade",
        PlaylistTransitionKind::AlphaFade => "alpha_fade",
        PlaylistTransitionKind::Wipe => "wipe",
        PlaylistTransitionKind::Slide => "slide",
        PlaylistTransitionKind::Zoom => "zoom",
    }
}

fn parse_transition(value: &str) -> Option<PlaylistTransitionKind> {
    Some(match value {
        "cut" => PlaylistTransitionKind::Cut,
        "fade" => PlaylistTransitionKind::Fade,
        "alpha_fade" => PlaylistTransitionKind::AlphaFade,
        "wipe" => PlaylistTransitionKind::Wipe,
        "slide" => PlaylistTransitionKind::Slide,
        "zoom" => PlaylistTransitionKind::Zoom,
        _ => return None,
    })
}

/// Encodes a validated entry list.
pub(super) fn encode_playlist_entries(entries: &[PlaylistEntrySpec]) -> Result<String, CodecError> {
    validate_playlist_entries(entries)?;
    Ok(entries
        .iter()
        .map(|entry| {
            format!(
                "{}:{}:{}:{}:{}:{}",
                entry.id,
                entry.input,
                entry.duration_frames,
                transition_name(entry.transition),
                entry.transition_frames,
                u8::from(entry.enabled)
            )
        })
        .collect::<Vec<_>>()
        .join(","))
}

/// Strictly decodes an entry list and enforces [`validate_playlist_entries`].
pub(super) fn decode_playlist_entries(value: &str) -> Result<Vec<PlaylistEntrySpec>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').take(MAX_PLAYLIST_ENTRIES + 1).count() > MAX_PLAYLIST_ENTRIES {
        return Err(CodecError::TooManyItems(ENTRIES));
    }
    let entries = value
        .split(',')
        .map(|item| {
            let invalid = || CodecError::InvalidField {
                field: ENTRIES,
                value: item.to_owned(),
            };
            let parts: Vec<_> = item.split(':').collect();
            let [id, input, duration, transition, frames, enabled] = parts[..] else {
                return Err(invalid());
            };
            Ok(PlaylistEntrySpec {
                id: parse_canonical_u64(id).ok_or_else(invalid)?,
                input: parse_canonical_input(input).ok_or_else(invalid)?,
                duration_frames: parse_canonical_u64(duration).ok_or_else(invalid)?,
                transition: parse_transition(transition).ok_or_else(invalid)?,
                transition_frames: parse_canonical_u64(frames)
                    .and_then(|frames| u32::try_from(frames).ok())
                    .ok_or_else(invalid)?,
                enabled: match enabled {
                    "0" => false,
                    "1" => true,
                    _ => return Err(invalid()),
                },
            })
        })
        .collect::<Result<Vec<_>, CodecError>>()?;
    validate_playlist_entries(&entries)?;
    Ok(entries)
}

pub(super) const fn playlist_loop(mode: PlaylistLoopMode) -> &'static str {
    match mode {
        PlaylistLoopMode::Off => "off",
        PlaylistLoopMode::All => "all",
        PlaylistLoopMode::One => "one",
    }
}

pub(super) fn parse_playlist_loop(value: String) -> Result<PlaylistLoopMode, CodecError> {
    match value.as_str() {
        "off" => Ok(PlaylistLoopMode::Off),
        "all" => Ok(PlaylistLoopMode::All),
        "one" => Ok(PlaylistLoopMode::One),
        _ => Err(CodecError::InvalidField {
            field: "loop",
            value,
        }),
    }
}

fn optional_id(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |id| id.to_string())
}

#[allow(clippy::option_option)] // `None` is a parse failure, `Some(None)` is `-`.
fn parse_optional_id(value: &str) -> Option<Option<u64>> {
    if value == "-" {
        Some(None)
    } else {
        parse_canonical_u64(value).map(Some)
    }
}

pub(super) fn encode_playlist_state(state: PlaylistRunState) -> String {
    match state {
        PlaylistRunState::Stopped => "stopped".to_owned(),
        PlaylistRunState::Armed { start_frame, from } => {
            format!("armed:{start_frame}:{}", optional_id(from))
        }
        PlaylistRunState::Playing {
            entry,
            take,
            cycle,
            started_frame,
        } => format!("playing:{entry}:{take}:{cycle}:{started_frame}"),
        PlaylistRunState::Paused {
            entry,
            take,
            cycle,
            elapsed_frames,
        } => format!("paused:{entry}:{take}:{cycle}:{elapsed_frames}"),
        PlaylistRunState::Finished { last } => format!("finished:{}", optional_id(last)),
    }
}

fn parse_playlist_state(value: &str) -> Option<PlaylistRunState> {
    let parts: Vec<_> = value.split(':').collect();
    let number = |index: usize| parse_canonical_u64(parts[index]);
    Some(match parts[..] {
        ["stopped"] => PlaylistRunState::Stopped,
        ["armed", _, from] => PlaylistRunState::Armed {
            start_frame: number(1)?,
            from: parse_optional_id(from)?,
        },
        ["playing", _, _, _, _] => PlaylistRunState::Playing {
            entry: number(1)?,
            take: number(2)?,
            cycle: number(3)?,
            started_frame: number(4)?,
        },
        ["paused", _, _, _, _] => PlaylistRunState::Paused {
            entry: number(1)?,
            take: number(2)?,
            cycle: number(3)?,
            elapsed_frames: number(4)?,
        },
        ["finished", last] => PlaylistRunState::Finished {
            last: parse_optional_id(last)?,
        },
        _ => return None,
    })
}

pub(super) fn decode_playlist_state(value: String) -> Result<PlaylistRunState, CodecError> {
    parse_playlist_state(&value).ok_or(CodecError::InvalidField {
        field: "state",
        value,
    })
}

pub(super) fn encode_playlist_start(start: PlaylistStart) -> String {
    match start {
        PlaylistStart::Now => "now".to_owned(),
        PlaylistStart::AfterFrames(frames) => format!("after:{frames}"),
        PlaylistStart::AtFrame(frame) => format!("at:{frame}"),
    }
}

pub(super) fn decode_playlist_start(value: String) -> Result<PlaylistStart, CodecError> {
    let parsed = match value.split_once(':') {
        None if value == "now" => Some(PlaylistStart::Now),
        Some(("after", frames)) => parse_canonical_u64(frames).map(PlaylistStart::AfterFrames),
        Some(("at", frame)) => parse_canonical_u64(frame).map(PlaylistStart::AtFrame),
        _ => None,
    };
    parsed.ok_or(CodecError::InvalidField {
        field: "start",
        value,
    })
}

/// Parses an optional `-`-or-identifier value (used by `from`).
pub(super) fn decode_optional_entry(
    field: &'static str,
    value: String,
) -> Result<Option<u64>, CodecError> {
    parse_optional_id(&value).ok_or(CodecError::InvalidField { field, value })
}

pub(super) fn encode_optional_entry(value: Option<u64>) -> String {
    optional_id(value)
}
