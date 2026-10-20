mod animation;
mod audio_mixer;
mod capture;
mod cue;
mod decode;
mod display_output;
mod encode;
mod grade;
mod input_lifecycle;
mod layout;
mod live_lan;
mod media;
mod media_list;
mod network_stream;
mod paged;
mod playlist;
mod recorder_status;
mod refusal;
mod scene_layer;
mod scene_layout;
mod script;
mod shortcut;
mod source_sync;
mod still;
mod stream_target;
mod system_audio;
mod title;
mod transition;
mod value;
mod web_access;

pub use animation::{motion_wire_bytes, scene_clip_wire_bytes, title_clip_wire_bytes};
pub use cue::{cue_action_token, cue_name_is_valid, parse_cue_action_token};
pub use decode::{LineDecoder, decode_line};
pub use encode::encode_line;
pub use refusal::{decode_refusal_reason, encode_refusal_reason};
pub use scene_layout::scene_layout_wire_bytes;
pub use shortcut::validate_shortcut_spec;

use core::fmt;

pub const MAX_LINE_BYTES: usize = 64 * 1024;
/// Most fields one record carries (96 since Protocol 2.58, whose snapshot
/// outgrew 64).
pub const MAX_FIELDS_PER_MESSAGE: usize = 96;
pub const MAX_FIELD_NAME_BYTES: usize = 64;
/// Longest field value (60 KiB since Protocol 2.70, so the worst-case scene
/// layout with key/fill keys and key refinements fits one field).
pub const MAX_FIELD_VALUE_BYTES: usize = 60 * 1024;
pub const MAX_LIST_ITEMS: usize = 256;
pub const MAX_BATCH_EVENTS: usize = 256;
pub const MAX_MESSAGES_PER_PUSH: usize = 1024;
pub const MAX_REQUEST_ID_BYTES: usize = 128;
/// Largest `StreamStatus` name in bytes.
pub const MAX_STREAM_NAME_BYTES: usize = 128;
/// Largest redacted `StreamStatus` detail string in bytes.
pub const MAX_STREAM_DETAIL_BYTES: usize = 64;
/// Largest `StreamStatusMessage` sample count; matches the engine's
/// bounded stream inventory.
pub const MAX_STREAM_SAMPLES: usize = 5;
/// Largest accepted [`crate::StreamLocalStatus::segments`] count: one
/// playlist may publish at most this many media segments.
pub const MAX_STREAM_LOCAL_SEGMENTS: u32 = 4_096;
/// Largest accepted [`crate::StreamLocalStatus::target_duration_ms`]: one
/// hour in whole milliseconds.
pub const MAX_STREAM_LOCAL_TARGET_DURATION_MS: u32 = 3_600_000;

use crate::{
    EditHistoryStatus, MAX_EDIT_HISTORY_DEPTH, MAX_MONITOR_FRAME_HEIGHT,
    MAX_MONITOR_FRAME_PART_BYTES, MAX_MONITOR_FRAME_PARTS, MAX_MONITOR_FRAME_WIDTH, WireInputId,
};

/// Enforces the undo/redo summary invariants shared by the encoder and
/// decoder: each depth is at most [`MAX_EDIT_HISTORY_DEPTH`], each top edit
/// is present exactly when its depth is nonzero, names a revision in
/// `1..=revision` of the carrying record, and the two tops differ.
fn validate_edit_history(history: &EditHistoryStatus, revision: u64) -> Result<(), CodecError> {
    let side = |field: &'static str, depth: u16, edit: Option<u64>| {
        let valid = depth <= MAX_EDIT_HISTORY_DEPTH
            && match edit {
                None => depth == 0,
                Some(edit) => depth > 0 && (1..=revision).contains(&edit),
            };
        if valid {
            Ok(())
        } else {
            Err(CodecError::InvalidField {
                field,
                value: format!(
                    "{depth}:{}",
                    edit.map_or_else(|| "none".to_owned(), |edit| edit.to_string())
                ),
            })
        }
    };
    side("undo_edit", history.undo_depth, history.next_undo)?;
    side("redo_edit", history.redo_depth, history.next_redo)?;
    if history.next_undo.is_some() && history.next_undo == history.next_redo {
        return Err(CodecError::InvalidField {
            field: "redo_edit",
            value: history.next_redo.unwrap_or_default().to_string(),
        });
    }
    Ok(())
}

/// Enforces the locked-input list invariants shared by the encoder and
/// decoder: strictly ascending identifiers, each one an input of the same
/// snapshot.
fn validate_locked_inputs(
    locked: &[WireInputId],
    inputs: &[WireInputId],
) -> Result<(), CodecError> {
    if locked.len() > MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems("locked_inputs"));
    }
    let ascending = locked.windows(2).all(|pair| pair[0].get() < pair[1].get());
    if let Some(unknown) = locked.iter().find(|input| !inputs.contains(input)) {
        return Err(CodecError::InvalidField {
            field: "locked_inputs",
            value: unknown.to_string(),
        });
    }
    if ascending {
        Ok(())
    } else {
        Err(CodecError::InvalidField {
            field: "locked_inputs",
            value: locked
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        })
    }
}

/// Enforces the input-name rule shared by the encoder and decoder for the
/// records that introduce an input: nonblank and at most
/// [`fm_types::MAX_INPUT_NAME_BYTES`] bytes.
fn validate_new_input_name(name: &str) -> Result<(), CodecError> {
    fm_types::validate_input_name(name).map_err(|_| CodecError::InvalidField {
        field: "name",
        value: name.to_owned(),
    })
}

/// Encodes a solid color as eight lowercase hex digits, `rrggbbaa`.
fn encode_rgba(red: u8, green: u8, blue: u8, alpha: u8) -> String {
    format!("{red:02x}{green:02x}{blue:02x}{alpha:02x}")
}

/// Decodes exactly eight lowercase hex digits, `rrggbbaa`.
fn decode_rgba(value: &str) -> Option<[u8; 4]> {
    if value.len() != 8
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let channel = |index: usize| u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok();
    Some([channel(0)?, channel(1)?, channel(2)?, channel(3)?])
}

/// Enforces the `input_thumbnail` bounds shared by the encoder and decoder
/// (Protocol 2.59).
fn validate_input_thumbnail(width: u16, height: u16, data_len: usize) -> Result<(), CodecError> {
    let invalid =
        |field: &'static str, value: String| Err(CodecError::InvalidField { field, value });
    if !(1..=crate::MAX_INPUT_THUMBNAIL_WIDTH).contains(&width) {
        return invalid("width", width.to_string());
    }
    if !(1..=crate::MAX_INPUT_THUMBNAIL_HEIGHT).contains(&height) {
        return invalid("height", height.to_string());
    }
    if !(1..=crate::MAX_INPUT_THUMBNAIL_BYTES).contains(&data_len) {
        return invalid("data", format!("{data_len} bytes"));
    }
    Ok(())
}

/// Enforces the `monitor_frame` bounds shared by the encoder and decoder.
fn validate_monitor_frame(
    width: u16,
    height: u16,
    part: u16,
    parts: u16,
    data_len: usize,
) -> Result<(), CodecError> {
    let invalid =
        |field: &'static str, value: String| Err(CodecError::InvalidField { field, value });
    if !(1..=MAX_MONITOR_FRAME_WIDTH).contains(&width) {
        return invalid("width", width.to_string());
    }
    if !(1..=MAX_MONITOR_FRAME_HEIGHT).contains(&height) {
        return invalid("height", height.to_string());
    }
    if !(1..=MAX_MONITOR_FRAME_PARTS).contains(&parts) {
        return invalid("parts", parts.to_string());
    }
    if part >= parts {
        return invalid("part", part.to_string());
    }
    if !(1..=MAX_MONITOR_FRAME_PART_BYTES).contains(&data_len) {
        return invalid("data", format!("{data_len} bytes"));
    }
    Ok(())
}

/// Enforces the overlay transition duration bound shared by the encoder and
/// decoder.
fn validate_overlay_duration(duration_frames: u32, field: &'static str) -> Result<(), CodecError> {
    if (1..=3_600).contains(&duration_frames) {
        Ok(())
    } else {
        Err(CodecError::InvalidField {
            field,
            value: duration_frames.to_string(),
        })
    }
}

fn validate_request_id(value: &str) -> Result<(), CodecError> {
    if value.is_empty()
        || value.len() > MAX_REQUEST_ID_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        Err(CodecError::InvalidField {
            field: "request_id",
            value: value.to_owned(),
        })
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodecError {
    LineTooLong,
    MissingNewline,
    MultipleLines,
    InvalidRecord,
    InvalidEscape,
    InvalidUtf8,
    DuplicateField(String),
    MissingField(&'static str),
    UnknownField(String),
    InvalidField { field: &'static str, value: String },
    UnknownMessage(String),
    TrailingData,
    TooManyFields,
    FieldNameTooLong,
    FieldValueTooLong,
    TooManyItems(&'static str),
    TooManyMessages,
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LineTooLong => formatter.write_str("protocol line exceeds maximum length"),
            Self::MissingNewline => formatter.write_str("protocol record must end with newline"),
            Self::MultipleLines => formatter.write_str("decode_line accepts exactly one record"),
            Self::InvalidRecord => formatter.write_str("protocol record is malformed"),
            Self::InvalidEscape => formatter.write_str("field contains an invalid percent escape"),
            Self::InvalidUtf8 => formatter.write_str("field is not valid UTF-8"),
            Self::DuplicateField(field) => write!(formatter, "duplicate field {field}"),
            Self::MissingField(field) => write!(formatter, "required field {field} is missing"),
            Self::UnknownField(field) => write!(formatter, "unknown field {field}"),
            Self::InvalidField { field, value } => {
                write!(formatter, "field {field} has invalid value {value}")
            }
            Self::UnknownMessage(kind) => write!(formatter, "unknown message type {kind}"),
            Self::TrailingData => formatter.write_str("stream ended with an incomplete record"),
            Self::TooManyFields => formatter.write_str("protocol record contains too many fields"),
            Self::FieldNameTooLong => formatter.write_str("protocol field name is too long"),
            Self::FieldValueTooLong => formatter.write_str("protocol field value is too long"),
            Self::TooManyItems(field) => write!(formatter, "field {field} contains too many items"),
            Self::TooManyMessages => formatter.write_str("stream chunk contains too many messages"),
        }
    }
}

impl std::error::Error for CodecError {}
