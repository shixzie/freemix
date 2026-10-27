//! Highlight reel codec (OR-013); see [`crate::reel`] for the contract.
//!
//! Every number is a canonical unsigned decimal. A `reel_set` or
//! `reel_changed` carries `reel`, `name`, and `clips` (and, on the event,
//! optional `total_frames` and `error`); `clips` is a comma-separated list of
//! 1 to [`crate::MAX_REEL_CLIPS`]
//! `<event>:<angle>:<trim in>:<trim out>:<speed>:<dissolve>` records, the
//! first clip's `dissolve` 0. A reel with a music bed also carries
//! `music`, `<level>:<duck>:<path>` (the path last, raw). A `reel_status`
//! carries two `;`-separated lists whose names, errors, destinations, and
//! failures are percent-escaped inside their record and escaped again with
//! the field.

use std::collections::BTreeSet;

use fm_types::replay::ReplaySpeed;

use crate::{
    CodecError, MAX_REEL_CLIPS, MAX_REEL_DISSOLVE_FRAMES, MAX_REEL_EXPORTS, MAX_REELS, ReelClip,
    ReelExportState, ReelExportStatus, ReelMusic, ReelOperation, ReelSpec, ReelStatus,
    ReelStatusMessage, ReelStatusRequest, clip_dissolve_is_valid, reel_error_is_valid,
    reel_export_path_is_valid, reel_name_is_valid,
};

use super::MAX_FIELD_VALUE_BYTES;
use super::media::{FieldSource, Push};
use super::value::{
    bounded_join, escape_bounded, parse_canonical_input, parse_canonical_u64, unescape,
};

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

fn parse_input(field: &'static str, value: &str) -> Result<crate::WireInputId, CodecError> {
    parse_canonical_input(value).ok_or_else(|| invalid(field, value))
}

fn parse_optional<T: TryFrom<u64>>(
    field: &'static str,
    value: &str,
) -> Result<Option<T>, CodecError> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_number(field, value).map(Some)
    }
}

fn optional_text(value: Option<impl ToString>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn check_error(field: &'static str, text: &str) -> Result<(), CodecError> {
    if reel_error_is_valid(text) {
        Ok(())
    } else {
        Err(invalid(field, text))
    }
}

fn check_clip(field: &'static str, clip: &ReelClip) -> Result<(), CodecError> {
    if ReplaySpeed::from_percent(clip.speed_percent).is_none() {
        return Err(invalid(field, clip.speed_percent.to_string()));
    }
    if clip.trim_in_frames > crate::MAX_REEL_TRIM_FRAMES
        || clip.trim_out_frames > crate::MAX_REEL_TRIM_FRAMES
        || clip.dissolve_frames > MAX_REEL_DISSOLVE_FRAMES
    {
        return Err(invalid(field, format!("{clip:?}")));
    }
    Ok(())
}

/// The first clip cuts (Protocol 2.111).
fn check_dissolves(clips: &[ReelClip]) -> Result<(), CodecError> {
    for (index, clip) in clips.iter().enumerate() {
        if !clip_dissolve_is_valid(index, clip.dissolve_frames) {
            return Err(invalid("clips", format!("{clip:?}")));
        }
    }
    Ok(())
}

fn check_spec(spec: &ReelSpec) -> Result<(), CodecError> {
    if !reel_name_is_valid(&spec.name) {
        return Err(invalid("name", spec.name.clone()));
    }
    if spec.clips.is_empty() {
        return Err(invalid("clips", "empty"));
    }
    if spec.clips.len() > MAX_REEL_CLIPS {
        return Err(CodecError::TooManyItems("clips"));
    }
    for clip in &spec.clips {
        check_clip("clips", clip)?;
    }
    if let Some(music) = &spec.music {
        encode_music(music)?;
    }
    check_dissolves(&spec.clips)
}

fn check_status(status: &ReelStatus) -> Result<(), CodecError> {
    if status.id != status.spec.id {
        return Err(invalid("reel", format!("{}:{}", status.id, status.spec.id)));
    }
    check_spec(&status.spec)?;
    if let Some(error) = &status.error {
        check_error("error", error)?;
    }
    Ok(())
}

fn check_export(status: &ReelExportStatus) -> Result<(), CodecError> {
    if !reel_export_path_is_valid(&status.destination) {
        return Err(invalid("destination", status.destination.clone()));
    }
    if let Some(failure) = &status.failure {
        check_error("failure", failure)?;
    }
    Ok(())
}

fn encode_music(music: &ReelMusic) -> Result<String, CodecError> {
    if !music.is_valid() {
        return Err(invalid("music", music.path.clone()));
    }
    Ok(format!(
        "{}:{}:{}",
        music.level_percent, music.duck_percent, music.path
    ))
}

fn decode_music(value: &str) -> Result<ReelMusic, CodecError> {
    // The path is last and may hold `:` of its own.
    let [level, duck, path] = value.splitn(3, ':').collect::<Vec<_>>()[..] else {
        return Err(invalid("music", value));
    };
    let music = ReelMusic {
        path: path.to_owned(),
        level_percent: parse_number("music", level)?,
        duck_percent: parse_number("music", duck)?,
    };
    if !music.is_valid() {
        return Err(invalid("music", value));
    }
    Ok(music)
}

fn decode_optional_music(fields: &mut dyn FieldSource) -> Result<Option<ReelMusic>, CodecError> {
    fields
        .optional("music")
        .map(|value| decode_music(&value))
        .transpose()
}

fn encode_clip(clip: &ReelClip) -> Result<String, CodecError> {
    check_clip("clips", clip)?;
    Ok(format!(
        "{}:{}:{}:{}:{}:{}",
        clip.event,
        clip.angle,
        clip.trim_in_frames,
        clip.trim_out_frames,
        clip.speed_percent,
        clip.dissolve_frames
    ))
}

fn encode_clips(clips: &[ReelClip]) -> Result<String, CodecError> {
    if clips.is_empty() {
        return Err(invalid("clips", "empty"));
    }
    check_dissolves(clips)?;
    bounded_join(clips, MAX_REEL_CLIPS, "clips", encode_clip)
}

fn decode_clips(value: &str) -> Result<Vec<ReelClip>, CodecError> {
    if value.is_empty() {
        return Err(invalid("clips", value));
    }
    let entries: Vec<&str> = value.split(',').collect();
    if entries.len() > MAX_REEL_CLIPS {
        return Err(CodecError::TooManyItems("clips"));
    }
    let clips = entries
        .into_iter()
        .map(|entry| {
            let [event, angle, trim_in, trim_out, speed, dissolve] =
                entry.split(':').collect::<Vec<_>>()[..]
            else {
                return Err(invalid("clips", entry));
            };
            let clip = ReelClip {
                event: parse_number("clips", event)?,
                angle: parse_input("clips", angle)?.to_domain(),
                trim_in_frames: parse_number("clips", trim_in)?,
                trim_out_frames: parse_number("clips", trim_out)?,
                speed_percent: parse_number("clips", speed)?,
                dissolve_frames: parse_number("clips", dissolve)?,
            };
            check_clip("clips", &clip)?;
            Ok(clip)
        })
        .collect::<Result<Vec<_>, _>>()?;
    check_dissolves(&clips)?;
    Ok(clips)
}

/// Encodes an operation's fields after its token (`payload`).
pub(super) fn encode_operation(
    operation: &ReelOperation,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    match operation {
        ReelOperation::Set(spec) => {
            check_spec(spec)?;
            push("reel", spec.id.to_string())?;
            push("name", spec.name.clone())?;
            push("clips", encode_clips(&spec.clips)?)?;
            if let Some(music) = &spec.music {
                push("music", encode_music(music)?)?;
            }
            Ok(())
        }
        ReelOperation::Delete { reel } => push("reel", reel.to_string()),
        ReelOperation::Export {
            job,
            reel,
            destination,
        } => {
            if !reel_export_path_is_valid(destination) {
                return Err(invalid("destination", destination.clone()));
            }
            push("job", job.to_string())?;
            push("reel", reel.to_string())?;
            push("destination", destination.clone())
        }
        ReelOperation::CancelExport { job } | ReelOperation::ForgetExport { job } => {
            push("job", job.to_string())
        }
    }
}

/// Decodes the operation named `token`, or `None` when `token` names none.
pub(super) fn decode_operation(
    token: &str,
    fields: &mut dyn FieldSource,
) -> Result<Option<ReelOperation>, CodecError> {
    Ok(Some(match token {
        "reel_set" => {
            let reel = parse_number("reel", &fields.required("reel")?)?;
            let name = fields.required("name")?;
            let clips = decode_clips(&fields.required("clips")?)?;
            let music = decode_optional_music(fields)?;
            let spec = ReelSpec {
                id: reel,
                name,
                clips,
                music,
            };
            check_spec(&spec)?;
            ReelOperation::Set(Box::new(spec))
        }
        "reel_delete" => ReelOperation::Delete {
            reel: parse_number("reel", &fields.required("reel")?)?,
        },
        "reel_export" => {
            let job = parse_number("job", &fields.required("job")?)?;
            let reel = parse_number("reel", &fields.required("reel")?)?;
            let destination = fields.required("destination")?;
            if !reel_export_path_is_valid(&destination) {
                return Err(invalid("destination", destination));
            }
            ReelOperation::Export {
                job,
                reel,
                destination,
            }
        }
        "reel_export_cancel" => ReelOperation::CancelExport {
            job: parse_number("job", &fields.required("job")?)?,
        },
        "reel_export_forget" => ReelOperation::ForgetExport {
            job: parse_number("job", &fields.required("job")?)?,
        },
        _ => return Ok(None),
    }))
}

/// Encodes one reel's fields (`reel_changed`).
pub(super) fn encode_reel(status: &ReelStatus, push: &mut Push<'_>) -> Result<(), CodecError> {
    check_status(status)?;
    push("reel", status.id.to_string())?;
    push("name", status.spec.name.clone())?;
    push("clips", encode_clips(&status.spec.clips)?)?;
    if let Some(music) = &status.spec.music {
        push("music", encode_music(music)?)?;
    }
    if let Some(frames) = status.total_frames {
        push("total_frames", frames.to_string())?;
    }
    if let Some(error) = &status.error {
        push("error", error.clone())?;
    }
    Ok(())
}

/// Decodes one reel's fields (`reel_changed`).
pub(super) fn decode_reel(fields: &mut dyn FieldSource) -> Result<ReelStatus, CodecError> {
    let id = parse_number("reel", &fields.required("reel")?)?;
    let name = fields.required("name")?;
    let clips = decode_clips(&fields.required("clips")?)?;
    let music = decode_optional_music(fields)?;
    let total_frames = fields
        .optional("total_frames")
        .map(|value| parse_number("total_frames", &value))
        .transpose()?;
    let error = fields.optional("error");
    if let Some(error) = &error {
        check_error("error", error)?;
    }
    let status = ReelStatus {
        id,
        spec: ReelSpec {
            id,
            name,
            clips,
            music,
        },
        total_frames,
        error,
    };
    check_status(&status)?;
    Ok(status)
}

fn join_records(separator: char, records: &[String]) -> Result<String, CodecError> {
    let mut output = String::new();
    for record in records {
        let additional = usize::from(!output.is_empty()) + record.len();
        if output.len() + additional > MAX_FIELD_VALUE_BYTES {
            return Err(CodecError::FieldValueTooLong);
        }
        if !output.is_empty() {
            output.push(separator);
        }
        output.push_str(record);
    }
    Ok(output)
}

fn encode_reel_record(status: &ReelStatus) -> Result<String, CodecError> {
    check_status(status)?;
    let error = match &status.error {
        Some(error) => escape_bounded(error)?,
        None => String::new(),
    };
    let music = match &status.spec.music {
        Some(music) => escape_bounded(&encode_music(music)?)?,
        None => String::new(),
    };
    Ok(format!(
        "{}:{}:{}:{}:{}:{}",
        status.id,
        escape_bounded(&status.spec.name)?,
        optional_text(status.total_frames),
        error,
        music,
        encode_clips(&status.spec.clips)?
    ))
}

fn decode_reel_record(entry: &str) -> Result<ReelStatus, CodecError> {
    // `clips` is the last field and holds raw `:` separators of its own, so
    // split only the five fields before it.
    let [id, name, total_frames, error, music, clips] =
        entry.splitn(6, ':').collect::<Vec<_>>()[..]
    else {
        return Err(invalid("reels", entry));
    };
    let id = parse_number("reels", id)?;
    let name = unescape(name)?;
    let total_frames = parse_optional("reels", total_frames)?;
    let error = if error.is_empty() {
        None
    } else {
        let error = unescape(error)?;
        check_error("error", &error)?;
        Some(error)
    };
    let status = ReelStatus {
        id,
        spec: ReelSpec {
            id,
            name,
            clips: decode_clips(clips)?,
            music: if music.is_empty() {
                None
            } else {
                Some(decode_music(&unescape(music)?)?)
            },
        },
        total_frames,
        error,
    };
    check_status(&status)?;
    Ok(status)
}

fn encode_reels(reels: &[ReelStatus]) -> Result<String, CodecError> {
    if reels.len() > MAX_REELS {
        return Err(CodecError::TooManyItems("reels"));
    }
    let mut seen = BTreeSet::new();
    let mut records = Vec::with_capacity(reels.len());
    for status in reels {
        if !seen.insert(status.id) {
            return Err(invalid("reels", status.id.to_string()));
        }
        records.push(encode_reel_record(status)?);
    }
    join_records(';', &records)
}

fn decode_reels(value: &str) -> Result<Vec<ReelStatus>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<&str> = value.split(';').collect();
    if entries.len() > MAX_REELS {
        return Err(CodecError::TooManyItems("reels"));
    }
    let mut seen = BTreeSet::new();
    entries
        .into_iter()
        .map(|entry| {
            let status = decode_reel_record(entry)?;
            if !seen.insert(status.id) {
                return Err(invalid("reels", status.id.to_string()));
            }
            Ok(status)
        })
        .collect()
}

fn encode_export_record(status: &ReelExportStatus) -> Result<String, CodecError> {
    check_export(status)?;
    let failure = match &status.failure {
        Some(failure) => escape_bounded(failure)?,
        None => String::new(),
    };
    Ok(format!(
        "{}:{}:{}:{}:{}:{}:{}:{}",
        status.job,
        status.reel,
        escape_bounded(&status.destination)?,
        status.state.token(),
        status.completed_frames,
        optional_text(status.total_frames),
        optional_text(status.bytes),
        failure
    ))
}

fn decode_export_record(entry: &str) -> Result<ReelExportStatus, CodecError> {
    let [
        job,
        reel,
        destination,
        state,
        completed,
        total,
        bytes,
        failure,
    ] = entry.split(':').collect::<Vec<_>>()[..]
    else {
        return Err(invalid("exports", entry));
    };
    let destination = unescape(destination)?;
    if !reel_export_path_is_valid(&destination) {
        return Err(invalid("destination", destination));
    }
    let state = ReelExportState::from_token(state).ok_or_else(|| invalid("exports", state))?;
    let failure = if failure.is_empty() {
        None
    } else {
        let failure = unescape(failure)?;
        check_error("failure", &failure)?;
        Some(failure)
    };
    Ok(ReelExportStatus {
        job: parse_number("exports", job)?,
        reel: parse_number("exports", reel)?,
        destination,
        state,
        completed_frames: parse_number("exports", completed)?,
        total_frames: parse_optional("exports", total)?,
        bytes: parse_optional("exports", bytes)?,
        failure,
    })
}

fn encode_exports(exports: &[ReelExportStatus]) -> Result<String, CodecError> {
    if exports.len() > MAX_REEL_EXPORTS {
        return Err(CodecError::TooManyItems("exports"));
    }
    let mut seen = BTreeSet::new();
    let mut records = Vec::with_capacity(exports.len());
    for status in exports {
        if !seen.insert(status.job) {
            return Err(invalid("exports", status.job.to_string()));
        }
        records.push(encode_export_record(status)?);
    }
    join_records(';', &records)
}

fn decode_exports(value: &str) -> Result<Vec<ReelExportStatus>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<&str> = value.split(';').collect();
    if entries.len() > MAX_REEL_EXPORTS {
        return Err(CodecError::TooManyItems("exports"));
    }
    let mut seen = BTreeSet::new();
    entries
        .into_iter()
        .map(|entry| {
            let status = decode_export_record(entry)?;
            if !seen.insert(status.job) {
                return Err(invalid("exports", status.job.to_string()));
            }
            Ok(status)
        })
        .collect()
}

/// Encodes `reel_status`'s fields after the server identity and sequence.
pub(super) fn encode_status(
    message: &ReelStatusMessage,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    if let Some(request_id) = &message.request_id {
        push("request_id", request_id.clone())?;
    }
    push("reels", encode_reels(&message.reels)?)?;
    push("exports", encode_exports(&message.exports)?)
}

/// Decodes `reel_status`'s fields after the server identity and sequence.
pub(super) fn decode_status(
    server: crate::ServerIdentity,
    sequence: u64,
    fields: &mut dyn FieldSource,
) -> Result<ReelStatusMessage, CodecError> {
    Ok(ReelStatusMessage {
        server,
        sequence,
        request_id: fields.optional("request_id"),
        reels: decode_reels(&fields.required("reels")?)?,
        exports: decode_exports(&fields.required("exports")?)?,
    })
}

/// Encodes `reel_status_request`'s fields.
pub(super) fn encode_request(
    request: &ReelStatusRequest,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("request_id", request.request_id.clone())
}

/// Decodes `reel_status_request`'s fields.
pub(super) fn decode_request(
    fields: &mut dyn FieldSource,
) -> Result<ReelStatusRequest, CodecError> {
    Ok(ReelStatusRequest {
        request_id: fields.required("request_id")?,
    })
}
