//! The playback settings of a media input in `project.json` (schema 36,
//! IN-003).
//!
//! ```text
//! {"type": "media", "asset_uri": "asset://media/clip.mov", "playback": {
//!   "trim_in": 12, "trim_out": 480 | null, "end_action": "stop|hold|loop",
//!   "end_trigger_lead": 30 | null, "speed_basis_points": 10000,
//!   "audio_policy": "mute|resample", "audio_stream": 2 | null,
//!   "growing": {"live_edge_offset_frames": 2, "idle_completion_refreshes": 0} | null,
//!   "start_frame": 12}}
//! ```
//!
//! Every field is required; bounds are those of
//! [`fm_model::MediaPlaybackSettings::validate`].

use core::fmt::Write as _;

use fm_model::{MediaAudioSpeedPolicy, MediaEndAction, MediaGrowing, MediaPlaybackSettings};

use super::{DecodeError, Object, Value, parse_optional, syntax, unknown_enum};

/// Writes `{"type": "media", "asset_uri": ..., "playback": {...}}`.
pub(crate) fn write_media_input(
    output: &mut String,
    asset_uri: &str,
    playback: &MediaPlaybackSettings,
) {
    output.push_str("{\"type\": \"media\", \"asset_uri\": \"");
    super::escape_string(output, asset_uri);
    output.push_str("\", \"playback\": ");
    write_media_playback(output, playback);
    output.push('}');
}

fn optional<T: core::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| value.to_string())
}

fn write_media_playback(output: &mut String, playback: &MediaPlaybackSettings) {
    write!(
        output,
        "{{\"trim_in\": {}, \"trim_out\": {}, \"end_action\": \"{}\", \"end_trigger_lead\": {}, \
         \"speed_basis_points\": {}, \"audio_policy\": \"{}\", \"audio_stream\": {}, \
         \"growing\": ",
        playback.trim_in,
        optional(playback.trim_out),
        playback.end_action.token(),
        optional(playback.end_trigger_lead),
        playback.speed_basis_points,
        playback.audio_policy.token(),
        optional(playback.audio_stream),
    )
    .expect("writing to a string cannot fail");
    match playback.growing {
        Some(growing) => write!(
            output,
            "{{\"live_edge_offset_frames\": {}, \"idle_completion_refreshes\": {}}}",
            growing.live_edge_offset_frames, growing.idle_completion_refreshes
        )
        .expect("writing to a string cannot fail"),
        None => output.push_str("null"),
    }
    write!(output, ", \"start_frame\": {}}}", playback.start_frame)
        .expect("writing to a string cannot fail");
}

fn number(value: &Value, field: &str) -> Result<u128, DecodeError> {
    let Value::Number(value) = *value else {
        return Err(syntax(format!(
            "field `{field}` must be an unsigned integer or null"
        )));
    };
    Ok(value)
}

fn optional_u64(object: &mut Object, field: &str) -> Result<Option<u64>, DecodeError> {
    parse_optional(object.take(field)?, |value| {
        u64::try_from(number(&value, field)?)
            .map_err(|_| syntax(format!("field `{field}` exceeds u64")))
    })
}

fn optional_u32(object: &mut Object, field: &str) -> Result<Option<u32>, DecodeError> {
    parse_optional(object.take(field)?, |value| {
        u32::try_from(number(&value, field)?)
            .map_err(|_| syntax(format!("field `{field}` exceeds u32")))
    })
}

/// Parses a media input's `"playback"` object.
pub(crate) fn parse_media_playback(value: Value) -> Result<MediaPlaybackSettings, DecodeError> {
    let mut object = Object::new(value, "media playback")?;
    let trim_in = object.u64("trim_in")?;
    let trim_out = optional_u64(&mut object, "trim_out")?;
    let end_action = {
        let token = object.string("end_action")?;
        MediaEndAction::from_token(&token)
            .ok_or_else(|| unknown_enum("media end action", &token))?
    };
    let end_trigger_lead = optional_u32(&mut object, "end_trigger_lead")?;
    let speed_basis_points = object.u32("speed_basis_points")?;
    let audio_policy = {
        let token = object.string("audio_policy")?;
        MediaAudioSpeedPolicy::from_token(&token)
            .ok_or_else(|| unknown_enum("media audio policy", &token))?
    };
    let audio_stream = optional_u32(&mut object, "audio_stream")?;
    let growing = parse_optional(object.take("growing")?, |value| {
        let mut growing = Object::new(value, "media growing")?;
        let parsed = MediaGrowing {
            live_edge_offset_frames: growing.u32("live_edge_offset_frames")?,
            idle_completion_refreshes: growing.u32("idle_completion_refreshes")?,
        };
        growing.finish()?;
        Ok(parsed)
    })?;
    let start_frame = object.u64("start_frame")?;
    object.finish()?;
    let settings = MediaPlaybackSettings {
        trim_in,
        trim_out,
        end_action,
        end_trigger_lead,
        speed_basis_points,
        audio_policy,
        audio_stream,
        growing,
        start_frame,
    };
    settings
        .validate()
        .map_err(|error| syntax(error.to_string()))?;
    Ok(settings)
}
