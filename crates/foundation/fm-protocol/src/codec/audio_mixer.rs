//! Mixer label, group, solo-mode, monitor output, and Fade-to-Black output
//! codec (Protocols 2.40, 2.44, and 2.53).
//!
//! * `audio_groups` is four comma-separated `<name>~<gain_millidb>~<muted
//!   0|1>~<soloed 0|1>~<parent - or 1..=4>` entries, group 1 first; no group
//!   is its own parent.
//! * `audio_solo_mode` is `sip`, `pfl`, or `afl`.
//! * `audio_monitor` (Protocol 2.53) is `<device>~<gain_millidb>~<muted
//!   0|1>`, the device percent-escaped and empty for none.
//! * `input_audio_labels` is a comma-separated list of
//!   `<input>~<length>:<label>` (Protocol 2.45), strictly ascending by input,
//!   each label non-empty (empty for none). `<length>` is the label's
//!   canonical decimal UTF-8 byte length and the label is written raw, so
//!   the line encoding escapes it exactly once and a worst-case list of
//!   [`MAX_LIST_ITEMS`] labels fits one field.
//! * `fade_to_black_excluded_outputs` is a comma-separated, strictly
//!   ascending list of output ids (empty for none).
//! * `fade_to_black_output_durations` (Protocol 2.48) is a comma-separated
//!   list of `<output>:<frames>`, strictly ascending by output, each 1
//!   through 3600 frames (empty for none); `fade_to_black_curve` is
//!   `linear` or `ease`. A command or event `duration_frames` is frames or
//!   `-` for none.
//!
//! Group names inside `audio_groups` are percent-escaped before they are
//! joined.

use core::fmt::Write as _;

use fm_types::strip_dsp::StripDsp;

use crate::{
    AUDIO_GROUP_COUNT, AudioDspTarget, AudioGroupStatus, AudioMixerOperation, AudioMixerStatus,
    AudioMonitorStatus, AudioSoloMode, CodecError, InputAudioLabel, MAX_AUDIO_LABEL_BYTES,
    WireOutputId, audio_group_is_valid, audio_label_is_valid, audio_monitor_device_is_valid,
};

use super::MAX_LIST_ITEMS;
use super::media::{FieldSource, Push};
use super::value::{escape_bounded, parse_canonical_input, parse_canonical_u64, unescape};

/// Smallest group fader, the strip gain floor.
const MIN_GROUP_GAIN_MILLIDB: i32 = -96_000;
/// Largest group fader, the strip gain ceiling.
const MAX_GROUP_GAIN_MILLIDB: i32 = 24_000;

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn parse_flag(field: &'static str, value: &str) -> Result<bool, CodecError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

fn parse_gain(field: &'static str, value: &str) -> Result<i32, CodecError> {
    value
        .parse::<i32>()
        .ok()
        .filter(|gain| {
            (MIN_GROUP_GAIN_MILLIDB..=MAX_GROUP_GAIN_MILLIDB).contains(gain)
                && gain.to_string() == value
        })
        .ok_or_else(|| invalid(field, value))
}

fn parse_group(field: &'static str, value: &str) -> Result<u8, CodecError> {
    parse_canonical_u64(value)
        .and_then(|number| u8::try_from(number).ok())
        .filter(|group| audio_group_is_valid(*group))
        .ok_or_else(|| invalid(field, value))
}

fn check_gain(field: &'static str, gain: i32) -> Result<(), CodecError> {
    if (MIN_GROUP_GAIN_MILLIDB..=MAX_GROUP_GAIN_MILLIDB).contains(&gain) {
        Ok(())
    } else {
        Err(invalid(field, gain.to_string()))
    }
}

/// Encodes a strip's group inside an `input_audio_strips` tuple.
pub(super) fn encode_strip_group(group: Option<u8>) -> Result<String, CodecError> {
    match group {
        None => Ok("-".to_owned()),
        Some(group) if audio_group_is_valid(group) => Ok(group.to_string()),
        Some(group) => Err(invalid("input_audio_strips", group.to_string())),
    }
}

/// Decodes a strip's group inside an `input_audio_strips` tuple: `None`
/// when malformed, `Some(None)` for no group.
#[allow(clippy::option_option)]
pub(super) fn decode_strip_group(value: &str) -> Option<Option<u8>> {
    if value == "-" {
        return Some(None);
    }
    parse_group("input_audio_strips", value).ok().map(Some)
}

fn parse_solo_mode(field: &'static str, value: &str) -> Result<AudioSoloMode, CodecError> {
    AudioSoloMode::from_token(value).ok_or_else(|| invalid(field, value))
}

/// Encodes a group's parent: `-` or a group other than `group` itself.
fn encode_parent(field: &'static str, group: u8, parent: Option<u8>) -> Result<String, CodecError> {
    match parent {
        None => Ok("-".to_owned()),
        Some(parent) if audio_group_is_valid(parent) && parent != group => Ok(parent.to_string()),
        Some(parent) => Err(invalid(field, parent.to_string())),
    }
}

fn decode_parent(field: &'static str, group: u8, value: &str) -> Result<Option<u8>, CodecError> {
    match decode_strip_group(value) {
        Some(parent) if parent != Some(group) => Ok(parent),
        _ => Err(invalid(field, value)),
    }
}

/// Encodes a snapshot's `audio_groups`.
pub(super) fn encode_groups(
    groups: &[AudioGroupStatus; AUDIO_GROUP_COUNT],
) -> Result<String, CodecError> {
    groups
        .iter()
        .zip(1..)
        .map(|(group, number)| {
            if !audio_label_is_valid(&group.name, false) {
                return Err(invalid("audio_groups", group.name.clone()));
            }
            check_gain("audio_groups", group.gain_millidb)?;
            Ok(format!(
                "{}~{}~{}~{}~{}",
                escape_bounded(&group.name)?,
                group.gain_millidb,
                u8::from(group.muted),
                u8::from(group.soloed),
                encode_parent("audio_groups", number, group.parent)?
            ))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|groups| groups.join(","))
}

/// Decodes a snapshot's `audio_groups`.
pub(super) fn decode_groups(
    value: &str,
) -> Result<[AudioGroupStatus; AUDIO_GROUP_COUNT], CodecError> {
    let groups = value
        .split(',')
        .zip(1..)
        .map(|(entry, number)| {
            let parts: Vec<&str> = entry.split('~').collect();
            let [name, gain, muted, soloed, parent] = parts.as_slice() else {
                return Err(invalid("audio_groups", entry));
            };
            let name = unescape(name)?;
            if !audio_label_is_valid(&name, false) {
                return Err(invalid("audio_groups", entry));
            }
            Ok(AudioGroupStatus {
                name,
                gain_millidb: parse_gain("audio_groups", gain)?,
                muted: parse_flag("audio_groups", muted)?,
                soloed: parse_flag("audio_groups", soloed)?,
                parent: decode_parent("audio_groups", number, parent)?,
            })
        })
        .take(AUDIO_GROUP_COUNT + 1)
        .collect::<Result<Vec<_>, _>>()?;
    groups
        .try_into()
        .map_err(|_| invalid("audio_groups", value))
}

/// Encodes a snapshot's `input_audio_labels`: each label raw behind its
/// byte length, so the line encoding escapes it exactly once.
pub(super) fn encode_labels(labels: &[InputAudioLabel]) -> Result<String, CodecError> {
    if labels.len() > MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems("input_audio_labels"));
    }
    let mut previous = None;
    let mut encoded = String::new();
    for entry in labels {
        if entry.label.is_empty()
            || !audio_label_is_valid(&entry.label, true)
            || previous.is_some_and(|previous| previous >= entry.input.get())
        {
            return Err(invalid("input_audio_labels", entry.label.clone()));
        }
        previous = Some(entry.input.get());
        if !encoded.is_empty() {
            encoded.push(',');
        }
        write!(
            encoded,
            "{}~{}:{}",
            entry.input,
            entry.label.len(),
            entry.label
        )
        .expect("writing to a string cannot fail");
    }
    Ok(encoded)
}

/// Decodes a snapshot's `input_audio_labels`, the exact inverse of
/// [`encode_labels`].
pub(super) fn decode_labels(value: &str) -> Result<Vec<InputAudioLabel>, CodecError> {
    let mut labels = Vec::new();
    let mut rest = value;
    let mut previous = None;
    while !rest.is_empty() {
        if !labels.is_empty() {
            rest = rest
                .strip_prefix(',')
                .ok_or_else(|| invalid("input_audio_labels", value))?;
        }
        if labels.len() == MAX_LIST_ITEMS {
            return Err(CodecError::TooManyItems("input_audio_labels"));
        }
        let (input, after) = rest
            .split_once('~')
            .ok_or_else(|| invalid("input_audio_labels", rest))?;
        let input =
            parse_canonical_input(input).ok_or_else(|| invalid("input_audio_labels", rest))?;
        let (length, after) = after
            .split_once(':')
            .ok_or_else(|| invalid("input_audio_labels", rest))?;
        let label = parse_canonical_u64(length)
            .and_then(|length| usize::try_from(length).ok())
            .filter(|length| *length <= MAX_AUDIO_LABEL_BYTES)
            .and_then(|length| after.get(..length))
            .ok_or_else(|| invalid("input_audio_labels", rest))?;
        if label.is_empty()
            || !audio_label_is_valid(label, true)
            || previous.is_some_and(|previous| previous >= input.get())
        {
            return Err(invalid("input_audio_labels", rest));
        }
        previous = Some(input.get());
        rest = &after[label.len()..];
        labels.push(InputAudioLabel {
            input,
            label: label.to_owned(),
        });
    }
    Ok(labels)
}

/// Encodes the outputs Fade to Black leaves live.
pub(super) fn encode_excluded_outputs(outputs: &[WireOutputId]) -> Result<String, CodecError> {
    if outputs.len() > MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems("fade_to_black_excluded_outputs"));
    }
    if outputs
        .windows(2)
        .any(|pair| pair[0].get() >= pair[1].get())
    {
        return Err(invalid("fade_to_black_excluded_outputs", "unordered"));
    }
    Ok(outputs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(","))
}

/// Decodes the outputs Fade to Black leaves live.
pub(super) fn decode_excluded_outputs(value: &str) -> Result<Vec<WireOutputId>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').take(MAX_LIST_ITEMS + 1).count() > MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems("fade_to_black_excluded_outputs"));
    }
    let outputs = value
        .split(',')
        .map(|output| {
            parse_canonical_input(output)
                .map(|id| WireOutputId::new(id.get()))
                .ok_or_else(|| invalid("fade_to_black_excluded_outputs", output))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if outputs
        .windows(2)
        .any(|pair| pair[0].get() >= pair[1].get())
    {
        return Err(invalid("fade_to_black_excluded_outputs", value));
    }
    Ok(outputs)
}

fn parse_canonical_u32(value: &str) -> Option<u32> {
    parse_canonical_u64(value).and_then(|value| u32::try_from(value).ok())
}

/// Encodes an optional Fade-to-Black output duration: 1 through 3600
/// frames, or `-`.
pub(super) fn encode_optional_duration(duration_frames: Option<u32>) -> Result<String, CodecError> {
    match duration_frames {
        None => Ok("-".to_owned()),
        Some(frames) if (1..=crate::MAX_FADE_TO_BLACK_DURATION_FRAMES).contains(&frames) => {
            Ok(frames.to_string())
        }
        Some(frames) => Err(invalid("duration_frames", frames.to_string())),
    }
}

/// Decodes an optional Fade-to-Black output duration, 1 through 3600
/// frames or `-`.
pub(super) fn decode_optional_duration(value: String) -> Result<Option<u32>, CodecError> {
    if value == "-" {
        return Ok(None);
    }
    parse_canonical_u32(&value)
        .filter(|frames| (1..=crate::MAX_FADE_TO_BLACK_DURATION_FRAMES).contains(frames))
        .map(Some)
        .ok_or_else(|| invalid("duration_frames", value))
}

/// Decodes a Fade-to-Black curve token.
pub(super) fn decode_curve(value: String) -> Result<crate::FadeToBlackCurve, CodecError> {
    crate::FadeToBlackCurve::from_token(&value).ok_or_else(|| invalid("curve", value))
}

/// Decodes the snapshot's `fade_to_black_curve`.
pub(super) fn decode_snapshot_curve(
    fields: &mut dyn FieldSource,
) -> Result<crate::FadeToBlackCurve, CodecError> {
    decode_curve(fields.required("fade_to_black_curve")?)
}

/// Decodes the snapshot's `fade_to_black_output_durations`.
pub(super) fn decode_snapshot_output_durations(
    fields: &mut dyn FieldSource,
) -> Result<Vec<(WireOutputId, u32)>, CodecError> {
    decode_output_durations(&fields.required("fade_to_black_output_durations")?)
}

/// Encodes the outputs with their own Fade-to-Black duration as
/// `output:frames` entries, ascending by output.
pub(super) fn encode_output_durations(
    durations: &[(WireOutputId, u32)],
) -> Result<String, CodecError> {
    if durations.len() > MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems("fade_to_black_output_durations"));
    }
    if durations
        .windows(2)
        .any(|pair| pair[0].0.get() >= pair[1].0.get())
        || durations
            .iter()
            .any(|(_, frames)| !(1..=crate::MAX_FADE_TO_BLACK_DURATION_FRAMES).contains(frames))
    {
        return Err(invalid("fade_to_black_output_durations", "unordered"));
    }
    Ok(durations
        .iter()
        .map(|(output, frames)| format!("{output}:{frames}"))
        .collect::<Vec<_>>()
        .join(","))
}

/// Decodes the outputs with their own Fade-to-Black duration.
fn decode_output_durations(value: &str) -> Result<Vec<(WireOutputId, u32)>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').take(MAX_LIST_ITEMS + 1).count() > MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems("fade_to_black_output_durations"));
    }
    let durations = value
        .split(',')
        .map(|entry| {
            let (output, frames) = entry
                .split_once(':')
                .ok_or_else(|| invalid("fade_to_black_output_durations", entry))?;
            let output = parse_canonical_input(output)
                .map(|id| WireOutputId::new(id.get()))
                .ok_or_else(|| invalid("fade_to_black_output_durations", entry))?;
            let frames = parse_canonical_u32(frames)
                .filter(|frames| (1..=crate::MAX_FADE_TO_BLACK_DURATION_FRAMES).contains(frames))
                .ok_or_else(|| invalid("fade_to_black_output_durations", entry))?;
            Ok((output, frames))
        })
        .collect::<Result<Vec<_>, CodecError>>()?;
    if durations
        .windows(2)
        .any(|pair| pair[0].0.get() >= pair[1].0.get())
    {
        return Err(invalid("fade_to_black_output_durations", value));
    }
    Ok(durations)
}

/// Checks an optional monitor device and returns its wire text (empty for
/// none).
fn encode_monitor_device<'a>(
    field: &'static str,
    device: Option<&'a str>,
) -> Result<&'a str, CodecError> {
    match device {
        None => Ok(""),
        Some(device) if audio_monitor_device_is_valid(device) => Ok(device),
        Some(device) => Err(invalid(field, device)),
    }
}

/// Decodes a monitor device's wire text: empty for none.
fn decode_monitor_device(field: &'static str, value: String) -> Result<Option<String>, CodecError> {
    if value.is_empty() {
        Ok(None)
    } else if audio_monitor_device_is_valid(&value) {
        Ok(Some(value))
    } else {
        Err(invalid(field, value))
    }
}

/// Encodes the snapshot's `audio_monitor`.
fn encode_monitor(monitor: &AudioMonitorStatus) -> Result<String, CodecError> {
    let device = encode_monitor_device("audio_monitor", monitor.device.as_deref())?;
    check_gain("audio_monitor", monitor.gain_millidb)?;
    Ok(format!(
        "{}~{}~{}",
        escape_bounded(device)?,
        monitor.gain_millidb,
        u8::from(monitor.muted)
    ))
}

/// Decodes the snapshot's `audio_monitor`.
fn decode_monitor(value: &str) -> Result<AudioMonitorStatus, CodecError> {
    let parts: Vec<&str> = value.split('~').collect();
    let [device, gain, muted] = parts.as_slice() else {
        return Err(invalid("audio_monitor", value));
    };
    Ok(AudioMonitorStatus {
        device: decode_monitor_device("audio_monitor", unescape(device)?)?,
        gain_millidb: parse_gain("audio_monitor", gain)?,
        muted: parse_flag("audio_monitor", muted)?,
    })
}

/// Encodes a DSP chain as its canonical code.
pub(super) fn encode_chain(field: &'static str, chain: &StripDsp) -> Result<String, CodecError> {
    chain
        .validate()
        .map_err(|error| invalid(field, error.to_string()))?;
    Ok(chain.code())
}

/// Decodes a canonical DSP chain code.
pub(super) fn decode_chain(field: &'static str, value: &str) -> Result<StripDsp, CodecError> {
    StripDsp::from_code(value).map_err(|_| invalid(field, value))
}

/// Encodes a DSP target: `master` or a canonical input id.
pub(super) fn encode_target(target: AudioDspTarget) -> String {
    match target {
        AudioDspTarget::Master => "master".to_owned(),
        AudioDspTarget::Input(input) => input.to_string(),
    }
}

/// Decodes a DSP target.
pub(super) fn decode_target(
    field: &'static str,
    value: &str,
) -> Result<AudioDspTarget, CodecError> {
    if value == "master" {
        return Ok(AudioDspTarget::Master);
    }
    parse_canonical_input(value)
        .map(AudioDspTarget::Input)
        .ok_or_else(|| invalid(field, value))
}

/// Encodes the snapshot mixer fields.
pub(super) fn encode_status(
    status: &AudioMixerStatus,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("audio_groups", encode_groups(&status.groups)?)?;
    push("audio_solo_mode", status.solo_mode.token().to_owned())?;
    push("audio_monitor", encode_monitor(&status.monitor)?)?;
    push("input_audio_labels", encode_labels(&status.labels)?)?;
    push(
        "audio_master_dsp",
        encode_chain("audio_master_dsp", &status.master_dsp)?,
    )?;
    if status.strip_dsp_states > crate::MAX_AUDIO_STRIP_DSP_STATES {
        return Err(invalid(
            "audio_strip_dsp_states",
            status.strip_dsp_states.to_string(),
        ));
    }
    push(
        "audio_strip_dsp_states",
        status.strip_dsp_states.to_string(),
    )
}

/// Decodes the snapshot mixer fields.
pub(super) fn decode_status(fields: &mut dyn FieldSource) -> Result<AudioMixerStatus, CodecError> {
    Ok(AudioMixerStatus {
        groups: decode_groups(&fields.required("audio_groups")?)?,
        solo_mode: parse_solo_mode("audio_solo_mode", &fields.required("audio_solo_mode")?)?,
        monitor: decode_monitor(&fields.required("audio_monitor")?)?,
        labels: decode_labels(&fields.required("input_audio_labels")?)?,
        master_dsp: decode_chain("audio_master_dsp", &fields.required("audio_master_dsp")?)?,
        strip_dsp_states: {
            let value = fields.required("audio_strip_dsp_states")?;
            parse_canonical_u64(&value)
                .and_then(|count| u16::try_from(count).ok())
                .filter(|count| *count <= crate::MAX_AUDIO_STRIP_DSP_STATES)
                .ok_or_else(|| invalid("audio_strip_dsp_states", value))?
        },
    })
}

/// Encodes `audio_meters` `dynamics`: comma-separated
/// `<target>~<gate>~<expander>~<compressor>~<limiter>` entries, each reduction a
/// canonical millidecibel count up to [`crate::MAX_DYNAMICS_REDUCTION_MILLIDB`]
/// or `-` for a stage that is off, at least one stage present, strictly
/// ascending by target (Master first).
pub(super) fn encode_dynamics(meters: &[crate::AudioDynamicsMeter]) -> Result<String, CodecError> {
    if meters.len() > MAX_LIST_ITEMS + 1 {
        return Err(CodecError::TooManyItems("dynamics"));
    }
    let stage = |value: Option<u32>| match value {
        None => Ok("-".to_owned()),
        Some(value) if value <= crate::MAX_DYNAMICS_REDUCTION_MILLIDB => Ok(value.to_string()),
        Some(value) => Err(invalid("dynamics", value.to_string())),
    };
    let mut previous: Option<AudioDspTarget> = None;
    let mut entries = Vec::with_capacity(meters.len());
    for meter in meters {
        if previous.is_some_and(|previous| previous >= meter.target)
            || (meter.gate_millidb.is_none()
                && meter.expander_millidb.is_none()
                && meter.compressor_millidb.is_none()
                && meter.limiter_millidb.is_none())
        {
            return Err(invalid("dynamics", encode_target(meter.target)));
        }
        previous = Some(meter.target);
        entries.push(format!(
            "{}~{}~{}~{}~{}",
            encode_target(meter.target),
            stage(meter.gate_millidb)?,
            stage(meter.expander_millidb)?,
            stage(meter.compressor_millidb)?,
            stage(meter.limiter_millidb)?
        ));
    }
    Ok(entries.join(","))
}

/// Decodes `audio_meters` `dynamics`, the exact inverse of
/// [`encode_dynamics`].
pub(super) fn decode_dynamics(value: &str) -> Result<Vec<crate::AudioDynamicsMeter>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').take(MAX_LIST_ITEMS + 2).count() > MAX_LIST_ITEMS + 1 {
        return Err(CodecError::TooManyItems("dynamics"));
    }
    let stage = |value: &str| -> Result<Option<u32>, CodecError> {
        if value == "-" {
            return Ok(None);
        }
        parse_canonical_u32(value)
            .filter(|value| *value <= crate::MAX_DYNAMICS_REDUCTION_MILLIDB)
            .map(Some)
            .ok_or_else(|| invalid("dynamics", value))
    };
    let mut previous: Option<AudioDspTarget> = None;
    value
        .split(',')
        .map(|entry| {
            let parts: Vec<&str> = entry.split('~').collect();
            let [target, gate, expander, compressor, limiter] = parts.as_slice() else {
                return Err(invalid("dynamics", entry));
            };
            let meter = crate::AudioDynamicsMeter {
                target: decode_target("dynamics", target)?,
                gate_millidb: stage(gate)?,
                expander_millidb: stage(expander)?,
                compressor_millidb: stage(compressor)?,
                limiter_millidb: stage(limiter)?,
            };
            if previous.is_some_and(|previous| previous >= meter.target)
                || (meter.gate_millidb.is_none()
                    && meter.expander_millidb.is_none()
                    && meter.compressor_millidb.is_none()
                    && meter.limiter_millidb.is_none())
            {
                return Err(invalid("dynamics", entry));
            }
            previous = Some(meter.target);
            Ok(meter)
        })
        .collect()
}

/// Decodes an optional knob drag: `gesture` (a canonical nonzero decimal)
/// with `continues` (`true` or `false`), or neither.
fn decode_gesture(fields: &mut dyn FieldSource) -> Result<Option<crate::GradeGesture>, CodecError> {
    let Some(token) = fields.optional("gesture") else {
        return match fields.optional("continues") {
            Some(_) => Err(CodecError::MissingField("gesture")),
            None => Ok(None),
        };
    };
    let token = parse_canonical_u64(&token)
        .and_then(core::num::NonZeroU64::new)
        .ok_or_else(|| invalid("gesture", token))?;
    let continues = match fields.required("continues")?.as_str() {
        "true" => true,
        "false" => false,
        other => return Err(invalid("continues", other)),
    };
    Ok(Some(crate::GradeGesture { token, continues }))
}

/// Encodes an operation's fields (not its payload token).
pub(super) fn encode_operation(
    operation: &AudioMixerOperation,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    match operation {
        AudioMixerOperation::SetLabel { input, label } => {
            if !audio_label_is_valid(label, true) {
                return Err(invalid("label", label.clone()));
            }
            push("input", input.to_string())?;
            push("label", label.clone())
        }
        AudioMixerOperation::SetGroup { input, group } => {
            push("input", input.to_string())?;
            push(
                "group",
                encode_strip_group(*group).map_err(|_| invalid("group", format!("{group:?}")))?,
            )
        }
        AudioMixerOperation::RenameGroup { group, name } => {
            if !audio_group_is_valid(*group) {
                return Err(invalid("group", group.to_string()));
            }
            if !audio_label_is_valid(name, false) {
                return Err(invalid("name", name.clone()));
            }
            push("group", group.to_string())?;
            push("name", name.clone())
        }
        AudioMixerOperation::SetGroupLevel {
            group,
            gain_millidb,
            muted,
        } => {
            if !audio_group_is_valid(*group) {
                return Err(invalid("group", group.to_string()));
            }
            check_gain("gain_millidb", *gain_millidb)?;
            push("group", group.to_string())?;
            push("gain_millidb", gain_millidb.to_string())?;
            push("muted", u8::from(*muted).to_string())
        }
        AudioMixerOperation::SetGroupSolo { group, soloed } => {
            if !audio_group_is_valid(*group) {
                return Err(invalid("group", group.to_string()));
            }
            push("group", group.to_string())?;
            push("soloed", u8::from(*soloed).to_string())
        }
        AudioMixerOperation::SetGroupParent { group, parent } => {
            if !audio_group_is_valid(*group) {
                return Err(invalid("group", group.to_string()));
            }
            let parent = encode_parent("parent", *group, *parent)?;
            push("group", group.to_string())?;
            push("parent", parent)
        }
        AudioMixerOperation::SetSoloMode { mode } => push("mode", mode.token().to_owned()),
        AudioMixerOperation::SetMonitorDevice { device } => push(
            "device",
            encode_monitor_device("device", device.as_deref())?.to_owned(),
        ),
        AudioMixerOperation::SetMonitorLevel {
            gain_millidb,
            muted,
        } => {
            check_gain("gain_millidb", *gain_millidb)?;
            push("gain_millidb", gain_millidb.to_string())?;
            push("muted", u8::from(*muted).to_string())
        }
        AudioMixerOperation::SetStripDsp {
            target,
            chain,
            gesture,
        } => {
            let code = encode_chain("chain", chain)?;
            push("target", encode_target(*target))?;
            push("chain", code)?;
            if let Some(gesture) = gesture {
                push("gesture", gesture.token.to_string())?;
                push("continues", gesture.continues.to_string())?;
            }
            Ok(())
        }
    }
}

/// Decodes an operation named by `payload`, or `None` for another payload.
pub(super) fn decode_operation(
    payload: &str,
    fields: &mut dyn FieldSource,
) -> Result<Option<AudioMixerOperation>, CodecError> {
    let input = |fields: &mut dyn FieldSource| {
        let value = fields.required("input")?;
        parse_canonical_input(&value).ok_or_else(|| invalid("input", value))
    };
    Ok(Some(match payload {
        "audio_strip_label" => {
            let input = input(fields)?;
            let label = fields.optional("label").unwrap_or_default();
            if !audio_label_is_valid(&label, true) {
                return Err(invalid("label", label));
            }
            AudioMixerOperation::SetLabel { input, label }
        }
        "audio_strip_group" => {
            let input = input(fields)?;
            let value = fields.required("group")?;
            AudioMixerOperation::SetGroup {
                input,
                group: decode_strip_group(&value).ok_or_else(|| invalid("group", value))?,
            }
        }
        "audio_group_name" => {
            let group = parse_group("group", &fields.required("group")?)?;
            let name = fields.required("name")?;
            if !audio_label_is_valid(&name, false) {
                return Err(invalid("name", name));
            }
            AudioMixerOperation::RenameGroup { group, name }
        }
        "audio_group_level" => AudioMixerOperation::SetGroupLevel {
            group: parse_group("group", &fields.required("group")?)?,
            gain_millidb: parse_gain("gain_millidb", &fields.required("gain_millidb")?)?,
            muted: parse_flag("muted", &fields.required("muted")?)?,
        },
        "audio_group_solo" => AudioMixerOperation::SetGroupSolo {
            group: parse_group("group", &fields.required("group")?)?,
            soloed: parse_flag("soloed", &fields.required("soloed")?)?,
        },
        "audio_group_parent" => {
            let group = parse_group("group", &fields.required("group")?)?;
            let parent = decode_parent("parent", group, &fields.required("parent")?)?;
            AudioMixerOperation::SetGroupParent { group, parent }
        }
        "audio_solo_mode" => AudioMixerOperation::SetSoloMode {
            mode: parse_solo_mode("mode", &fields.required("mode")?)?,
        },
        "audio_monitor_device" => AudioMixerOperation::SetMonitorDevice {
            device: decode_monitor_device("device", fields.required("device")?)?,
        },
        "audio_monitor_level" => AudioMixerOperation::SetMonitorLevel {
            gain_millidb: parse_gain("gain_millidb", &fields.required("gain_millidb")?)?,
            muted: parse_flag("muted", &fields.required("muted")?)?,
        },
        "audio_strip_dsp" => AudioMixerOperation::SetStripDsp {
            target: decode_target("target", &fields.required("target")?)?,
            chain: Box::new(decode_chain("chain", &fields.required("chain")?)?),
            gesture: decode_gesture(fields)?,
        },
        _ => return Ok(None),
    }))
}
