//! Cue list codec (Protocol 2.50, RC-017).
//!
//! * `cues` is a comma-separated list (empty for none) of
//!   `<id>~<name hex>~<auto follow 0|1>~<actions>`, where `<actions>` is a
//!   `/`-separated list (empty for none) of `.`-separated action tokens:
//!   `preview.<input>`, `take.<input|->.<effect>.<frames>` (`<effect>` a
//!   favourite effect token: `cut`, a mix kind, or `stinger_<slot>`),
//!   `overlay_on.<channel>.<input>`, `overlay_off.<channel>`,
//!   `title_on.<input>.<channel>`, `title_text.<input>.<field>.<text hex>`,
//!   `title_number.<input>.<field>.<set|add>.<value>` (Protocol 2.65, a
//!   signed decimal),
//!   `title_off.<input>`, `fade_to_black.<active 0|1>.<frames>`,
//!   `playlist_start`, `playlist_stop`, `audio_gain.<input>.<millidB>`,
//!   `audio_mute.<input>.<0|1>`, and since Protocol 2.99 (RC-004 dynamic
//!   values) `audio_gain_step.<input>.<millidB>` (a nonzero step from the
//!   strip's gain), `audio_gain_fired.<input>.<min millidB>.<max millidB>`
//!   (the firing control's value mapped from `min` to `max`, only in a
//!   shortcut), and `audio_mute_toggle.<input>`, `record.<recorder>.<0|1>`,
//!   `stream.<target>.<0|1>`, `media_play.<input>`, `media_pause.<input>`,
//!   `media_restart.<input>`, `media_seek.<input>.<frame>`, `wait.<frames>`,
//!   `delay.<millis>` (the media and delay tokens since Protocol 2.51),
//!   `run_script.<script>` (Protocol 2.60, a nonzero project script id).
//!   Since Protocol 2.88 (RC-004) every `<input>` of an action is an input
//!   id or a role resolved when the action is fired: `preview`, `program`,
//!   or `selected` (see [`crate::WireInputTarget`]). Since Protocol 2.104
//!   (GX-006) `title_text.<input>.<field>.hidden` and
//!   `title_number.<input>.<field>.hidden` stand for a title action whose
//!   value is hidden from the session ([`WireCueAction::TitleHidden`]): the
//!   marker is exactly [`crate::HIDDEN_TITLE_VALUE`], which is neither
//!   lowercase hex nor a `set`/`add` mode.
//! * `cue_go` and `cue_arm` (and `shortcut_fire`) carry the optional
//!   target bindings `selected`, `bound_preview`, and `bound_program`
//!   (Protocol 2.88, see [`crate::WireTargetBindings`]).
//! * `state` is `<standby>:<live>:<pending>:<played>:<fault>:<armed>`, each of
//!   `standby` and `live` a cue id or `-`, `pending`
//!   `<cue>.<action>.<due frame>` or `-`, `played` `.`-separated ascending
//!   cue ids (empty for none), and `fault` `<cue>.<action>.<reason>.<message>`
//!   or `-`: since Protocol 2.63 a fault says what its action was refused
//!   with, `reason` the lowercase hex of the refusal's `reason` token (`-`
//!   for none) and `message` the lowercase hex of its English text (see
//!   [`fm_command::FaultDetail`]); `armed` (Protocol 2.64) is
//!   `<cue>.<due frame>` or `-`. Since Protocol 2.89 (RC-004) `armed` is
//!   `<cue>.<due frame>.<targets>` when the armed GO bound targets, and the
//!   state gains a seventh part, the run's bound `targets` or `-`; `targets`
//!   is `<preview>.<program>.<selected>`, each a canonical input id or `-`.
//! * `cue_arm` carries `list`, an optional `cue`, and `in_ms` (canonical
//!   decimal, `1..=86400000`); `cue_disarm` carries `list` (Protocol 2.64).
//! * `fired` is `.`-separated cue ids; `titles` is a `/`-separated list of
//!   `<input>.<field revision>.<field>.<value>`, `value` the text's hex or,
//!   for a number field, `n<signed decimal>` (Protocol 2.65); `media` (Protocol
//!   2.51) is a `/`-separated list of
//!   `<input>.<transport state>.<position>.<time numerator>.<time denominator>`;
//!   `scripts` (Protocol 2.61) is a `/`-separated list (empty for none) of
//!   at most 8 `<script>.<actions>.<instructions>`, `script` nonzero.
//!
//! Names and texts are lowercase hex of their UTF-8 bytes, so no separator
//! can occur inside them. Every number is canonical decimal.

use core::num::NonZeroU128;

use crate::{
    CodecError, CueArmedStatus, CueFaultStatus, CueListSpec, CueListStatus, CueMediaTransport,
    CueOperation, CuePendingStatus, CueRunStatus, CueScriptRun, CueTitleText, CueTitleValue,
    HIDDEN_TITLE_VALUE, MAX_CUE_ACTIONS, MAX_CUE_ARM_MS, MAX_CUE_DELAY_MS, MAX_CUE_LIST_ACTIONS,
    MAX_CUE_LIST_SCRIPT_STEPS, MAX_CUE_LIST_TEXT_BYTES, MAX_CUE_LISTS, MAX_CUE_NAME_BYTES,
    MAX_CUE_TEXT_BYTES, MAX_CUE_TITLE_NUMBER, MAX_CUE_TRANSITION_FRAMES, MAX_CUE_WAIT_FRAMES,
    MAX_CUES, MediaTime, MediaTransportState, TransitionFavoriteEffect, WireCue, WireCueAction,
    WireCueMediaOperation, WireInputTarget, WireOverlayChannelId, WireRecorderSlot,
    WireStreamTargetId, WireTargetBindings, WireTitleFieldId,
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

pub(super) fn action_is_valid(action: &WireCueAction) -> bool {
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
        WireCueAction::TitleNumber { add, value, .. } => {
            (!*add || *value != 0) && value.unsigned_abs() <= MAX_CUE_TITLE_NUMBER.unsigned_abs()
        }
        WireCueAction::FadeToBlack {
            duration_frames, ..
        } => transition(*duration_frames),
        WireCueAction::Wait { frames } => (1..=MAX_CUE_WAIT_FRAMES).contains(frames),
        WireCueAction::Delay { millis } => (1..=MAX_CUE_DELAY_MS).contains(millis),
        WireCueAction::RunScript { script } => *script != 0,
        WireCueAction::AudioGain { gain, .. } => gain.is_valid(),
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
    let mut scripts = 0_usize;
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
            // Only a shortcut fire carries a control value (Protocol 2.99).
            if !action_is_valid(action)
                || matches!(
                    action,
                    WireCueAction::AudioGain {
                        gain: fm_types::CueGain::Fired { .. },
                        ..
                    }
                )
            {
                return Err(invalid("cues", format!("{action:?}")));
            }
            if let WireCueAction::TitleText { text: value, .. } = action {
                text += value.len();
            }
            if matches!(action, WireCueAction::RunScript { .. }) {
                scripts += 1;
            }
        }
    }
    if actions > MAX_CUE_LIST_ACTIONS
        || text > MAX_CUE_LIST_TEXT_BYTES
        || scripts > MAX_CUE_LIST_SCRIPT_STEPS
    {
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
        WireCueAction::TitleNumber {
            input,
            field,
            add,
            value,
        } => format!(
            "title_number.{input}.{field}.{}.{value}",
            if *add { "add" } else { "set" }
        ),
        WireCueAction::TitleHidden {
            input,
            field,
            number,
        } => format!(
            "{}.{input}.{field}.{HIDDEN_TITLE_VALUE}",
            if *number {
                "title_number"
            } else {
                "title_text"
            }
        ),
        WireCueAction::TitleOff { input } => format!("title_off.{input}"),
        WireCueAction::FadeToBlack {
            active,
            duration_frames,
        } => format!("fade_to_black.{}.{duration_frames}", flag(*active)),
        WireCueAction::PlaylistStart => "playlist_start".to_owned(),
        WireCueAction::PlaylistStop => "playlist_stop".to_owned(),
        WireCueAction::AudioGain { input, gain } => match gain {
            fm_types::CueGain::Set(gain) => format!("audio_gain.{input}.{gain}"),
            fm_types::CueGain::Step(step) => format!("audio_gain_step.{input}.{step}"),
            fm_types::CueGain::Fired { min, max } => {
                format!("audio_gain_fired.{input}.{min}.{max}")
            }
        },
        WireCueAction::AudioMute { input, mute } => match mute {
            fm_types::CueMute::Set(muted) => format!("audio_mute.{input}.{}", flag(*muted)),
            fm_types::CueMute::Toggle => format!("audio_mute_toggle.{input}"),
        },
        WireCueAction::Record { recorder, start } => format!("record.{recorder}.{}", flag(*start)),
        WireCueAction::Stream { target, start } => format!("stream.{target}.{}", flag(*start)),
        WireCueAction::Media { input, operation } => match operation {
            WireCueMediaOperation::Play => format!("media_play.{input}"),
            WireCueMediaOperation::Pause => format!("media_pause.{input}"),
            WireCueMediaOperation::Restart => format!("media_restart.{input}"),
            WireCueMediaOperation::Seek { frame } => format!("media_seek.{input}.{frame}"),
        },
        WireCueAction::Page { input, navigation } => match navigation {
            crate::PagedNavigation::Goto(page) => format!("page.{input}.goto.{page}"),
            other => format!("page.{input}.{}", other.token()),
        },
        WireCueAction::Wait { frames } => format!("wait.{frames}"),
        WireCueAction::Delay { millis } => format!("delay.{millis}"),
        WireCueAction::RunScript { script } => format!("run_script.{script}"),
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
    i32::try_from(parse_i64(value)?).ok()
}

/// A canonical signed decimal: no `+`, no leading zeros, no `-0`.
fn parse_i64(value: &str) -> Option<i64> {
    let (negative, digits) = value
        .strip_prefix('-')
        .map_or((false, value), |digits| (true, digits));
    let magnitude = parse_canonical_u64(digits)?;
    if negative && magnitude == 0 {
        return None;
    }
    let magnitude = i64::try_from(magnitude).ok()?;
    Some(if negative { -magnitude } else { magnitude })
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

/// Parses an audio action token's `kind`, input `id`, and the parts after
/// them; `None` when malformed.
fn parse_audio_token(kind: &str, id: &str, rest: &[&str]) -> Option<WireCueAction> {
    let input = parse_target(id)?;
    let gain = |gain| Some(WireCueAction::AudioGain { input, gain });
    let mute = |mute| Some(WireCueAction::AudioMute { input, mute });
    match (kind, rest) {
        ("audio_gain", [gain_millidb]) => gain(fm_types::CueGain::Set(parse_i32(gain_millidb)?)),
        // A step and a gain from the firing control (Protocol 2.99).
        ("audio_gain_step", [step]) => gain(fm_types::CueGain::Step(parse_i32(step)?)),
        ("audio_gain_fired", [min, max]) => gain(fm_types::CueGain::Fired {
            min: parse_i32(min)?,
            max: parse_i32(max)?,
        }),
        ("audio_mute", [muted]) => mute(fm_types::CueMute::Set(parse_flag(muted)?)),
        ("audio_mute_toggle", []) => mute(fm_types::CueMute::Toggle),
        _ => None,
    }
}

/// Parses one canonical cue action token; `None` when it is malformed.
/// Bounds are checked by [`validate_spec`], not here.
#[must_use]
pub fn parse_cue_action_token(value: &str) -> Option<WireCueAction> {
    let parts: Vec<&str> = value.split('.').collect();
    let input = parse_target;
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
        [
            kind @ ("title_text" | "title_number"),
            id,
            field,
            HIDDEN_TITLE_VALUE,
        ] => WireCueAction::TitleHidden {
            input: input(id)?,
            field: WireTitleFieldId::new(parse_u128(field)?),
            number: *kind == "title_number",
        },
        ["title_text", id, field, text] => WireCueAction::TitleText {
            input: input(id)?,
            field: WireTitleFieldId::new(parse_u128(field)?),
            text: unhex("cues", text).ok()?,
        },
        ["title_number", id, field, update, value] => WireCueAction::TitleNumber {
            input: input(id)?,
            field: WireTitleFieldId::new(parse_u128(field)?),
            add: match *update {
                "set" => false,
                "add" => true,
                _ => return None,
            },
            value: parse_i64(value)?,
        },
        ["title_off", id] => WireCueAction::TitleOff { input: input(id)? },
        ["fade_to_black", active, frames] => WireCueAction::FadeToBlack {
            active: parse_flag(active)?,
            duration_frames: parse_u32(frames)?,
        },
        ["playlist_start"] => WireCueAction::PlaylistStart,
        ["playlist_stop"] => WireCueAction::PlaylistStop,
        [kind, id, rest @ ..] if kind.starts_with("audio_") => parse_audio_token(kind, id, rest)?,
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
        ["page", id, "goto", page] => WireCueAction::Page {
            input: input(id)?,
            navigation: crate::PagedNavigation::from_token("goto", Some(parse_u32(page)?))?,
        },
        ["page", id, action] if *action != "goto" => WireCueAction::Page {
            input: input(id)?,
            navigation: crate::PagedNavigation::from_token(action, None)?,
        },
        ["wait", frames] => WireCueAction::Wait {
            frames: parse_u32(frames)?,
        },
        ["delay", millis] => WireCueAction::Delay {
            millis: parse_u32(millis)?,
        },
        ["run_script", script] => WireCueAction::RunScript {
            script: parse_canonical_u64(script)?,
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
///
/// # Errors
///
/// Refuses a fault whose detail is invalid.
pub(super) fn encode_state(state: &CueRunStatus) -> Result<String, CodecError> {
    let fault = match &state.fault {
        None => "-".to_owned(),
        Some(fault) => format!(
            "{}.{}.{}",
            fault.cue,
            fault.action,
            super::refusal::encode_fault_detail("state", &fault.detail, '.')?
        ),
    };
    Ok(format!(
        "{}:{}:{}:{}:{}:{}:{}",
        optional_id(state.standby),
        optional_id(state.live),
        state.pending.map_or_else(
            || "-".to_owned(),
            |pending| format!("{}.{}.{}", pending.cue, pending.action, pending.due_frame)
        ),
        encode_ids(&state.played),
        fault,
        state.armed.map_or_else(
            || "-".to_owned(),
            |armed| match armed.targets {
                None => format!("{}.{}", armed.cue, armed.due_frame),
                Some(targets) => format!(
                    "{}.{}.{}",
                    armed.cue,
                    armed.due_frame,
                    encode_bound(&targets)
                ),
            }
        ),
        state
            .targets
            .map_or_else(|| "-".to_owned(), |targets| encode_bound(&targets)),
    ))
}

/// Encodes a run's bound targets as `<preview>.<program>.<selected>`, each
/// a canonical input id or `-` (Protocol 2.89).
fn encode_bound(targets: &WireTargetBindings) -> String {
    let id = |input: Option<crate::WireInputId>| {
        input.map_or_else(|| "-".to_owned(), |input| input.to_string())
    };
    format!(
        "{}.{}.{}",
        id(targets.preview),
        id(targets.program),
        id(targets.selected)
    )
}

/// Decodes the three parts of a run's bound targets (see [`encode_bound`]).
fn decode_bound(parts: &[&str]) -> Option<WireTargetBindings> {
    let id = |value: &str| match value {
        "-" => Some(None),
        value => parse_canonical_input(value).map(Some),
    };
    let [preview, program, selected] = parts else {
        return None;
    };
    Some(WireTargetBindings {
        preview: id(preview)?,
        program: id(program)?,
        selected: id(selected)?,
    })
}

/// Decodes a run state.
pub(super) fn decode_state(value: &str) -> Result<CueRunStatus, CodecError> {
    let bad = || invalid("state", value);
    let parts: Vec<&str> = value.split(':').collect();
    let [standby, live, pending, played, fault, armed, targets] = parts.as_slice() else {
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
                [cue, step, reason, message] => Some(CueFaultStatus {
                    cue: parse_canonical_u64(cue).ok_or_else(bad)?,
                    action: action(step).ok_or_else(bad)?,
                    detail: super::refusal::decode_fault_detail("state", reason, message)?,
                }),
                _ => return Err(bad()),
            },
        },
        armed: match *armed {
            "-" => None,
            armed => match armed.split('.').collect::<Vec<_>>().as_slice() {
                [cue, due, bound @ ..] => Some(CueArmedStatus {
                    cue: parse_canonical_u64(cue).ok_or_else(bad)?,
                    due_frame: parse_canonical_u64(due).ok_or_else(bad)?,
                    targets: if bound.is_empty() {
                        None
                    } else {
                        Some(decode_bound(bound).ok_or_else(bad)?)
                    },
                }),
                _ => return Err(bad()),
            },
        },
        targets: match *targets {
            "-" => None,
            targets => Some(decode_bound(&targets.split('.').collect::<Vec<_>>()).ok_or_else(bad)?),
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
    push("state", encode_state(&status.state)?)
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
        CueOperation::Go { cue, targets } => {
            if let Some(cue) = cue {
                push("cue", cue.to_string())?;
            }
            encode_targets(&targets, push)
        }
        CueOperation::Standby { cue } => push("cue", cue.to_string()),
        CueOperation::Arm {
            cue,
            in_ms,
            targets,
        } => {
            if !(1..=MAX_CUE_ARM_MS).contains(&in_ms) {
                return Err(invalid("in_ms", in_ms.to_string()));
            }
            if let Some(cue) = cue {
                push("cue", cue.to_string())?;
            }
            push("in_ms", in_ms.to_string())?;
            encode_targets(&targets, push)
        }
        CueOperation::Back
        | CueOperation::Skip
        | CueOperation::Stop
        | CueOperation::Reset
        | CueOperation::Disarm => Ok(()),
    }
}

/// The optional command fields of a fire's target bindings (Protocol 2.88,
/// RC-004), each a canonical input id.
const TARGET_FIELDS: [&str; 3] = ["selected", "bound_preview", "bound_program"];

/// Encodes a fire's target bindings as the optional fields `selected`,
/// `bound_preview`, and `bound_program`, each written only when bound.
pub(super) fn encode_targets(
    targets: &WireTargetBindings,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    for (name, input) in
        TARGET_FIELDS
            .into_iter()
            .zip([targets.selected, targets.preview, targets.program])
    {
        if let Some(input) = input {
            push(name, input.to_string())?;
        }
    }
    Ok(())
}

/// Decodes a fire's target bindings (see [`encode_targets`]).
pub(super) fn decode_targets(
    fields: &mut dyn FieldSource,
) -> Result<WireTargetBindings, CodecError> {
    let mut input = |name: &'static str| match fields.optional(name) {
        None => Ok(None),
        Some(value) => parse_canonical_input(&value)
            .map(Some)
            .ok_or_else(|| invalid(name, value)),
    };
    Ok(WireTargetBindings {
        selected: input(TARGET_FIELDS[0])?,
        preview: input(TARGET_FIELDS[1])?,
        program: input(TARGET_FIELDS[2])?,
    })
}

/// Parses an action's target: a canonical input id or a role token.
fn parse_target(value: &str) -> Option<WireInputTarget> {
    WireInputTarget::from_role_token(value)
        .or_else(|| parse_canonical_input(value).map(WireInputTarget::Fixed))
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
            let targets = decode_targets(fields)?;
            return Ok(Some((list, CueOperation::Go { cue, targets })));
        }
        "cue_arm" => {
            let list = decode_list_id(fields)?;
            let cue = match fields.optional("cue") {
                Some(value) => {
                    Some(parse_canonical_u64(&value).ok_or_else(|| invalid("cue", value))?)
                }
                None => None,
            };
            let value = fields.required("in_ms")?;
            let in_ms = parse_canonical_u64(&value)
                .and_then(|millis| u32::try_from(millis).ok())
                .filter(|millis| (1..=MAX_CUE_ARM_MS).contains(millis))
                .ok_or_else(|| invalid("in_ms", value))?;
            let targets = decode_targets(fields)?;
            return Ok(Some((
                list,
                CueOperation::Arm {
                    cue,
                    in_ms,
                    targets,
                },
            )));
        }
        "cue_disarm" => CueOperation::Disarm,
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
            let value = match &title.value {
                CueTitleValue::Text(text) => hex(text),
                CueTitleValue::Number(number) => format!("n{number}"),
            };
            format!(
                "{}.{}.{}.{value}",
                title.input, title.field_revision, title.field
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
                    value: match text.strip_prefix('n') {
                        Some(number) => CueTitleValue::Number(parse_i64(number).ok_or_else(bad)?),
                        None => CueTitleValue::Text(unhex("titles", text)?),
                    },
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

/// Encodes the script steps of a run (Protocol 2.61).
pub(super) fn encode_scripts(scripts: &[CueScriptRun]) -> Result<String, CodecError> {
    if scripts.len() > MAX_CUE_LIST_SCRIPT_STEPS {
        return Err(CodecError::TooManyItems("scripts"));
    }
    scripts
        .iter()
        .map(|run| {
            if run.script == 0 {
                return Err(invalid("scripts", "0"));
            }
            Ok(format!(
                "{}.{}.{}",
                run.script, run.actions, run.instructions
            ))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|items| items.join("/"))
}

/// Decodes the script steps of a run.
pub(super) fn decode_scripts(value: &str) -> Result<Vec<CueScriptRun>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split('/').count() > MAX_CUE_LIST_SCRIPT_STEPS {
        return Err(CodecError::TooManyItems("scripts"));
    }
    value
        .split('/')
        .map(|item| {
            let bad = || invalid("scripts", item);
            match item.split('.').collect::<Vec<_>>().as_slice() {
                [script, actions, instructions] => Ok(CueScriptRun {
                    script: parse_canonical_u64(script)
                        .filter(|script| *script != 0)
                        .ok_or_else(bad)?,
                    actions: parse_u32(actions).ok_or_else(bad)?,
                    instructions: parse_u32(instructions).ok_or_else(bad)?,
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
