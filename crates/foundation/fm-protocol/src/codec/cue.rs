//! Cue list codec (Protocol 2.50, RC-017).
//!
//! * `cues` is a comma-separated list (empty for none) of
//!   `<id>~<name hex>~<auto follow 0|1>~<actions>`, where `<actions>` is a
//!   `/`-separated list (empty for none) of `.`-separated action tokens:
//!   `preview.<input>`, `take.<input|->.<effect>.<frames>` (`<effect>` a
//!   favourite effect token: `cut`, a mix kind, or `stinger_<slot>`),
//!   `overlay_on.<channel>.<input>`, `overlay_off.<channel>`,
//!   `title_on.<input>.<channel>`, `title_text.<input>.<field>.<text hex>`,
//!   `title_off.<input>`, `fade_to_black.<active 0|1>.<frames>`,
//!   `playlist_start`, `playlist_stop`, `audio_gain.<input>.<millidB>`,
//!   `audio_mute.<input>.<0|1>`, `record.<recorder>.<0|1>`,
//!   `stream.<target>.<0|1>`, `media_play.<input>`, `media_pause.<input>`,
//!   `media_restart.<input>`, `media_seek.<input>.<frame>`, `wait.<frames>`,
//!   `delay.<millis>` (the media and delay tokens since Protocol 2.51).
//! * `state` is `<standby>:<live>:<pending>:<played>:<fault>`, each of
//!   `standby` and `live` a cue id or `-`, `pending`
//!   `<cue>.<action>.<due frame>` or `-`, `played` `.`-separated ascending
//!   cue ids (empty for none), and `fault` `<cue>.<action>` or `-`.
//! * `fired` is `.`-separated cue ids; `titles` is a `/`-separated list of
//!   `<input>.<field revision>.<field>.<text hex>`; `media` (Protocol
//!   2.51) is a `/`-separated list of
//!   `<input>.<transport state>.<position>.<time numerator>.<time denominator>`.
//!
//! Names and texts are lowercase hex of their UTF-8 bytes, so no separator
//! can occur inside them. Every number is canonical decimal.

use core::num::NonZeroU128;

use crate::{
    CodecError, CueListSpec, CueListStatus, CueMediaTransport, CueOperation, CuePendingStatus,
    CueRunStatus, CueTitleText, MAX_CUE_ACTIONS, MAX_CUE_DELAY_MS, MAX_CUE_LIST_ACTIONS,
    MAX_CUE_LIST_TEXT_BYTES, MAX_CUE_LISTS, MAX_CUE_NAME_BYTES, MAX_CUE_TEXT_BYTES,
    MAX_CUE_TRANSITION_FRAMES, MAX_CUE_WAIT_FRAMES, MAX_CUES, MediaTime, MediaTransportState,
    TransitionFavoriteEffect, WireCue, WireCueAction, WireCueMediaOperation, WireOverlayChannelId,
    WireRecorderSlot, WireStreamTargetId, WireTitleFieldId,
};

use super::media::{FieldSource, Push};
use super::value::{parse_canonical_input, parse_canonical_u64};

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

pub(super) fn hex(value: &str) -> String {
    use core::fmt::Write as _;
    let mut out = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        write!(out, "{byte:02x}").expect("writing to a string cannot fail");
    }
    out
}

pub(super) fn unhex(field: &'static str, value: &str) -> Result<String, CodecError> {
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    if !value.len().is_multiple_of(2) {
        return Err(invalid(field, value));
    }
    let bytes = value
        .as_bytes()
        .chunks(2)
        .map(|pair| Some(digit(pair[0])? * 16 + digit(pair[1])?))
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(|| invalid(field, value))?;
    String::from_utf8(bytes).map_err(|_| invalid(field, value))
}

/// Whether `name` is a valid list or cue name.
#[must_use]
pub fn cue_name_is_valid(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_CUE_NAME_BYTES
        && !name.chars().any(char::is_control)
}

fn action_is_valid(action: &WireCueAction) -> bool {
    let transition = |frames: u32| (1..=MAX_CUE_TRANSITION_FRAMES).contains(&frames);
    match action {
        WireCueAction::Take {
            effect,
            duration_frames,
            ..
        } => match effect {
            TransitionFavoriteEffect::Cut => *duration_frames == 0,
            _ => transition(*duration_frames),
        },
        WireCueAction::TitleText { text, .. } => {
            text.len() <= MAX_CUE_TEXT_BYTES && !text.chars().any(char::is_control)
        }
        WireCueAction::FadeToBlack {
            duration_frames, ..
        } => transition(*duration_frames),
        WireCueAction::Wait { frames } => (1..=MAX_CUE_WAIT_FRAMES).contains(frames),
        WireCueAction::Delay { millis } => (1..=MAX_CUE_DELAY_MS).contains(millis),
        _ => true,
    }
}

/// Checks a list definition's structural bounds.
///
/// # Errors
///
/// Refuses an invalid name, too many cues or actions, a duplicate cue id,
/// an out-of-range action, and too much title text.
pub fn validate_spec(spec: &CueListSpec) -> Result<(), CodecError> {
    if !cue_name_is_valid(&spec.name) {
        return Err(invalid("name", spec.name.clone()));
    }
    if spec.cues.len() > MAX_CUES {
        return Err(CodecError::TooManyItems("cues"));
    }
    let mut actions = 0_usize;
    let mut text = 0_usize;
    for (index, cue) in spec.cues.iter().enumerate() {
        if spec.cues[..index].iter().any(|other| other.id == cue.id)
            || !cue_name_is_valid(&cue.name)
        {
            return Err(invalid("cues", cue.id.to_string()));
        }
        if cue.actions.len() > MAX_CUE_ACTIONS {
            return Err(CodecError::TooManyItems("cues"));
        }
        actions += cue.actions.len();
        for action in &cue.actions {
            if !action_is_valid(action) {
                return Err(invalid("cues", format!("{action:?}")));
            }
            if let WireCueAction::TitleText { text: value, .. } = action {
                text += value.len();
            }
        }
    }
    if actions > MAX_CUE_LIST_ACTIONS || text > MAX_CUE_LIST_TEXT_BYTES {
        return Err(CodecError::TooManyItems("cues"));
    }
    Ok(())
}

fn flag(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

/// The wire token of one cue action (see the module documentation).
#[must_use]
pub fn cue_action_token(action: &WireCueAction) -> String {
    match action {
        WireCueAction::Preview { input } => format!("preview.{input}"),
        WireCueAction::Take {
            input,
            effect,
            duration_frames,
        } => format!(
            "take.{}.{}.{duration_frames}",
            input.map_or_else(|| "-".to_owned(), |input| input.to_string()),
            effect.token()
        ),
        WireCueAction::OverlayOn { channel, input } => format!("overlay_on.{channel}.{input}"),
        WireCueAction::OverlayOff { channel } => format!("overlay_off.{channel}"),
        WireCueAction::TitleOn { input, channel } => format!("title_on.{input}.{channel}"),
        WireCueAction::TitleText { input, field, text } => {
            format!("title_text.{input}.{field}.{}", hex(text))
        }
        WireCueAction::TitleOff { input } => format!("title_off.{input}"),
        WireCueAction::FadeToBlack {
            active,
            duration_frames,
        } => format!("fade_to_black.{}.{duration_frames}", flag(*active)),
        WireCueAction::PlaylistStart => "playlist_start".to_owned(),
        WireCueAction::PlaylistStop => "playlist_stop".to_owned(),
        WireCueAction::AudioGain {
            input,
            gain_millidb,
        } => format!("audio_gain.{input}.{gain_millidb}"),
        WireCueAction::AudioMute { input, muted } => format!("audio_mute.{input}.{}", flag(*muted)),
        WireCueAction::Record { recorder, start } => format!("record.{recorder}.{}", flag(*start)),
        WireCueAction::Stream { target, start } => format!("stream.{target}.{}", flag(*start)),
        WireCueAction::Media { input, operation } => match operation {
            WireCueMediaOperation::Play => format!("media_play.{input}"),
            WireCueMediaOperation::Pause => format!("media_pause.{input}"),
            WireCueMediaOperation::Restart => format!("media_restart.{input}"),
            WireCueMediaOperation::Seek { frame } => format!("media_seek.{input}.{frame}"),
        },
        WireCueAction::Wait { frames } => format!("wait.{frames}"),
        WireCueAction::Delay { millis } => format!("delay.{millis}"),
    }
}

fn parse_u128(value: &str) -> Option<NonZeroU128> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || value.starts_with('0')
    {
        return None;
    }
    NonZeroU128::new(value.parse().ok()?)
}

fn parse_u32(value: &str) -> Option<u32> {
    parse_canonical_u64(value).and_then(|value| u32::try_from(value).ok())
}

fn parse_i32(value: &str) -> Option<i32> {
    let (negative, digits) = value
        .strip_prefix('-')
        .map_or((false, value), |digits| (true, digits));
    let magnitude = parse_canonical_u64(digits)?;
    if negative && magnitude == 0 {
        return None;
    }
    let magnitude = i64::try_from(magnitude).ok()?;
    i32::try_from(if negative { -magnitude } else { magnitude }).ok()
}

fn parse_flag(value: &str) -> Option<bool> {
    match value {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn parse_channel(value: &str) -> Option<WireOverlayChannelId> {
    parse_canonical_u64(value)
        .and_then(|value| u8::try_from(value).ok())
        .and_then(WireOverlayChannelId::new)
}

/// Parses one canonical cue action token; `None` when it is malformed.
/// Bounds are checked by [`validate_spec`], not here.
#[must_use]
pub fn parse_cue_action_token(value: &str) -> Option<WireCueAction> {
    let parts: Vec<&str> = value.split('.').collect();
    let input = |value: &str| parse_canonical_input(value);
    Some(match parts.as_slice() {
        ["preview", id] => WireCueAction::Preview { input: input(id)? },
        ["take", id, effect, frames] => WireCueAction::Take {
            input: if *id == "-" { None } else { Some(input(id)?) },
            effect: TransitionFavoriteEffect::from_token(effect)?,
            duration_frames: parse_u32(frames)?,
        },
        ["overlay_on", channel, id] => WireCueAction::OverlayOn {
            channel: parse_channel(channel)?,
            input: input(id)?,
        },
        ["overlay_off", channel] => WireCueAction::OverlayOff {
            channel: parse_channel(channel)?,
        },
        ["title_on", id, channel] => WireCueAction::TitleOn {
            input: input(id)?,
            channel: parse_channel(channel)?,
        },
        ["title_text", id, field, text] => WireCueAction::TitleText {
            input: input(id)?,
            field: WireTitleFieldId::new(parse_u128(field)?),
            text: unhex("cues", text).ok()?,
        },
        ["title_off", id] => WireCueAction::TitleOff { input: input(id)? },
        ["fade_to_black", active, frames] => WireCueAction::FadeToBlack {
            active: parse_flag(active)?,
            duration_frames: parse_u32(frames)?,
        },
        ["playlist_start"] => WireCueAction::PlaylistStart,
        ["playlist_stop"] => WireCueAction::PlaylistStop,
        ["audio_gain", id, gain] => WireCueAction::AudioGain {
            input: input(id)?,
            gain_millidb: parse_i32(gain)?,
        },
        ["audio_mute", id, muted] => WireCueAction::AudioMute {
            input: input(id)?,
            muted: parse_flag(muted)?,
        },
        ["record", recorder, start] => WireCueAction::Record {
            recorder: parse_canonical_u64(recorder)
                .and_then(|value| u8::try_from(value).ok())
                .and_then(WireRecorderSlot::new)?,
            start: parse_flag(start)?,
        },
        ["stream", target, start] => WireCueAction::Stream {
            target: WireStreamTargetId::new(parse_u128(target)?),
            start: parse_flag(start)?,
        },
        [
            operation @ ("media_play" | "media_pause" | "media_restart"),
            id,
        ] => WireCueAction::Media {
            input: input(id)?,
            operation: match *operation {
                "media_play" => WireCueMediaOperation::Play,
                "media_pause" => WireCueMediaOperation::Pause,
                _ => WireCueMediaOperation::Restart,
            },
        },
        ["media_seek", id, frame] => WireCueAction::Media {
            input: input(id)?,
            operation: WireCueMediaOperation::Seek {
                frame: parse_canonical_u64(frame)?,
            },
        },
        ["wait", frames] => WireCueAction::Wait {
            frames: parse_u32(frames)?,
        },
        ["delay", millis] => WireCueAction::Delay {
            millis: parse_u32(millis)?,
        },
        _ => return None,
    })
}

/// Encodes a list's cues.
fn encode_cues(cues: &[WireCue]) -> String {
    cues.iter()
        .map(|cue| {
            format!(
                "{}~{}~{}~{}",
                cue.id,
                hex(&cue.name),
                flag(cue.auto_follow),
                cue.actions
                    .iter()
                    .map(cue_action_token)
                    .collect::<Vec<_>>()
                    .join("/")
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn decode_cues(value: &str) -> Result<Vec<WireCue>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').count() > MAX_CUES {
        return Err(CodecError::TooManyItems("cues"));
    }
    value
        .split(',')
        .map(|cue| {
            let bad = || invalid("cues", cue);
            let parts: Vec<&str> = cue.split('~').collect();
            let [id, name, follow, actions] = parts.as_slice() else {
                return Err(bad());
            };
            let actions = if actions.is_empty() {
                Vec::new()
            } else {
                if actions.split('/').count() > MAX_CUE_ACTIONS {
                    return Err(CodecError::TooManyItems("cues"));
                }
                actions
                    .split('/')
                    .map(|action| parse_cue_action_token(action).ok_or_else(bad))
                    .collect::<Result<_, _>>()?
            };
            Ok(WireCue {
                id: parse_canonical_u64(id).ok_or_else(bad)?,
                name: unhex("cues", name)?,
                auto_follow: parse_flag(follow).ok_or_else(bad)?,
                actions,
            })
        })
        .collect()
}

fn optional_id(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |id| id.to_string())
}

/// `None` for a malformed id; `Some(None)` for `-`.
#[allow(clippy::option_option)] // malformed differs from absent
fn parse_optional_id(value: &str) -> Option<Option<u64>> {
    if value == "-" {
        Some(None)
    } else {
        parse_canonical_u64(value).map(Some)
    }
}

fn encode_ids(ids: &[u64]) -> String {
    ids.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

fn decode_ids(field: &'static str, value: &str) -> Result<Vec<u64>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split('.').count() > MAX_CUES {
        return Err(CodecError::TooManyItems(field));
    }
    value
        .split('.')
        .map(|id| parse_canonical_u64(id).ok_or_else(|| invalid(field, value)))
        .collect()
}

/// Encodes a run state.
#[must_use]
pub(super) fn encode_state(state: &CueRunStatus) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        optional_id(state.standby),
        optional_id(state.live),
        state.pending.map_or_else(
            || "-".to_owned(),
            |pending| format!("{}.{}.{}", pending.cue, pending.action, pending.due_frame)
        ),
        encode_ids(&state.played),
        state
            .fault
            .map_or_else(|| "-".to_owned(), |(cue, action)| format!("{cue}.{action}")),
    )
}

/// Decodes a run state.
pub(super) fn decode_state(value: &str) -> Result<CueRunStatus, CodecError> {
    let bad = || invalid("state", value);
    let parts: Vec<&str> = value.split(':').collect();
    let [standby, live, pending, played, fault] = parts.as_slice() else {
        return Err(bad());
    };
    let played = decode_ids("state", played)?;
    if !played.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(bad());
    }
    let action =
        |value: &str| parse_canonical_u64(value).and_then(|value| u16::try_from(value).ok());
    Ok(CueRunStatus {
        standby: parse_optional_id(standby).ok_or_else(bad)?,
        live: parse_optional_id(live).ok_or_else(bad)?,
        pending: match *pending {
            "-" => None,
            pending => match pending.split('.').collect::<Vec<_>>().as_slice() {
                [cue, step, due] => Some(CuePendingStatus {
                    cue: parse_canonical_u64(cue).ok_or_else(bad)?,
                    action: action(step).ok_or_else(bad)?,
                    due_frame: parse_canonical_u64(due).ok_or_else(bad)?,
                }),
                _ => return Err(bad()),
            },
        },
        played,
        fault: match *fault {
            "-" => None,
            fault => match fault.split('.').collect::<Vec<_>>().as_slice() {
                [cue, step] => Some((
                    parse_canonical_u64(cue).ok_or_else(bad)?,
                    action(step).ok_or_else(bad)?,
                )),
                _ => return Err(bad()),
            },
        },
    })
}

/// Encodes a list identity: a list id (nonzero).
fn validate_list_id(list: u64) -> Result<(), CodecError> {
    if list == 0 {
        Err(invalid("list", "0"))
    } else {
        Ok(())
    }
}

/// Decodes a nonzero `list` field.
pub(super) fn decode_list_id(fields: &mut dyn FieldSource) -> Result<u64, CodecError> {
    let value = fields.required("list")?;
    parse_canonical_u64(&value)
        .filter(|list| *list != 0)
        .ok_or_else(|| invalid("list", value))
}

/// Encodes `list`, `name`, and `cues` of a definition.
pub(super) fn encode_spec(
    list: u64,
    spec: &CueListSpec,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    validate_list_id(list)?;
    validate_spec(spec)?;
    push("list", list.to_string())?;
    push("name", spec.name.clone())?;
    push("cues", encode_cues(&spec.cues))
}

/// Decodes `name` and `cues` of a definition.
pub(super) fn decode_spec(fields: &mut dyn FieldSource) -> Result<CueListSpec, CodecError> {
    let spec = CueListSpec {
        name: fields.required("name")?,
        cues: decode_cues(&fields.required("cues")?)?,
    };
    validate_spec(&spec)?;
    Ok(spec)
}

/// Encodes a whole list (`list`, `name`, `cues`, `state`).
pub(super) fn encode_list(status: &CueListStatus, push: &mut Push<'_>) -> Result<(), CodecError> {
    encode_spec(status.id, &status.spec, push)?;
    push("state", encode_state(&status.state))
}

/// Decodes a whole list.
pub(super) fn decode_list(fields: &mut dyn FieldSource) -> Result<CueListStatus, CodecError> {
    Ok(CueListStatus {
        id: decode_list_id(fields)?,
        spec: decode_spec(fields)?,
        state: decode_state(&fields.required("state")?)?,
    })
}

/// Encodes a live operation's fields after its `payload`.
pub(super) fn encode_operation(
    list: u64,
    operation: CueOperation,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    validate_list_id(list)?;
    push("list", list.to_string())?;
    match operation {
        CueOperation::Go { cue: Some(cue) } | CueOperation::Standby { cue } => {
            push("cue", cue.to_string())
        }
        CueOperation::Go { cue: None }
        | CueOperation::Back
        | CueOperation::Skip
        | CueOperation::Stop
        | CueOperation::Reset => Ok(()),
    }
}

/// Decodes a live operation named by `payload`, or `None` for another
/// payload.
pub(super) fn decode_operation(
    payload: &str,
    fields: &mut dyn FieldSource,
) -> Result<Option<(u64, CueOperation)>, CodecError> {
    let cue = |fields: &mut dyn FieldSource| -> Result<u64, CodecError> {
        let value = fields.required("cue")?;
        parse_canonical_u64(&value).ok_or_else(|| invalid("cue", value))
    };
    let operation = match payload {
        "cue_go" => {
            let list = decode_list_id(fields)?;
            let cue = match fields.optional("cue") {
                Some(value) => {
                    Some(parse_canonical_u64(&value).ok_or_else(|| invalid("cue", value))?)
                }
                None => None,
            };
            return Ok(Some((list, CueOperation::Go { cue })));
        }
        "cue_standby" => {
            let list = decode_list_id(fields)?;
            return Ok(Some((list, CueOperation::Standby { cue: cue(fields)? })));
        }
        "cue_back" => CueOperation::Back,
        "cue_skip" => CueOperation::Skip,
        "cue_stop" => CueOperation::Stop,
        "cue_reset" => CueOperation::Reset,
        _ => return Ok(None),
    };
    Ok(Some((decode_list_id(fields)?, operation)))
}

/// Encodes fired cue ids.
#[must_use]
pub(super) fn encode_fired(fired: &[u64]) -> String {
    encode_ids(fired)
}

/// Decodes fired cue ids.
pub(super) fn decode_fired(value: &str) -> Result<Vec<u64>, CodecError> {
    decode_ids("fired", value)
}

/// Encodes title texts.
pub(super) fn encode_titles(titles: &[CueTitleText]) -> Result<String, CodecError> {
    if titles.len() > MAX_CUE_LIST_ACTIONS {
        return Err(CodecError::TooManyItems("titles"));
    }
    Ok(titles
        .iter()
        .map(|title| {
            format!(
                "{}.{}.{}.{}",
                title.input,
                title.field_revision,
                title.field,
                hex(&title.text)
            )
        })
        .collect::<Vec<_>>()
        .join("/"))
}

/// Decodes title texts.
pub(super) fn decode_titles(value: &str) -> Result<Vec<CueTitleText>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split('/').count() > MAX_CUE_LIST_ACTIONS {
        return Err(CodecError::TooManyItems("titles"));
    }
    value
        .split('/')
        .map(|title| {
            let bad = || invalid("titles", title);
            match title.split('.').collect::<Vec<_>>().as_slice() {
                [input, revision, field, text] => Ok(CueTitleText {
                    input: parse_canonical_input(input).ok_or_else(bad)?,
                    field_revision: parse_canonical_u64(revision).ok_or_else(bad)?,
                    field: WireTitleFieldId::new(parse_u128(field).ok_or_else(bad)?),
                    text: unhex("titles", text)?,
                }),
                _ => Err(bad()),
            }
        })
        .collect()
}

/// Encodes the media transports of a run (Protocol 2.51).
pub(super) fn encode_media(media: &[CueMediaTransport]) -> Result<String, CodecError> {
    if media.len() > MAX_CUE_LIST_ACTIONS {
        return Err(CodecError::TooManyItems("media"));
    }
    media
        .iter()
        .map(|transport| {
            let time = transport.position_time;
            if time.denominator == 0 {
                return Err(invalid("media", transport.input.to_string()));
            }
            Ok(format!(
                "{}.{}.{}.{}.{}",
                transport.input,
                transport.state.token(),
                transport.position,
                time.numerator,
                time.denominator
            ))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|items| items.join("/"))
}

/// Decodes the media transports of a run.
pub(super) fn decode_media(value: &str) -> Result<Vec<CueMediaTransport>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split('/').count() > MAX_CUE_LIST_ACTIONS {
        return Err(CodecError::TooManyItems("media"));
    }
    value
        .split('/')
        .map(|item| {
            let bad = || invalid("media", item);
            match item.split('.').collect::<Vec<_>>().as_slice() {
                [input, state, position, numerator, denominator] => Ok(CueMediaTransport {
                    input: parse_canonical_input(input).ok_or_else(bad)?,
                    state: MediaTransportState::from_token(state).ok_or_else(bad)?,
                    position: parse_canonical_u64(position).ok_or_else(bad)?,
                    position_time: MediaTime {
                        numerator: if *numerator == "0" {
                            0
                        } else {
                            parse_u128(numerator).ok_or_else(bad)?.get()
                        },
                        denominator: parse_u128(denominator).ok_or_else(bad)?.get(),
                    },
                }),
                _ => Err(bad()),
            }
        })
        .collect()
}

/// Validates a snapshot's `cue_list_states` count.
pub(super) fn parse_list_count(value: &str) -> Result<u16, CodecError> {
    parse_canonical_u64(value)
        .and_then(|count| u16::try_from(count).ok())
        .filter(|count| usize::from(*count) <= MAX_CUE_LISTS)
        .ok_or_else(|| invalid("cue_list_states", value))
}

/// Whether `count` is a valid `cue_list_states` count.
pub(super) fn list_count_is_valid(count: u16) -> bool {
    usize::from(count) <= MAX_CUE_LISTS
}
