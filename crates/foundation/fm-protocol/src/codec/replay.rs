//! Instant replay codec (OR-012); see [`crate::replay`] for the contract.

use fm_types::replay::{ReplaySourceCheck, ReplayTransition, ReplayTransitionKind};

use crate::{
    CodecError, MAX_REPLAY_EVENTS, MAX_REPLAY_INPUTS, MAX_REPLAY_PLAYLIST_ENTRIES,
    MAX_REPLAY_RECORD_SOURCES, ReplayEventStatus, ReplayFeed, ReplayOperation, ReplayPlayerStatus,
    ReplayPlaylistEntry, ReplaySettings, ReplaySourceStatus, ReplaySpeed, ReplayStatusMessage,
    WireInputId, replay_event_name_is_valid,
};

use super::media::{FieldSource, Push};
use super::value::{escape_bounded, parse_canonical_input, parse_canonical_u64, unescape};

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn parse_number<T: TryFrom<u64>>(field: &'static str, value: &str) -> Result<T, CodecError> {
    parse_canonical_u64(value)
        .and_then(|value| T::try_from(value).ok())
        .ok_or_else(|| invalid(field, value))
}

fn parse_input(field: &'static str, value: &str) -> Result<WireInputId, CodecError> {
    parse_canonical_input(value).ok_or_else(|| invalid(field, value))
}

fn parse_flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

fn parse_optional<T>(
    value: &str,
    parse: impl FnOnce(&str) -> Result<T, CodecError>,
) -> Result<Option<T>, CodecError> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse(value).map(Some)
    }
}

fn optional_text(value: Option<impl ToString>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn check_settings(field: &'static str, settings: &ReplaySettings) -> Result<(), CodecError> {
    settings
        .check(|_| ReplaySourceCheck::Recordable)
        .map_err(|_| invalid(field, format!("{settings:?}")))
}

fn check_name(field: &'static str, name: &str) -> Result<(), CodecError> {
    if replay_event_name_is_valid(name) {
        Ok(())
    } else {
        Err(invalid(field, name))
    }
}

fn parse_speed(field: &'static str, value: &str) -> Result<ReplaySpeed, CodecError> {
    parse_number::<u8>(field, value)
        .ok()
        .and_then(ReplaySpeed::from_percent)
        .ok_or_else(|| invalid(field, value))
}

/// Encodes a playlist as `event/angle/speed` entries joined by `|`.
fn encode_playlist(entries: &[ReplayPlaylistEntry]) -> Result<String, CodecError> {
    if entries.len() > MAX_REPLAY_PLAYLIST_ENTRIES {
        return Err(CodecError::TooManyItems("entries"));
    }
    Ok(entries
        .iter()
        .map(|entry| {
            format!(
                "{}/{}/{}",
                entry.event,
                optional_text(entry.angle),
                entry.speed.percent()
            )
        })
        .collect::<Vec<_>>()
        .join("|"))
}

fn decode_playlist(value: &str) -> Result<Vec<ReplayPlaylistEntry>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<&str> = value.split('|').collect();
    if entries.len() > MAX_REPLAY_PLAYLIST_ENTRIES {
        return Err(CodecError::TooManyItems("entries"));
    }
    entries
        .into_iter()
        .map(|entry| {
            let [event, angle, speed] = entry.split('/').collect::<Vec<_>>()[..] else {
                return Err(invalid("entries", entry));
            };
            Ok(ReplayPlaylistEntry {
                event: parse_number("entries", event)?,
                angle: parse_optional(angle, |value| parse_input("entries", value))?,
                speed: parse_speed("entries", speed)?,
            })
        })
        .collect()
}

fn encode_sources(sources: &[fm_types::InputId]) -> String {
    sources
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn decode_sources(value: &str) -> Result<Vec<fm_types::InputId>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<&str> = value.split(',').collect();
    if entries.len() > MAX_REPLAY_RECORD_SOURCES {
        return Err(CodecError::TooManyItems("sources"));
    }
    entries
        .into_iter()
        .map(|entry| parse_input("sources", entry).map(WireInputId::to_domain))
        .collect()
}

fn push_settings(settings: &ReplaySettings, push: &mut Push<'_>) -> Result<(), CodecError> {
    check_settings("sources", settings)?;
    push("sources", encode_sources(&settings.sources))?;
    push("retention_seconds", settings.retention_seconds.to_string())?;
    if let Some(root) = &settings.storage_root {
        push("storage_root", root.clone())?;
    }
    if let Some(frames) = settings.auto_return {
        push("auto_return", frames.to_string())?;
    }
    push("transition", encode_transition(settings.transition))?;
    Ok(())
}

fn encode_transition(transition: ReplayTransition) -> String {
    match transition.kind {
        ReplayTransitionKind::Cut => "cut".to_owned(),
        ReplayTransitionKind::Fade => format!("fade:{}", transition.duration_frames),
    }
}

fn decode_transition(value: &str) -> Result<ReplayTransition, CodecError> {
    match value {
        "cut" => Ok(ReplayTransition::CUT),
        _ => {
            let Some(frames) = value.strip_prefix("fade:") else {
                return Err(invalid("transition", value));
            };
            let frames = parse_number("transition", frames)?;
            ReplayTransition::new(ReplayTransitionKind::Fade, frames)
                .map_err(|_| invalid("transition", value))
        }
    }
}

fn decode_settings(fields: &mut dyn FieldSource) -> Result<ReplaySettings, CodecError> {
    let settings = ReplaySettings {
        sources: decode_sources(&fields.required("sources")?)?,
        retention_seconds: parse_number(
            "retention_seconds",
            &fields.required("retention_seconds")?,
        )?,
        storage_root: fields.optional("storage_root"),
        auto_return: fields
            .optional("auto_return")
            .map(|frames| parse_number("auto_return", &frames))
            .transpose()?,
        transition: decode_transition(&fields.required("transition")?)?,
    };
    check_settings("sources", &settings)?;
    Ok(settings)
}

/// Encodes an operation's fields after its token (`payload` or
/// `operation`).
pub(super) fn encode_operation(
    operation: &ReplayOperation,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    match operation {
        ReplayOperation::Configure(settings) => push_settings(settings, push),
        ReplayOperation::MarkIn | ReplayOperation::MarkOut => Ok(()),
        ReplayOperation::CaptureMarks { name } => match name {
            Some(name) => {
                check_name("name", name)?;
                push("name", name.clone())
            }
            None => Ok(()),
        },
        ReplayOperation::CaptureLast { seconds } => {
            if !crate::REPLAY_LAST_SECONDS.contains(seconds) {
                return Err(invalid("seconds", seconds.to_string()));
            }
            push("seconds", seconds.to_string())
        }
        ReplayOperation::RenameEvent { event, name } => {
            check_name("name", name)?;
            push("event_id", event.to_string())?;
            push("name", name.clone())
        }
        ReplayOperation::DeleteEvent { event } => push("event_id", event.to_string()),
        ReplayOperation::Cue {
            input,
            event,
            angle,
        } => {
            push("input", input.to_string())?;
            push("event_id", event.to_string())?;
            if let Some(angle) = angle {
                push("angle", angle.to_string())?;
            }
            Ok(())
        }
        ReplayOperation::Angle { input, angle } => {
            push("input", input.to_string())?;
            push("angle", angle.to_string())
        }
        ReplayOperation::Play { input, speed } => {
            push("input", input.to_string())?;
            push("speed", speed.percent().to_string())
        }
        ReplayOperation::Shuttle { input, rate_milli } => {
            if !(-16_000..=16_000).contains(rate_milli) {
                return Err(invalid("rate", rate_milli.to_string()));
            }
            push("input", input.to_string())?;
            push("rate", rate_milli.to_string())
        }
        ReplayOperation::Pause { input }
        | ReplayOperation::CueIn { input }
        | ReplayOperation::PlaylistNext { input }
        | ReplayOperation::PlaylistPrev { input } => push("input", input.to_string()),
        ReplayOperation::Step { input, frames } => {
            if !crate::replay_step_is_valid(*frames) {
                return Err(invalid("frames", frames.to_string()));
            }
            push("input", input.to_string())?;
            push("frames", frames.to_string())
        }
        ReplayOperation::Link { input, linked } => {
            push("input", input.to_string())?;
            push(
                "linked",
                linked.map(|linked| linked.to_string()).unwrap_or_default(),
            )
        }
        ReplayOperation::PlaylistSet { input, entries } => {
            push("input", input.to_string())?;
            push("entries", encode_playlist(entries)?)
        }
        ReplayOperation::Return { input, to } => {
            push("input", input.to_string())?;
            push("to", to.to_string())
        }
    }
}

/// Decodes the operation named `token`, or `None` when `token` names none.
pub(super) fn decode_operation(
    token: &str,
    fields: &mut dyn FieldSource,
) -> Result<Option<ReplayOperation>, CodecError> {
    let input = |fields: &mut dyn FieldSource| parse_input("input", &fields.required("input")?);
    Ok(Some(match token {
        "replay_configure" => ReplayOperation::Configure(Box::new(decode_settings(fields)?)),
        "replay_mark_in" => ReplayOperation::MarkIn,
        "replay_mark_out" => ReplayOperation::MarkOut,
        "replay_capture_marks" => ReplayOperation::CaptureMarks {
            name: fields
                .optional("name")
                .map(|name| check_name("name", &name).map(|()| name))
                .transpose()?,
        },
        "replay_capture_last" => {
            let seconds = parse_number("seconds", &fields.required("seconds")?)?;
            if !crate::REPLAY_LAST_SECONDS.contains(&seconds) {
                return Err(invalid("seconds", seconds.to_string()));
            }
            ReplayOperation::CaptureLast { seconds }
        }
        "replay_event_rename" => {
            let event = parse_number("event_id", &fields.required("event_id")?)?;
            let name = fields.required("name")?;
            check_name("name", &name)?;
            ReplayOperation::RenameEvent { event, name }
        }
        "replay_event_delete" => ReplayOperation::DeleteEvent {
            event: parse_number("event_id", &fields.required("event_id")?)?,
        },
        "replay_cue" => ReplayOperation::Cue {
            input: input(fields)?,
            event: parse_number("event_id", &fields.required("event_id")?)?,
            angle: fields
                .optional("angle")
                .map(|angle| parse_input("angle", &angle))
                .transpose()?,
        },
        "replay_angle" => ReplayOperation::Angle {
            input: input(fields)?,
            angle: parse_input("angle", &fields.required("angle")?)?,
        },
        "replay_play" => ReplayOperation::Play {
            input: input(fields)?,
            speed: parse_speed("speed", &fields.required("speed")?)?,
        },
        "replay_shuttle" => {
            let input = input(fields)?;
            let value = fields.required("rate")?;
            let rate_milli = super::source_sync::parse_canonical_i64(&value)
                .and_then(|rate| i32::try_from(rate).ok())
                .filter(|rate| (-16_000..=16_000).contains(rate))
                .ok_or_else(|| invalid("rate", value))?;
            ReplayOperation::Shuttle { input, rate_milli }
        }
        "replay_pause" => ReplayOperation::Pause {
            input: input(fields)?,
        },
        "replay_cue_in" => ReplayOperation::CueIn {
            input: input(fields)?,
        },
        "replay_step" => {
            let input = input(fields)?;
            let value = fields.required("frames")?;
            let frames = super::source_sync::parse_canonical_i64(&value)
                .and_then(|frames| i32::try_from(frames).ok())
                .filter(|frames| crate::replay_step_is_valid(*frames))
                .ok_or_else(|| invalid("frames", value))?;
            ReplayOperation::Step { input, frames }
        }
        "replay_link" => ReplayOperation::Link {
            input: input(fields)?,
            linked: match fields.required("linked")? {
                value if value.is_empty() => None,
                value => Some(parse_input("linked", &value)?),
            },
        },
        "replay_playlist_set" => ReplayOperation::PlaylistSet {
            input: input(fields)?,
            entries: decode_playlist(&fields.required("entries")?)?,
        },
        "replay_playlist_next" => ReplayOperation::PlaylistNext {
            input: input(fields)?,
        },
        "replay_playlist_prev" => ReplayOperation::PlaylistPrev {
            input: input(fields)?,
        },
        "replay_return" => ReplayOperation::Return {
            input: input(fields)?,
            to: parse_input("to", &fields.required("to")?)?,
        },
        _ => return Ok(None),
    }))
}

fn encode_recorded(sources: &[ReplaySourceStatus]) -> Result<String, CodecError> {
    if sources.len() > MAX_REPLAY_RECORD_SOURCES {
        return Err(CodecError::TooManyItems("recorded"));
    }
    Ok(sources
        .iter()
        .map(|source| {
            format!(
                "{}:{}:{}:{}:{}",
                source.input,
                source.segments,
                source.bytes,
                optional_text(source.first_frame),
                optional_text(source.last_frame)
            )
        })
        .collect::<Vec<_>>()
        .join(";"))
}

fn decode_recorded(value: &str) -> Result<Vec<ReplaySourceStatus>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<&str> = value.split(';').collect();
    if entries.len() > MAX_REPLAY_RECORD_SOURCES {
        return Err(CodecError::TooManyItems("recorded"));
    }
    entries
        .into_iter()
        .map(|entry| {
            let [input, segments, bytes, first, last] = entry.split(':').collect::<Vec<_>>()[..]
            else {
                return Err(invalid("recorded", entry));
            };
            Ok(ReplaySourceStatus {
                input: parse_input("recorded", input)?,
                segments: parse_number("recorded", segments)?,
                bytes: parse_number("recorded", bytes)?,
                first_frame: parse_optional(first, |value| parse_number("recorded", value))?,
                last_frame: parse_optional(last, |value| parse_number("recorded", value))?,
            })
        })
        .collect()
}

fn encode_events(events: &[ReplayEventStatus]) -> Result<String, CodecError> {
    if events.len() > MAX_REPLAY_EVENTS {
        return Err(CodecError::TooManyItems("events"));
    }
    events
        .iter()
        .map(|event| {
            check_name("events", &event.name)?;
            if event.angles.is_empty()
                || event.angles.len() > MAX_REPLAY_RECORD_SOURCES
                || event.in_frame >= event.out_frame
            {
                return Err(invalid("events", event.id.to_string()));
            }
            Ok(format!(
                "{}:{}:{}:{}:{}:{}",
                event.id,
                event.in_frame,
                event.out_frame,
                u8::from(event.complete),
                event
                    .angles
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("/"),
                escape_bounded(&event.name)?
            ))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|entries| entries.join(";"))
}

fn decode_events(value: &str) -> Result<Vec<ReplayEventStatus>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<&str> = value.split(';').collect();
    if entries.len() > MAX_REPLAY_EVENTS {
        return Err(CodecError::TooManyItems("events"));
    }
    entries
        .into_iter()
        .map(|entry| {
            let [id, in_frame, out_frame, complete, angles, name] =
                entry.split(':').collect::<Vec<_>>()[..]
            else {
                return Err(invalid("events", entry));
            };
            let angles = angles
                .split('/')
                .map(|angle| parse_input("events", angle))
                .collect::<Result<Vec<_>, _>>()?;
            let name = unescape(name)?;
            check_name("events", &name)?;
            let event = ReplayEventStatus {
                id: parse_number("events", id)?,
                name,
                in_frame: parse_number("events", in_frame)?,
                out_frame: parse_number("events", out_frame)?,
                angles,
                complete: parse_flag("events", complete)?,
            };
            if event.angles.len() > MAX_REPLAY_RECORD_SOURCES || event.in_frame >= event.out_frame {
                return Err(invalid("events", entry));
            }
            Ok(event)
        })
        .collect()
}

/// `storage`: `-`, or `<free>:<rate>:<remaining or ->:<level>`.
fn encode_storage(storage: Option<&crate::ReplayStorageStatus>) -> String {
    let Some(storage) = storage else {
        return "-".to_owned();
    };
    format!(
        "{}:{}:{}:{}",
        storage.free_bytes,
        storage.bytes_per_second,
        storage
            .remaining_seconds
            .map_or_else(|| "-".to_owned(), |seconds| seconds.to_string()),
        storage.level.token()
    )
}

fn decode_storage(value: &str) -> Result<Option<crate::ReplayStorageStatus>, CodecError> {
    if value == "-" {
        return Ok(None);
    }
    let [free, rate, remaining, level] = value.split(':').collect::<Vec<_>>()[..] else {
        return Err(invalid("storage", value));
    };
    let free_bytes = parse_number("storage", free)?;
    let bytes_per_second = parse_number("storage", rate)?;
    let remaining_seconds = if remaining == "-" {
        None
    } else {
        Some(parse_number("storage", remaining)?)
    };
    let level =
        crate::ReplayStorageLevel::from_token(level).ok_or_else(|| invalid("storage", value))?;
    Ok(Some(crate::ReplayStorageStatus {
        free_bytes,
        bytes_per_second,
        remaining_seconds,
        level,
    }))
}

fn encode_players(players: &[ReplayPlayerStatus]) -> Result<String, CodecError> {
    if players.len() > MAX_REPLAY_INPUTS {
        return Err(CodecError::TooManyItems("players"));
    }
    players
        .iter()
        .map(|player| {
            Ok(format!(
                "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
                player.input,
                optional_text(player.event),
                optional_text(player.angle),
                player.speed.percent(),
                u8::from(player.playing),
                optional_text(player.frame),
                optional_text(player.playlist_index),
                encode_playlist(&player.playlist)?,
                optional_text(player.linked),
                player.rate_milli,
                u8::from(player.shuttling)
            ))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|entries| entries.join(";"))
}

fn decode_players(value: &str) -> Result<Vec<ReplayPlayerStatus>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<&str> = value.split(';').collect();
    if entries.len() > MAX_REPLAY_INPUTS {
        return Err(CodecError::TooManyItems("players"));
    }
    entries
        .into_iter()
        .map(|entry| {
            let [
                input,
                event,
                angle,
                speed,
                playing,
                frame,
                index,
                playlist,
                linked,
                rate_milli,
                shuttling,
            ] = entry.split(':').collect::<Vec<_>>()[..]
            else {
                return Err(invalid("players", entry));
            };
            let playlist = decode_playlist(playlist)?;
            let index: Option<u32> = parse_optional(index, |value| parse_number("players", value))?;
            if index
                .is_some_and(|index| usize::try_from(index).map_or(true, |i| i >= playlist.len()))
            {
                return Err(invalid("players", entry));
            }
            let rate_milli = super::source_sync::parse_canonical_i64(rate_milli)
                .and_then(|rate| i32::try_from(rate).ok())
                .filter(|rate| (-16_000..=16_000).contains(rate))
                .ok_or_else(|| invalid("players", entry))?;
            Ok(ReplayPlayerStatus {
                input: parse_input("players", input)?,
                event: parse_optional(event, |value| parse_number("players", value))?,
                angle: parse_optional(angle, |value| parse_input("players", value))?,
                speed: parse_speed("players", speed)?,
                rate_milli,
                shuttling: parse_flag("players", shuttling)?,
                playing: parse_flag("players", playing)?,
                frame: parse_optional(frame, |value| parse_number("players", value))?,
                playlist,
                playlist_index: index,
                linked: parse_optional(linked, |value| parse_input("players", value))?,
            })
        })
        .collect()
}

/// Encodes `replay_status`'s fields after the server identity and
/// sequence.
pub(super) fn encode_status(
    message: &ReplayStatusMessage,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    if let Some(request_id) = &message.request_id {
        push("request_id", request_id.clone())?;
    }
    push("feed", message.feed.token().to_owned())?;
    push_settings(&message.settings, push)?;
    push("root", message.root.clone())?;
    if let Some(frame) = message.live_frame {
        push("live_frame", frame.to_string())?;
    }
    if let Some(frame) = message.mark_in {
        push("mark_in", frame.to_string())?;
    }
    if let Some(frame) = message.mark_out {
        push("mark_out", frame.to_string())?;
    }
    push("recorded", encode_recorded(&message.recorded)?)?;
    push("events", encode_events(&message.events)?)?;
    push("players", encode_players(&message.players)?)?;
    push("storage", encode_storage(message.storage.as_ref()))
}

/// Decodes `replay_status`'s fields after the server identity and
/// sequence.
pub(super) fn decode_status(
    server: crate::ServerIdentity,
    sequence: u64,
    fields: &mut dyn FieldSource,
) -> Result<ReplayStatusMessage, CodecError> {
    let request_id = fields.optional("request_id");
    let feed = fields.required("feed")?;
    let optional_frame = |fields: &mut dyn FieldSource, name: &'static str| {
        fields
            .optional(name)
            .map(|value| parse_number(name, &value))
            .transpose()
    };
    Ok(ReplayStatusMessage {
        server,
        sequence,
        request_id,
        feed: ReplayFeed::from_token(&feed).ok_or_else(|| invalid("feed", feed.clone()))?,
        settings: decode_settings(fields)?,
        root: fields.required("root")?,
        live_frame: optional_frame(fields, "live_frame")?,
        mark_in: optional_frame(fields, "mark_in")?,
        mark_out: optional_frame(fields, "mark_out")?,
        recorded: decode_recorded(&fields.required("recorded")?)?,
        events: decode_events(&fields.required("events")?)?,
        players: decode_players(&fields.required("players")?)?,
        storage: decode_storage(&fields.required("storage")?)?,
    })
}
