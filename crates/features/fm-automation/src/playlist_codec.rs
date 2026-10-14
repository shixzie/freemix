//! Versioned, tab-separated text encoding of a [`PlaylistSnapshot`].
//!
//! One record per line; the fields below are separated by single tab
//! characters (shown as spaces):
//!
//! ```text
//! freemix-playlist 1
//! config <off|all|one> <reject|start-now|join>
//! clock <observed_ms> <next_take>
//! state stopped
//! state armed <start_at_ms> <from|->
//! state playing <entry> <take> <cycle> <started_at_ms>
//! state paused <entry> <take> <cycle> <elapsed_ms>
//! state finished <last|->
//! entry <id> <input> <fixed|media> <ms|-> <transition> <transition_ms> <on|off>
//! ```
//!
//! Exactly one `state` line follows the `clock` line, then zero or more
//! `entry` lines in playlist order. Decoding is structural and all-or-nothing;
//! semantic consistency is checked by [`crate::Playlist::restore`].

use crate::{
    EntryDuration, LatePolicy, LoopMode, MAX_PLAYLIST_ENTRIES, PlaylistConfig, PlaylistEntry,
    PlaylistEntryId, PlaylistSnapshot, PlaylistState, PlaylistTransition, PlaylistTransitionKind,
};
use core::fmt;
use fm_types::InputId;
use std::num::NonZeroU128;

/// First field of the header line of an encoded playlist snapshot.
pub const PLAYLIST_SNAPSHOT_MAGIC: &str = "freemix-playlist";
/// Version field of the header line of an encoded playlist snapshot.
pub const PLAYLIST_SNAPSHOT_VERSION: &str = "1";

/// A malformed encoded snapshot. `line` is 1-based.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotCodecError {
    pub line: usize,
    pub reason: &'static str,
}

impl fmt::Display for SnapshotCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "playlist snapshot line {}: {}",
            self.line, self.reason
        )
    }
}

impl std::error::Error for SnapshotCodecError {}

/// Encodes a snapshot; [`decode_playlist_snapshot`] inverts it exactly.
#[must_use]
pub fn encode_playlist_snapshot(snapshot: &PlaylistSnapshot) -> String {
    let mut lines = vec![
        format!("{PLAYLIST_SNAPSHOT_MAGIC}\t{PLAYLIST_SNAPSHOT_VERSION}"),
        format!(
            "config\t{}\t{}",
            loop_name(snapshot.config.loop_mode),
            late_name(snapshot.config.late_policy)
        ),
        format!("clock\t{}\t{}", snapshot.observed_ms, snapshot.next_take),
        state_line(snapshot.state),
    ];
    lines.extend(snapshot.entries.iter().map(|entry| entry_line(*entry)));
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// Decodes a snapshot produced by [`encode_playlist_snapshot`].
///
/// # Errors
///
/// Returns the first structural error; no partial snapshot is returned.
pub fn decode_playlist_snapshot(text: &str) -> Result<PlaylistSnapshot, SnapshotCodecError> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let field_line = |number: usize| -> Result<Vec<&str>, SnapshotCodecError> {
        lines
            .get(number - 1)
            .map(|line| line.split('\t').collect())
            .ok_or(SnapshotCodecError {
                line: number,
                reason: "missing line",
            })
    };
    let header = field_line(1)?;
    if header.first() != Some(&PLAYLIST_SNAPSHOT_MAGIC) || header.len() != 2 {
        return Err(error(1, "not a playlist snapshot"));
    }
    if header[1] != PLAYLIST_SNAPSHOT_VERSION {
        return Err(error(1, "unsupported version"));
    }
    let config = field_line(2)?;
    let config = match config.as_slice() {
        ["config", loop_mode, late] => PlaylistConfig {
            loop_mode: parse_loop(loop_mode).ok_or(error(2, "unknown loop mode"))?,
            late_policy: parse_late(late).ok_or(error(2, "unknown late policy"))?,
        },
        _ => return Err(error(2, "expected config line")),
    };
    let clock = field_line(3)?;
    let (observed_ms, next_take) = match clock.as_slice() {
        ["clock", observed, next] => (number(3, observed)?, number(3, next)?),
        _ => return Err(error(3, "expected clock line")),
    };
    let state = parse_state(4, &field_line(4)?)?;
    let entry_lines = &lines[4..];
    if entry_lines.len() > MAX_PLAYLIST_ENTRIES {
        return Err(error(5 + MAX_PLAYLIST_ENTRIES, "too many entries"));
    }
    let entries = entry_lines
        .iter()
        .enumerate()
        .map(|(offset, line)| parse_entry(offset + 5, line))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PlaylistSnapshot {
        entries,
        config,
        observed_ms,
        next_take,
        state,
    })
}

const fn error(line: usize, reason: &'static str) -> SnapshotCodecError {
    SnapshotCodecError { line, reason }
}

fn number(line: usize, field: &str) -> Result<u64, SnapshotCodecError> {
    if field.is_empty() || !field.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(error(line, "expected a decimal number"));
    }
    field
        .parse()
        .map_err(|_| error(line, "number out of range"))
}

fn optional_id(line: usize, field: &str) -> Result<Option<PlaylistEntryId>, SnapshotCodecError> {
    if field == "-" {
        Ok(None)
    } else {
        number(line, field).map(|id| Some(PlaylistEntryId::new(id)))
    }
}

fn optional_id_text(id: Option<PlaylistEntryId>) -> String {
    id.map_or_else(|| "-".to_owned(), |id| id.to_string())
}

const fn loop_name(mode: LoopMode) -> &'static str {
    match mode {
        LoopMode::Off => "off",
        LoopMode::All => "all",
        LoopMode::One => "one",
    }
}

fn parse_loop(text: &str) -> Option<LoopMode> {
    [LoopMode::Off, LoopMode::All, LoopMode::One]
        .into_iter()
        .find(|mode| loop_name(*mode) == text)
}

const fn late_name(policy: LatePolicy) -> &'static str {
    match policy {
        LatePolicy::Reject => "reject",
        LatePolicy::StartNow => "start-now",
        LatePolicy::JoinInProgress => "join",
    }
}

fn parse_late(text: &str) -> Option<LatePolicy> {
    [
        LatePolicy::Reject,
        LatePolicy::StartNow,
        LatePolicy::JoinInProgress,
    ]
    .into_iter()
    .find(|policy| late_name(*policy) == text)
}

const TRANSITION_KINDS: [PlaylistTransitionKind; 7] = [
    PlaylistTransitionKind::Cut,
    PlaylistTransitionKind::Fade,
    PlaylistTransitionKind::AlphaFade,
    PlaylistTransitionKind::Wipe,
    PlaylistTransitionKind::Slide,
    PlaylistTransitionKind::Zoom,
    PlaylistTransitionKind::Stinger,
];

const fn transition_name(kind: PlaylistTransitionKind) -> &'static str {
    match kind {
        PlaylistTransitionKind::Cut => "cut",
        PlaylistTransitionKind::Fade => "fade",
        PlaylistTransitionKind::AlphaFade => "alpha-fade",
        PlaylistTransitionKind::Wipe => "wipe",
        PlaylistTransitionKind::Slide => "slide",
        PlaylistTransitionKind::Zoom => "zoom",
        PlaylistTransitionKind::Stinger => "stinger",
    }
}

fn state_line(state: PlaylistState) -> String {
    match state {
        PlaylistState::Stopped => "state\tstopped".to_owned(),
        PlaylistState::Armed { start_at_ms, from } => {
            format!("state\tarmed\t{start_at_ms}\t{}", optional_id_text(from))
        }
        PlaylistState::Playing {
            entry,
            take,
            cycle,
            started_at_ms,
        } => format!("state\tplaying\t{entry}\t{take}\t{cycle}\t{started_at_ms}"),
        PlaylistState::Paused {
            entry,
            take,
            cycle,
            elapsed_ms,
        } => format!("state\tpaused\t{entry}\t{take}\t{cycle}\t{elapsed_ms}"),
        PlaylistState::Finished { last } => {
            format!("state\tfinished\t{}", optional_id_text(last))
        }
    }
}

fn parse_state(line: usize, fields: &[&str]) -> Result<PlaylistState, SnapshotCodecError> {
    let id = |field: &str| number(line, field).map(PlaylistEntryId::new);
    match fields {
        ["state", "stopped"] => Ok(PlaylistState::Stopped),
        ["state", "armed", start, from] => Ok(PlaylistState::Armed {
            start_at_ms: number(line, start)?,
            from: optional_id(line, from)?,
        }),
        ["state", "playing", entry, take, cycle, started] => Ok(PlaylistState::Playing {
            entry: id(entry)?,
            take: number(line, take)?,
            cycle: number(line, cycle)?,
            started_at_ms: number(line, started)?,
        }),
        ["state", "paused", entry, take, cycle, elapsed] => Ok(PlaylistState::Paused {
            entry: id(entry)?,
            take: number(line, take)?,
            cycle: number(line, cycle)?,
            elapsed_ms: number(line, elapsed)?,
        }),
        ["state", "finished", last] => Ok(PlaylistState::Finished {
            last: optional_id(line, last)?,
        }),
        _ => Err(error(line, "expected state line")),
    }
}

fn entry_line(entry: PlaylistEntry) -> String {
    let (kind, ms) = match entry.duration {
        EntryDuration::Fixed { ms } => ("fixed", ms.to_string()),
        EntryDuration::Media { length_ms } => (
            "media",
            length_ms.map_or_else(|| "-".to_owned(), |ms| ms.to_string()),
        ),
    };
    format!(
        "entry\t{}\t{}\t{kind}\t{ms}\t{}\t{}\t{}",
        entry.id,
        entry.input,
        transition_name(entry.transition.kind),
        entry.transition.duration_ms,
        if entry.enabled { "on" } else { "off" }
    )
}

fn parse_entry(line: usize, text: &str) -> Result<PlaylistEntry, SnapshotCodecError> {
    let fields: Vec<&str> = text.split('\t').collect();
    let [
        "entry",
        id,
        input,
        kind,
        ms,
        transition,
        transition_ms,
        enabled,
    ] = fields.as_slice()
    else {
        return Err(error(line, "expected entry line"));
    };
    let input = input
        .parse::<u128>()
        .ok()
        .filter(|_| input.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(NonZeroU128::new)
        .map(InputId::new)
        .ok_or(error(line, "invalid input id"))?;
    let duration = match (*kind, *ms) {
        ("fixed", ms) => EntryDuration::Fixed {
            ms: number(line, ms)?,
        },
        ("media", "-") => EntryDuration::Media { length_ms: None },
        ("media", ms) => EntryDuration::Media {
            length_ms: Some(number(line, ms)?),
        },
        _ => return Err(error(line, "unknown duration kind")),
    };
    let kind = TRANSITION_KINDS
        .into_iter()
        .find(|candidate| transition_name(*candidate) == *transition)
        .ok_or(error(line, "unknown transition"))?;
    let enabled = match *enabled {
        "on" => true,
        "off" => false,
        _ => return Err(error(line, "expected on or off")),
    };
    Ok(PlaylistEntry {
        id: PlaylistEntryId::new(number(line, id)?),
        input,
        duration,
        transition: PlaylistTransition::new(kind, number(line, transition_ms)?),
        enabled,
    })
}
