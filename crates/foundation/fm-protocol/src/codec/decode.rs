use core::{num::NonZeroU128, str::FromStr};
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    AudioMeterChannel, AudioMetersMessage, CapabilityReportMessage, CapabilityReportSummary,
    CodecError, CommandMessage, CommandPayload, CommandResult, DiagnosticsRequest,
    DiagnosticsResponse, DurableEventBatch, DurableGap, EditHistoryStatus, EditLockScope,
    EditLockStatus, EngineIdentity, ErrorMessage, EventCursor, EventMessage, EventPayload,
    FadeToBlackPosition, FadeToBlackState, GradeChange, GradeEntry, GradeStateMessage,
    HandshakeOutcome, HandshakeRequest, HandshakeResponse, HeartbeatAcknowledgementMessage,
    HeartbeatMessage, InputAudioMeters, InputAudioStripStatus, LiveInputSource,
    MAX_AUDIO_METER_CHANNELS, MAX_GRADE_STATES, ManualTransitionKind, ManualTransitionPosition,
    ManualTransitionState, ManualTransitionStatus, MonitorFrameMessage, MonitorFrameOrigin,
    MonitorTap, OverlayStatus, PlaylistOperation, PlaylistStateMessage, ResumeCursor,
    RuntimeEventMessage, RuntimeFailureDisposition, RuntimeLifecycleEvent,
    SceneAnimationStateMessage, SceneLayoutStateMessage, ServerIdentity, SnapshotMessage,
    SnapshotReason, StingerAudioPolicy, StingerMissingMediaFallback, StingerReadiness,
    StingerStatus, StreamLocalStatus, StreamStatusMessage, StreamStatusSample, StructuredError,
    TitleAnimationStateMessage, TitleFieldStatus, TitleLayoutStateMessage,
    TitlePreviewFrameMessage, TitlePreviewRequest, TitleStateMessage, WireGradeSource, WireInputId,
    WireMessage, WireOutputId, WireOverlayChannelId, WireRecorderSlot, WireSceneLayerId,
    WireStingerSlotId, WireStreamTargetId, WireTitleElementId,
};

use super::animation::{
    decode_animation_phase, decode_animation_preset, decode_clip, decode_preview_sample,
    validate_animation_inputs, validate_phase_frames, validate_preview_part, validate_preview_size,
};
use super::grade::{
    decode_cube_kind, decode_grade, decode_lut_data, parse_grade_id, validate_grade_name,
    validate_lut_size,
};
use super::layout::decode_layout;
use super::playlist::{
    decode_frame_rate, decode_optional_entry, decode_playlist_entries, decode_playlist_start,
    decode_playlist_state, parse_playlist_loop,
};
use super::scene_layout::{decode_scene_clip, decode_scene_layout, decode_scene_list};
use super::title::{
    decode_title_field_names, decode_title_fields, decode_title_phase, decode_title_summaries,
    parse_title_air_requirement, validate_title_name, validate_title_phase,
};
use super::value::{
    parse_canonical_input, parse_canonical_u64, parse_client_type, parse_durable_events,
    parse_field_issues, parse_input_ids, parse_input_statuses, parse_output_statuses,
    parse_recording_statuses, parse_role, parse_runtime_domains, parse_stream_realized_state,
    parse_stream_statuses, parse_string_list, parse_version, unescape,
};
use super::{
    MAX_FIELD_NAME_BYTES, MAX_FIELD_VALUE_BYTES, MAX_FIELDS_PER_MESSAGE, MAX_LINE_BYTES,
    MAX_LIST_ITEMS, MAX_MESSAGES_PER_PUSH, MAX_STREAM_LOCAL_SEGMENTS,
    MAX_STREAM_LOCAL_TARGET_DURATION_MS, MAX_STREAM_SAMPLES, decode_rgba, validate_edit_history,
    validate_locked_inputs, validate_new_input_name, validate_overlay_duration,
    validate_request_id,
};

/// Decodes exactly one newline-terminated wire record.
///
/// Unknown fields are rejected by the exact-current contract.
///
/// # Errors
///
/// Returns a [`CodecError`] for invalid framing, escaping, required fields,
/// field values, or message types.
pub fn decode_line(line: &str) -> Result<WireMessage, CodecError> {
    if line.len() > MAX_LINE_BYTES {
        return Err(CodecError::LineTooLong);
    }
    let Some(record) = line.strip_suffix('\n') else {
        return Err(CodecError::MissingNewline);
    };
    if record.contains('\n') || record.contains('\r') {
        return Err(CodecError::MultipleLines);
    }
    let mut parts = record.split('\t');
    let kind = parts
        .next()
        .filter(|kind| !kind.is_empty())
        .ok_or(CodecError::InvalidRecord)?;
    let mut fields = Fields::parse(parts)?;
    let message = match kind {
        "command" => WireMessage::Command(decode_command(&mut fields)?),
        "command_result" => WireMessage::CommandResult(decode_result(&mut fields)?),
        "snapshot" => WireMessage::Snapshot(decode_snapshot(&mut fields)?),
        "event" => WireMessage::Event(decode_event(&mut fields)?),
        "handshake_request" => {
            WireMessage::HandshakeRequest(decode_handshake_request(&mut fields)?)
        }
        "handshake_response" => {
            WireMessage::HandshakeResponse(decode_handshake_response(&mut fields)?)
        }
        "durable_event_batch" => WireMessage::DurableEventBatch(decode_durable_batch(&mut fields)?),
        "durable_gap" => WireMessage::DurableGap(decode_durable_gap(&mut fields)?),
        "runtime_event" => WireMessage::RuntimeEvent(decode_runtime_event(&mut fields)?),
        "heartbeat" => WireMessage::Heartbeat(decode_heartbeat(&mut fields)?),
        "heartbeat_acknowledgement" => {
            WireMessage::HeartbeatAcknowledgement(decode_heartbeat_acknowledgement(&mut fields)?)
        }
        "audio_meters" => WireMessage::AudioMeters(decode_audio_meters(&mut fields)?),
        "stream_status" => WireMessage::StreamStatus(decode_stream_status(&mut fields)?),
        "monitor_frame" => WireMessage::MonitorFrame(decode_monitor_frame(&mut fields)?),
        "capability_report" => {
            WireMessage::CapabilityReport(decode_capability_report(&mut fields)?)
        }
        "diagnostics_request" => {
            WireMessage::DiagnosticsRequest(decode_diagnostics_request(&mut fields)?)
        }
        "diagnostics_response" => {
            WireMessage::DiagnosticsResponse(decode_diagnostics_response(&mut fields)?)
        }
        "error" => WireMessage::Error(decode_error_message(&mut fields)?),
        "title_state" => WireMessage::TitleState(decode_title_state(&mut fields)?),
        "playlist_state" => WireMessage::PlaylistState(decode_playlist_state_record(&mut fields)?),
        "grade_state" => WireMessage::GradeState(decode_grade_state(&mut fields)?),
        "title_animation_state" => {
            WireMessage::TitleAnimationState(decode_title_animation_state(&mut fields)?)
        }
        "title_layout_state" => {
            WireMessage::TitleLayoutState(decode_title_layout_state(&mut fields)?)
        }
        "scene_animation_state" => {
            WireMessage::SceneAnimationState(decode_scene_animation_state(&mut fields)?)
        }
        "scene_layout_state" => {
            WireMessage::SceneLayoutState(decode_scene_layout_state(&mut fields)?)
        }
        "title_preview_request" => {
            WireMessage::TitlePreviewRequest(decode_title_preview_request(&mut fields)?)
        }
        "title_preview_frame" => {
            WireMessage::TitlePreviewFrame(decode_title_preview_frame(&mut fields)?)
        }
        _ => return Err(CodecError::UnknownMessage(kind.to_owned())),
    };
    fields.finish()?;
    Ok(message)
}

#[derive(Clone, Debug, Default)]
pub struct LineDecoder {
    buffer: Vec<u8>,
}

impl LineDecoder {
    #[must_use]
    pub const fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Adds an arbitrary stream chunk and returns every complete record.
    ///
    /// # Errors
    ///
    /// Returns a [`CodecError`] for an oversized, non-UTF-8, or malformed
    /// record. Successfully decoded earlier records are not returned when a
    /// later record in the same chunk fails.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<WireMessage>, CodecError> {
        let mut messages = Vec::new();
        for part in chunk.split_inclusive(|byte| *byte == b'\n') {
            if self.buffer.len() + part.len() > MAX_LINE_BYTES {
                self.buffer.clear();
                return Err(CodecError::LineTooLong);
            }
            self.buffer.extend_from_slice(part);
            if part.last() == Some(&b'\n') {
                if messages.len() == MAX_MESSAGES_PER_PUSH {
                    self.buffer.clear();
                    return Err(CodecError::TooManyMessages);
                }
                let Ok(line) = core::str::from_utf8(&self.buffer) else {
                    self.buffer.clear();
                    return Err(CodecError::InvalidUtf8);
                };
                let decoded = decode_line(line);
                self.buffer.clear();
                let message = decoded?;
                messages.push(message);
            } else if self.buffer.len() == MAX_LINE_BYTES {
                self.buffer.clear();
                return Err(CodecError::LineTooLong);
            }
        }
        Ok(messages)
    }

    /// Verifies that the stream ended at a record boundary.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::TrailingData`] when an incomplete record remains.
    pub fn finish(self) -> Result<(), CodecError> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            Err(CodecError::TrailingData)
        }
    }
}

struct Fields(BTreeMap<String, String>);

impl Fields {
    fn parse<'a>(parts: impl Iterator<Item = &'a str>) -> Result<Self, CodecError> {
        let mut fields = BTreeMap::new();
        for part in parts {
            if fields.len() == MAX_FIELDS_PER_MESSAGE {
                return Err(CodecError::TooManyFields);
            }
            let (name, value) = part.split_once('=').ok_or(CodecError::InvalidRecord)?;
            if name.len() > MAX_FIELD_NAME_BYTES {
                return Err(CodecError::FieldNameTooLong);
            }
            if value.len() > MAX_FIELD_VALUE_BYTES {
                return Err(CodecError::FieldValueTooLong);
            }
            if name.is_empty() || name.bytes().any(|byte| !valid_field_byte(byte)) {
                return Err(CodecError::InvalidRecord);
            }
            if fields.contains_key(name) {
                return Err(CodecError::DuplicateField(name.to_owned()));
            }
            let value = unescape(value)?;
            fields.insert(name.to_owned(), value);
        }
        Ok(Self(fields))
    }

    fn required(&mut self, name: &'static str) -> Result<String, CodecError> {
        self.0.remove(name).ok_or(CodecError::MissingField(name))
    }

    fn optional(&mut self, name: &'static str) -> Option<String> {
        self.0.remove(name)
    }

    fn parse_required<T: FromStr>(&mut self, name: &'static str) -> Result<T, CodecError> {
        let value = self.required(name)?;
        value
            .parse()
            .map_err(|_| CodecError::InvalidField { field: name, value })
    }

    fn parse_optional<T: FromStr>(&mut self, name: &'static str) -> Result<Option<T>, CodecError> {
        self.optional(name)
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| CodecError::InvalidField { field: name, value })
            })
            .transpose()
    }

    fn boolean(&mut self, name: &'static str) -> Result<bool, CodecError> {
        let value = self.required(name)?;
        match value.as_str() {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(CodecError::InvalidField { field: name, value }),
        }
    }

    /// Parses an optional canonical decimal (no sign, no leading zero).
    fn canonical_optional(&mut self, name: &'static str) -> Result<Option<u64>, CodecError> {
        self.optional(name)
            .map(|value| {
                parse_canonical_u64(&value).ok_or(CodecError::InvalidField { field: name, value })
            })
            .transpose()
    }

    /// Parses a required canonical decimal (no sign, no leading zero).
    fn canonical_required(&mut self, name: &'static str) -> Result<u64, CodecError> {
        let value = self.required(name)?;
        parse_canonical_u64(&value).ok_or(CodecError::InvalidField { field: name, value })
    }

    fn input(&mut self, name: &'static str) -> Result<WireInputId, CodecError> {
        let value = self.required(name)?;
        parse_input(&value).ok_or(CodecError::InvalidField { field: name, value })
    }

    /// Parses a required canonical nonzero input identifier.
    fn canonical_input(&mut self, name: &'static str) -> Result<WireInputId, CodecError> {
        let value = self.required(name)?;
        parse_canonical_input(&value).ok_or(CodecError::InvalidField { field: name, value })
    }

    fn finish(self) -> Result<(), CodecError> {
        self.0
            .into_keys()
            .next()
            .map_or(Ok(()), |name| Err(CodecError::UnknownField(name)))
    }
}

const fn valid_field_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn decode_identity(fields: &mut Fields) -> Result<EngineIdentity, CodecError> {
    Ok(EngineIdentity {
        engine_id: fields.required("engine_id")?,
        state_epoch: fields.parse_required("state_epoch")?,
        log_id: fields.required("log_id")?,
    })
}

fn decode_cursor(fields: &mut Fields) -> Result<EventCursor, CodecError> {
    Ok(EventCursor {
        engine: decode_identity(fields)?,
        revision: fields.parse_required("revision")?,
    })
}

fn decode_command(fields: &mut Fields) -> Result<CommandMessage, CodecError> {
    let value = fields.required("protocol")?;
    let protocol = parse_version(&value).ok_or(CodecError::InvalidField {
        field: "protocol",
        value,
    })?;
    let payload_name = fields.required("payload")?;
    let payload = decode_command_payload(fields, payload_name)?;
    Ok(CommandMessage {
        protocol,
        id: fields.required("id")?,
        idempotency_key: fields.required("idempotency_key")?,
        expected_revision: fields.parse_optional("expected_revision")?,
        deadline_ms: fields.parse_optional("deadline_ms")?,
        payload,
    })
}

/// Decodes the input catalogue commands: reorder, rename, add, and remove.
fn decode_input_catalogue_command(
    fields: &mut Fields,
    payload_name: &str,
) -> Result<CommandPayload, CodecError> {
    Ok(match payload_name {
        "input_reorder" => CommandPayload::ReorderInputs {
            inputs: parse_input_ids(&fields.required("inputs")?)?,
        },
        "input_rename" => CommandPayload::RenameInput {
            input: fields.input("input")?,
            name: fields.required("name")?,
        },
        "input_add" => CommandPayload::AddInput {
            input: fields.canonical_input("input")?,
            name: decode_new_input_name(fields)?,
            source: decode_live_input_source(fields)?,
        },
        _ => CommandPayload::RemoveInput {
            input: fields.canonical_input("input")?,
        },
    })
}

fn decode_command_payload(
    fields: &mut Fields,
    payload_name: String,
) -> Result<CommandPayload, CodecError> {
    Ok(match payload_name.as_str() {
        catalogue @ ("input_reorder" | "input_rename" | "input_add" | "input_remove") => {
            decode_input_catalogue_command(fields, catalogue)?
        }
        "input_audio_strip" => CommandPayload::SetInputAudioStrip {
            input: fields.input("input")?,
            gain_millidb: fields.parse_required("gain_millidb")?,
            balance_basis_points: fields.parse_required("balance_basis_points")?,
            muted: fields.boolean("muted")?,
            soloed: fields.boolean("soloed")?,
            follow_video: fields.boolean("follow_video")?,
            delay_samples: fields.parse_required("delay_samples")?,
        },
        "select_preview" => CommandPayload::SelectPreview {
            input: fields.input("input")?,
        },
        "cut" => CommandPayload::Cut,
        "fade" => CommandPayload::Fade {
            duration_frames: fields.parse_required("duration_frames")?,
        },
        "alpha_fade" => CommandPayload::AlphaFade {
            duration_frames: fields.parse_required("duration_frames")?,
        },
        "slide" => CommandPayload::Slide {
            duration_frames: fields.parse_required("duration_frames")?,
        },
        "zoom" => CommandPayload::Zoom {
            duration_frames: fields.parse_required("duration_frames")?,
        },
        "stinger" => {
            let number: u8 = fields.parse_required("slot")?;
            let slot = crate::WireStingerSlotId::new(number).ok_or(CodecError::InvalidField {
                field: "slot",
                value: number.to_string(),
            })?;
            CommandPayload::Stinger {
                slot,
                duration_frames: fields.parse_required("duration_frames")?,
            }
        }
        "configure_stinger" => decode_configure_stinger(fields)?,
        "remove_stinger" => CommandPayload::RemoveStinger {
            slot: decode_stinger_slot(fields)?,
        },
        overlay @ ("overlay_take" | "overlay_update" | "overlay_off" | "overlay_output"
        | "overlay_transition" | "overlay_appearance" | "overlay_queue"
        | "overlay_next") => decode_overlay_command(fields, overlay)?,
        stream @ ("stream_start" | "stream_stop") => decode_stream_command(fields, stream)?,
        record @ ("record_start" | "record_stop") => decode_record_command(fields, record)?,
        "wipe" => CommandPayload::Wipe {
            duration_frames: fields.parse_required("duration_frames")?,
        },
        "fade_to_black" => CommandPayload::FadeToBlack {
            active: fields.boolean("active")?,
            duration_frames: fields.parse_required("duration_frames")?,
        },
        manual @ ("manual_start" | "manual_position") => decode_manual_command(fields, manual)?,
        "manual_commit" => CommandPayload::CommitManualTransition,
        "manual_cancel" => CommandPayload::CancelManualTransition,
        edit @ ("undo" | "redo" | "edit_lock") => decode_edit_command(fields, edit)?,
        title @ ("title_fields" | "title_take" | "title_off_air") => {
            decode_title_command(fields, title)?
        }
        name if name.starts_with("title_animation_") => {
            decode_title_animation_command(fields, name)?
        }
        name if name.starts_with("scene_animation_") => {
            decode_scene_animation_command(fields, name)?
        }
        name if name.starts_with("playlist_") => decode_playlist_command(fields, name)?,
        name if name.starts_with("grade_") => decode_grade_command(fields, name)?,
        _ => {
            return Err(CodecError::InvalidField {
                field: "payload",
                value: payload_name,
            });
        }
    })
}

/// Decodes the manual transition start and position commands.
fn decode_manual_command(fields: &mut Fields, name: &str) -> Result<CommandPayload, CodecError> {
    if name == "manual_start" {
        let value = fields.required("transition")?;
        let kind = match value.as_str() {
            "fade" => crate::ManualTransitionKind::Fade,
            "wipe" => crate::ManualTransitionKind::Wipe,
            "alpha_fade" => crate::ManualTransitionKind::AlphaFade,
            "slide" => crate::ManualTransitionKind::Slide,
            _ => {
                return Err(CodecError::InvalidField {
                    field: "transition",
                    value,
                });
            }
        };
        return Ok(CommandPayload::StartManualTransition { kind });
    }
    let value = fields.required("position_basis_points")?;
    let basis_points = value.parse().map_err(|_| CodecError::InvalidField {
        field: "position_basis_points",
        value: value.clone(),
    })?;
    let position =
        crate::ManualTransitionPosition::new(basis_points).ok_or(CodecError::InvalidField {
            field: "position_basis_points",
            value,
        })?;
    Ok(CommandPayload::SetManualTransitionPosition { position })
}

/// Decodes the playlist commands: list edit, controls, and clock advance.
fn decode_playlist_command(fields: &mut Fields, name: &str) -> Result<CommandPayload, CodecError> {
    let operation = match name {
        "playlist_set" => {
            return Ok(CommandPayload::SetPlaylist {
                loop_mode: parse_playlist_loop(fields.required("loop")?)?,
                entries: decode_playlist_entries(&fields.required("entries")?)?,
            });
        }
        "playlist_advance" => {
            return Ok(CommandPayload::PlaylistAdvance {
                frame: fields.canonical_required("frame")?,
            });
        }
        "playlist_start" => {
            let at = decode_playlist_start(fields.required("start")?)?;
            let from = decode_optional_entry("from", fields.required("from")?)?;
            PlaylistOperation::Start { at, from }
        }
        "playlist_stop" => PlaylistOperation::Stop,
        "playlist_pause" => PlaylistOperation::Pause,
        "playlist_resume" => PlaylistOperation::Resume,
        "playlist_next" => PlaylistOperation::Next,
        "playlist_previous" => PlaylistOperation::Previous,
        "playlist_jump" => PlaylistOperation::Jump {
            entry: fields.canonical_required("entry")?,
        },
        _ => {
            return Err(CodecError::InvalidField {
                field: "payload",
                value: name.to_owned(),
            });
        }
    };
    Ok(CommandPayload::PlaylistControl { operation })
}

/// Decodes the title commands: field save, take, and off-air.
fn decode_title_command(fields: &mut Fields, name: &str) -> Result<CommandPayload, CodecError> {
    let input = fields.canonical_input("input")?;
    Ok(match name {
        "title_fields" => CommandPayload::SetTitleFields {
            input,
            base_revision: fields.canonical_required("base_revision")?,
            require: parse_title_air_requirement(fields.required("require")?)?,
            fields: decode_title_fields(&fields.required("fields")?)?,
        },
        "title_take" => CommandPayload::TakeTitle {
            input,
            channel: decode_overlay_channel(fields)?,
            base_revision: fields.canonical_required("base_revision")?,
            fields: decode_title_fields(&fields.required("fields")?)?,
        },
        _ => CommandPayload::TitleOffAir { input },
    })
}

fn decode_title_animation_command(
    fields: &mut Fields,
    name: &str,
) -> Result<CommandPayload, CodecError> {
    let input = fields.canonical_input("input")?;
    let base_revision = fields.canonical_required("base_revision")?;
    Ok(match name {
        "title_animation_set" => CommandPayload::SetTitleAnimation {
            input,
            base_revision,
            clip: Box::new(decode_clip(&fields.required("clip")?)?),
        },
        "title_animation_preset" => {
            let element = {
                let value = fields.required("element")?;
                parse_canonical_u128(&value)
                    .and_then(NonZeroU128::new)
                    .map(WireTitleElementId::new)
                    .ok_or(CodecError::InvalidField {
                        field: "element",
                        value,
                    })?
            };
            let phase = decode_animation_phase(fields.required("phase")?)?;
            let preset = decode_animation_preset(fields.required("preset")?)?;
            let frames = decode_phase_frames(fields)?;
            CommandPayload::ApplyTitleAnimationPreset {
                input,
                base_revision,
                element,
                phase,
                preset,
                frames,
            }
        }
        "title_animation_stretch" => CommandPayload::StretchTitleAnimation {
            input,
            base_revision,
            phase: decode_animation_phase(fields.required("phase")?)?,
            frames: decode_phase_frames(fields)?,
        },
        _ => {
            return Err(CodecError::InvalidField {
                field: "payload",
                value: name.to_owned(),
            });
        }
    })
}

fn decode_scene_animation_command(
    fields: &mut Fields,
    name: &str,
) -> Result<CommandPayload, CodecError> {
    let input = fields.canonical_input("input")?;
    let base_revision = fields.canonical_required("base_revision")?;
    Ok(match name {
        "scene_animation_set" => CommandPayload::SetSceneAnimation {
            input,
            base_revision,
            clip: Box::new(decode_scene_clip(&fields.required("clip")?)?),
        },
        "scene_animation_preset" => {
            let layer = {
                let value = fields.required("layer")?;
                parse_canonical_u128(&value)
                    .and_then(|layer| u32::try_from(layer).ok())
                    .and_then(core::num::NonZeroU32::new)
                    .map(WireSceneLayerId::new)
                    .ok_or(CodecError::InvalidField {
                        field: "layer",
                        value,
                    })?
            };
            let phase = decode_animation_phase(fields.required("phase")?)?;
            let preset = decode_animation_preset(fields.required("preset")?)?;
            let frames = decode_phase_frames(fields)?;
            CommandPayload::ApplySceneAnimationPreset {
                input,
                base_revision,
                layer,
                phase,
                preset,
                frames,
            }
        }
        "scene_animation_stretch" => CommandPayload::StretchSceneAnimation {
            input,
            base_revision,
            phase: decode_animation_phase(fields.required("phase")?)?,
            frames: decode_phase_frames(fields)?,
        },
        _ => {
            return Err(CodecError::InvalidField {
                field: "payload",
                value: name.to_owned(),
            });
        }
    })
}

fn decode_scene_animation_state(
    fields: &mut Fields,
) -> Result<SceneAnimationStateMessage, CodecError> {
    Ok(SceneAnimationStateMessage {
        revision: fields.canonical_required("revision")?,
        input: fields.canonical_input("input")?,
        animation_revision: fields.canonical_required("animation_revision")?,
        clip: Box::new(decode_scene_clip(&fields.required("clip")?)?),
    })
}

fn decode_scene_layout_state(fields: &mut Fields) -> Result<SceneLayoutStateMessage, CodecError> {
    let revision = fields.canonical_required("revision")?;
    let input = fields.canonical_input("input")?;
    let side = |fields: &mut Fields, name: &'static str| {
        let value = fields.canonical_required(name)?;
        u32::try_from(value).map_err(|_| CodecError::InvalidField {
            field: name,
            value: value.to_string(),
        })
    };
    let width = side(fields, "width")?;
    let height = side(fields, "height")?;
    let layout = decode_scene_layout(width, height, &fields.required("layers")?)?;
    Ok(SceneLayoutStateMessage {
        revision,
        input,
        layout: Box::new(layout),
    })
}

fn decode_phase_frames(fields: &mut Fields) -> Result<u32, CodecError> {
    let value = fields.required("frames")?;
    let frames = parse_canonical_u64(&value)
        .and_then(|frames| u32::try_from(frames).ok())
        .ok_or(CodecError::InvalidField {
            field: "frames",
            value,
        })?;
    validate_phase_frames(frames)?;
    Ok(frames)
}

/// Parses a canonical decimal `u128` (no sign, no leading zero).
fn parse_canonical_u128(value: &str) -> Option<u128> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.starts_with('0') && value != "0")
    {
        return None;
    }
    value.parse().ok()
}

fn decode_title_animation_state(
    fields: &mut Fields,
) -> Result<TitleAnimationStateMessage, CodecError> {
    Ok(TitleAnimationStateMessage {
        revision: fields.canonical_required("revision")?,
        input: fields.canonical_input("input")?,
        animation_revision: fields.canonical_required("animation_revision")?,
        clip: Box::new(decode_clip(&fields.required("clip")?)?),
    })
}

fn decode_title_layout_state(fields: &mut Fields) -> Result<TitleLayoutStateMessage, CodecError> {
    let revision = fields.canonical_required("revision")?;
    let input = fields.canonical_input("input")?;
    let side = |fields: &mut Fields, name: &'static str| {
        let value = fields.canonical_required(name)?;
        u32::try_from(value).map_err(|_| CodecError::InvalidField {
            field: name,
            value: value.to_string(),
        })
    };
    let width = side(fields, "width")?;
    let height = side(fields, "height")?;
    let layout = decode_layout(width, height, &fields.required("elements")?)?;
    Ok(TitleLayoutStateMessage {
        revision,
        input,
        layout: Box::new(layout),
    })
}

fn decode_title_preview_request(fields: &mut Fields) -> Result<TitlePreviewRequest, CodecError> {
    let protocol = fields.required("protocol")?;
    let request_id = fields.required("request_id")?;
    validate_request_id(&request_id)?;
    let input = fields.canonical_input("input")?;
    let phase = fields.required("phase")?;
    let frame = fields.canonical_required("frame")?;
    let sample = decode_preview_sample(phase, frame)?;
    let width = decode_preview_dimension(fields, "width")?;
    let height = decode_preview_dimension(fields, "height")?;
    validate_preview_size(width, height)?;
    let clip = fields
        .optional("clip")
        .map(|clip| decode_clip(&clip).map(Box::new))
        .transpose()?;
    Ok(TitlePreviewRequest {
        protocol: parse_version(&protocol).ok_or(CodecError::InvalidField {
            field: "protocol",
            value: protocol,
        })?,
        request_id,
        input,
        sample,
        width,
        height,
        clip,
    })
}

fn decode_preview_dimension(fields: &mut Fields, name: &'static str) -> Result<u16, CodecError> {
    let value = fields.required(name)?;
    parse_canonical_u64(&value)
        .and_then(|dimension| u16::try_from(dimension).ok())
        .ok_or(CodecError::InvalidField { field: name, value })
}

fn decode_title_preview_frame(fields: &mut Fields) -> Result<TitlePreviewFrameMessage, CodecError> {
    let server = decode_server_identity(fields)?;
    let request_id = fields.required("request_id")?;
    validate_request_id(&request_id)?;
    let input = fields.canonical_input("input")?;
    let animation_revision = fields.canonical_required("animation_revision")?;
    let draft = fields.boolean("draft")?;
    let width = decode_preview_dimension(fields, "width")?;
    let height = decode_preview_dimension(fields, "height")?;
    let part = decode_preview_dimension(fields, "part")?;
    let parts = decode_preview_dimension(fields, "parts")?;
    let data_value = fields.required("data")?;
    let encoded_limit = (crate::MAX_TITLE_PREVIEW_PART_BYTES * 4).div_ceil(3);
    let data = if data_value.len() > encoded_limit {
        None
    } else {
        crate::monitor_image::base64url_decode(&data_value)
    }
    .ok_or_else(|| CodecError::InvalidField {
        field: "data",
        value: format!("{} characters", data_value.len()),
    })?;
    validate_preview_part(width, height, part, parts, data.len())?;
    Ok(TitlePreviewFrameMessage {
        server,
        request_id,
        input,
        animation_revision,
        draft,
        width,
        height,
        part,
        parts,
        data,
    })
}

fn decode_title_state(fields: &mut Fields) -> Result<TitleStateMessage, CodecError> {
    let name = fields.required("name")?;
    validate_title_name(&name)?;
    let values = decode_title_fields(&fields.required("fields")?)?;
    let names = decode_title_field_names(&fields.required("field_names")?, values.len())?;
    let phase = decode_title_phase(&fields.required("phase")?)?;
    let phase_frame = fields.canonical_required("phase_frame")?;
    let phase_frames = fields.canonical_required("phase_frames")?;
    validate_title_phase(phase, phase_frame, phase_frames)?;
    Ok(TitleStateMessage {
        revision: fields.canonical_required("revision")?,
        input: fields.canonical_input("input")?,
        name,
        field_revision: fields.canonical_required("field_revision")?,
        phase,
        phase_frame,
        phase_frames,
        fields: values
            .into_iter()
            .zip(names)
            .map(|(update, name)| TitleFieldStatus {
                field: update.field,
                name,
                value: update.value,
            })
            .collect(),
    })
}

fn decode_new_input_name(fields: &mut Fields) -> Result<String, CodecError> {
    let name = fields.required("name")?;
    validate_new_input_name(&name)?;
    Ok(name)
}

fn decode_live_input_source(fields: &mut Fields) -> Result<LiveInputSource, CodecError> {
    let kind = fields.required("kind")?;
    match kind.as_str() {
        "solid" => {
            let value = fields.required("color")?;
            let [red, green, blue, alpha] =
                decode_rgba(&value).ok_or(CodecError::InvalidField {
                    field: "color",
                    value,
                })?;
            Ok(LiveInputSource::Solid {
                red,
                green,
                blue,
                alpha,
            })
        }
        "bars" => Ok(LiveInputSource::Bars),
        _ => Err(CodecError::InvalidField {
            field: "kind",
            value: kind,
        }),
    }
}

fn decode_edit_command(fields: &mut Fields, name: &str) -> Result<CommandPayload, CodecError> {
    Ok(match name {
        "undo" => CommandPayload::Undo {
            expected_edit: decode_expected_edit(fields)?,
        },
        "redo" => CommandPayload::Redo {
            expected_edit: decode_expected_edit(fields)?,
        },
        "edit_lock" => CommandPayload::SetEditLock {
            scope: decode_edit_lock_scope(fields)?,
            locked: fields.boolean("locked")?,
        },
        _ => unreachable!("only edit-history and padlock command names are delegated"),
    })
}

fn decode_expected_edit(fields: &mut Fields) -> Result<Option<u64>, CodecError> {
    match fields.canonical_optional("expected_edit")? {
        Some(0) => Err(CodecError::InvalidField {
            field: "expected_edit",
            value: "0".to_owned(),
        }),
        expected => Ok(expected),
    }
}

fn decode_edit_lock_scope(fields: &mut Fields) -> Result<EditLockScope, CodecError> {
    let scope = fields.required("scope")?;
    match scope.as_str() {
        "project" => Ok(EditLockScope::Project),
        "input" => {
            let value = fields.required("input")?;
            parse_canonical_input(&value)
                .map(EditLockScope::Input)
                .ok_or(CodecError::InvalidField {
                    field: "input",
                    value,
                })
        }
        _ => Err(CodecError::InvalidField {
            field: "scope",
            value: scope,
        }),
    }
}

fn decode_edit_history(
    fields: &mut Fields,
    revision: u64,
) -> Result<EditHistoryStatus, CodecError> {
    let depth = |fields: &mut Fields, name: &'static str| -> Result<u16, CodecError> {
        let value = fields.canonical_required(name)?;
        u16::try_from(value).map_err(|_| CodecError::InvalidField {
            field: name,
            value: value.to_string(),
        })
    };
    let history = EditHistoryStatus {
        undo_depth: depth(fields, "undo_depth")?,
        next_undo: fields.canonical_optional("undo_edit")?,
        redo_depth: depth(fields, "redo_depth")?,
        next_redo: fields.canonical_optional("redo_edit")?,
    };
    validate_edit_history(&history, revision)?;
    Ok(history)
}

fn decode_edit_locks(
    fields: &mut Fields,
    inputs: &[crate::InputStatus],
) -> Result<EditLockStatus, CodecError> {
    let project = fields.boolean("project_locked")?;
    let value = fields.required("locked_inputs")?;
    let locked = if value.is_empty() {
        Vec::new()
    } else {
        if value.split(',').take(MAX_LIST_ITEMS + 1).count() > MAX_LIST_ITEMS {
            return Err(CodecError::TooManyItems("locked_inputs"));
        }
        value
            .split(',')
            .map(parse_canonical_input)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| CodecError::InvalidField {
                field: "locked_inputs",
                value: value.clone(),
            })?
    };
    let inputs = inputs.iter().map(|status| status.input).collect::<Vec<_>>();
    validate_locked_inputs(&locked, &inputs)?;
    Ok(EditLockStatus {
        project,
        inputs: locked,
    })
}

fn decode_overlay_command(fields: &mut Fields, name: &str) -> Result<CommandPayload, CodecError> {
    let channel = decode_overlay_channel(fields)?;
    Ok(match name {
        "overlay_take" => CommandPayload::TakeOverlay {
            channel,
            source: fields.input("source")?,
        },
        "overlay_update" => CommandPayload::UpdateOverlay {
            channel,
            source: fields.input("source")?,
        },
        "overlay_off" => CommandPayload::OverlayOff { channel },
        "overlay_output" => CommandPayload::SetOverlayOutputInclusion {
            channel,
            output: decode_output(fields, "output")?,
            included: fields.boolean("included")?,
        },
        "overlay_transition" => {
            let transition =
                decode_overlay_transition(fields.required("transition")?, "transition")?;
            let duration_frames = fields.parse_required("duration_frames")?;
            validate_overlay_duration(duration_frames, "duration_frames")?;
            CommandPayload::ConfigureOverlayTransition {
                channel,
                transition,
                duration_frames,
            }
        }
        "overlay_appearance" => CommandPayload::ConfigureOverlayAppearance {
            channel,
            position: decode_overlay_position(fields.required("position")?, "position")?,
            border: decode_overlay_border(fields.required("border")?, "border")?,
        },
        "overlay_queue" => CommandPayload::QueueOverlay {
            channel,
            source: fields.input("source")?,
        },
        "overlay_next" => CommandPayload::TakeNextOverlay { channel },
        _ => unreachable!("only overlay command names are delegated"),
    })
}

fn decode_overlay_channel(fields: &mut Fields) -> Result<WireOverlayChannelId, CodecError> {
    let number: u8 = fields.parse_required("channel")?;
    WireOverlayChannelId::new(number).ok_or(CodecError::InvalidField {
        field: "channel",
        value: number.to_string(),
    })
}

fn decode_overlay_transition(
    value: String,
    field: &'static str,
) -> Result<crate::OverlayTransitionKind, CodecError> {
    match value.as_str() {
        "cut" => Ok(crate::OverlayTransitionKind::Cut),
        "fade" => Ok(crate::OverlayTransitionKind::Fade),
        _ => Err(CodecError::InvalidField { field, value }),
    }
}

fn decode_overlay_position(
    value: String,
    field: &'static str,
) -> Result<crate::OverlayPositionPreset, CodecError> {
    match value.as_str() {
        "full_frame" => Ok(crate::OverlayPositionPreset::FullFrame),
        "top_left" => Ok(crate::OverlayPositionPreset::TopLeft),
        "top_right" => Ok(crate::OverlayPositionPreset::TopRight),
        "bottom_left" => Ok(crate::OverlayPositionPreset::BottomLeft),
        "bottom_right" => Ok(crate::OverlayPositionPreset::BottomRight),
        _ => Err(CodecError::InvalidField { field, value }),
    }
}

fn decode_overlay_border(
    value: String,
    field: &'static str,
) -> Result<crate::OverlayBorderPreset, CodecError> {
    match value.as_str() {
        "none" => Ok(crate::OverlayBorderPreset::None),
        "thin_white" => Ok(crate::OverlayBorderPreset::ThinWhite),
        "thick_white" => Ok(crate::OverlayBorderPreset::ThickWhite),
        _ => Err(CodecError::InvalidField { field, value }),
    }
}

fn decode_output(fields: &mut Fields, field: &'static str) -> Result<WireOutputId, CodecError> {
    let value = fields.required(field)?;
    let id = NonZeroU128::new(value.parse().map_err(|_| CodecError::InvalidField {
        field,
        value: value.clone(),
    })?)
    .ok_or(CodecError::InvalidField { field, value })?;
    Ok(WireOutputId::new(id))
}

fn decode_stream_target(fields: &mut Fields) -> Result<WireStreamTargetId, CodecError> {
    let value = fields.required("target")?;
    let id = NonZeroU128::new(value.parse().map_err(|_| CodecError::InvalidField {
        field: "target",
        value: value.clone(),
    })?)
    .ok_or(CodecError::InvalidField {
        field: "target",
        value,
    })?;
    Ok(WireStreamTargetId::new(id))
}

fn decode_configure_stinger(fields: &mut Fields) -> Result<CommandPayload, CodecError> {
    let audio_policy = fields.required("audio_policy")?;
    let missing_media_fallback = fields.required("missing_media_fallback")?;
    Ok(CommandPayload::ConfigureStinger {
        slot: decode_stinger_slot(fields)?,
        media_input: fields.input("media_input")?,
        preload: fields.boolean("preload")?,
        cut_point_frames: fields.parse_required("cut_point_frames")?,
        audio_policy: match audio_policy.as_str() {
            "muted" => StingerAudioPolicy::Muted,
            "stinger_only" => StingerAudioPolicy::StingerOnly,
            "mix_with_program" => StingerAudioPolicy::MixWithProgram,
            _ => {
                return Err(CodecError::InvalidField {
                    field: "audio_policy",
                    value: audio_policy,
                });
            }
        },
        missing_media_fallback: match missing_media_fallback.as_str() {
            "cut" => StingerMissingMediaFallback::Cut,
            "fade" => StingerMissingMediaFallback::Fade,
            "keep_program" => StingerMissingMediaFallback::KeepProgram,
            _ => {
                return Err(CodecError::InvalidField {
                    field: "missing_media_fallback",
                    value: missing_media_fallback,
                });
            }
        },
    })
}

fn decode_stinger_slot(fields: &mut Fields) -> Result<WireStingerSlotId, CodecError> {
    let number: u8 = fields.parse_required("slot")?;
    WireStingerSlotId::new(number).ok_or(CodecError::InvalidField {
        field: "slot",
        value: number.to_string(),
    })
}

fn decode_stream_command(fields: &mut Fields, name: &str) -> Result<CommandPayload, CodecError> {
    let target = decode_stream_target(fields)?;
    Ok(match name {
        "stream_start" => CommandPayload::StreamStart { target },
        "stream_stop" => CommandPayload::StreamStop { target },
        _ => unreachable!("only stream command names are delegated"),
    })
}

fn decode_record_command(fields: &mut Fields, name: &str) -> Result<CommandPayload, CodecError> {
    let recorder = decode_recorder_slot(fields)?;
    Ok(match name {
        "record_start" => CommandPayload::RecordStart { recorder },
        "record_stop" => CommandPayload::RecordStop { recorder },
        _ => unreachable!("only record command names are delegated"),
    })
}

fn decode_recorder_slot(fields: &mut Fields) -> Result<WireRecorderSlot, CodecError> {
    let number: u8 = fields.parse_required("recorder")?;
    WireRecorderSlot::new(number).ok_or(CodecError::InvalidField {
        field: "recorder",
        value: number.to_string(),
    })
}

fn decode_result(fields: &mut Fields) -> Result<CommandResult, CodecError> {
    let status = fields.required("status")?;
    match status.as_str() {
        "accepted" => Ok(CommandResult::Accepted {
            id: fields.required("id")?,
            revision: fields.parse_required("revision")?,
            scheduled_frame: fields.parse_optional("scheduled_frame")?,
        }),
        "rejected" => Ok(CommandResult::Rejected {
            id: fields.required("id")?,
            code: fields.required("code")?,
            message: fields.required("message")?,
            fields: parse_field_issues(&fields.required("fields")?)?,
            current_revision: fields.parse_required("current_revision")?,
            retryable: fields.boolean("retryable")?,
        }),
        _ => Err(CodecError::InvalidField {
            field: "status",
            value: status,
        }),
    }
}

fn decode_snapshot(fields: &mut Fields) -> Result<SnapshotMessage, CodecError> {
    let inputs = parse_input_statuses(&fields.required("inputs")?)?;
    let revision = fields.parse_required("revision")?;
    let edit_locks = decode_edit_locks(fields, &inputs)?;
    let edit_history = decode_edit_history(fields, revision)?;
    let titles = decode_title_summaries(
        &fields.required("titles")?,
        &inputs.iter().map(|input| input.input).collect::<Vec<_>>(),
    )?;
    let scenes = decode_scene_list(
        &fields.required("scenes")?,
        &inputs.iter().map(|input| input.input).collect::<Vec<_>>(),
    )?;
    let frame_rate = decode_frame_rate(fields.required("frame_rate")?)?;
    let playlist_follows = fields.boolean("playlist")?;
    let grade_states = {
        let value = fields.required("grade_states")?;
        parse_canonical_u64(&value)
            .and_then(|count| u16::try_from(count).ok())
            .filter(|count| *count <= MAX_GRADE_STATES)
            .ok_or(CodecError::InvalidField {
                field: "grade_states",
                value,
            })?
    };
    Ok(SnapshotMessage {
        engine: decode_identity(fields)?,
        revision,
        show_name: fields.required("show_name")?,
        inputs,
        outputs: parse_output_statuses(&fields.required("outputs")?)?,
        input_audio_strips: decode_input_audio_strips(fields)?,
        desired_program: fields.input("desired_program")?,
        desired_preview: fields.input("desired_preview")?,
        realized_program: fields.input("realized_program")?,
        realized_preview: fields.input("realized_preview")?,
        desired_manual_transition: decode_manual_status(fields, ManualStatusFields::Desired)?,
        realized_manual_transition: decode_manual_status(fields, ManualStatusFields::Realized)?,
        desired_fade_to_black: decode_fade_to_black_state(fields, FadeToBlackStateFields::Desired)?,
        realized_fade_to_black: decode_fade_to_black_state(
            fields,
            FadeToBlackStateFields::Realized,
        )?,
        stingers: decode_stingers(fields)?,
        streams: parse_stream_statuses(&fields.required("streams")?)?,
        recordings: parse_recording_statuses(&fields.required("recordings")?)?,
        desired_overlays: decode_overlays(fields, "desired_overlays")?,
        realized_overlays: decode_overlays(fields, "realized_overlays")?,
        edit_locks,
        edit_history,
        titles,
        scenes,
        frame_rate,
        playlist_follows,
        grade_states,
    })
}

/// Decodes `library=<id>` or `grade=<grade>`: exactly one of them, or, when
/// `optional`, neither.
fn decode_grade_source(
    fields: &mut Fields,
    optional: bool,
) -> Result<Option<WireGradeSource>, CodecError> {
    match (fields.optional("library"), fields.optional("grade")) {
        (Some(library), None) => Ok(Some(WireGradeSource::Library(parse_grade_id(
            "library", &library,
        )?))),
        (None, Some(grade)) => Ok(Some(WireGradeSource::Inline(decode_grade(&grade)?))),
        (None, None) if optional => Ok(None),
        (None, None) => Err(CodecError::MissingField("grade")),
        (Some(_), Some(grade)) => Err(CodecError::InvalidField {
            field: "grade",
            value: grade,
        }),
    }
}

fn decode_required_grade_source(fields: &mut Fields) -> Result<WireGradeSource, CodecError> {
    decode_grade_source(fields, false)?.ok_or(CodecError::MissingField("grade"))
}

fn decode_grade_name(fields: &mut Fields) -> Result<String, CodecError> {
    let name = fields.required("name")?;
    validate_grade_name(&name)?;
    Ok(name)
}

fn decode_list_index(value: String) -> Result<u16, CodecError> {
    parse_canonical_u64(&value)
        .and_then(|index| u16::try_from(index).ok())
        .filter(|index| usize::from(*index) < MAX_LIST_ITEMS)
        .ok_or(CodecError::InvalidField {
            field: "index",
            value,
        })
}

/// Decodes the LUT id, name, kind, and size of an imported LUT.
fn decode_lut_metadata(
    fields: &mut Fields,
) -> Result<(core::num::NonZeroU32, String, fm_grade::CubeKind, u32), CodecError> {
    let lut = parse_grade_id("lut_id", &fields.required("lut_id")?)?;
    let name = decode_grade_name(fields)?;
    let kind = decode_cube_kind(&fields.required("kind")?)?;
    let size = {
        let value = fields.required("size")?;
        parse_canonical_u64(&value)
            .and_then(|size| u32::try_from(size).ok())
            .ok_or(CodecError::InvalidField {
                field: "size",
                value,
            })?
    };
    validate_lut_size(kind, size)?;
    Ok((lut, name, kind, size))
}

/// Decodes the grade commands (Protocol 2.28).
fn decode_grade_command(fields: &mut Fields, name: &str) -> Result<CommandPayload, CodecError> {
    Ok(match name {
        "grade_input_set" => CommandPayload::SetInputGrade {
            input: fields.canonical_input("input")?,
            source: Some(decode_required_grade_source(fields)?),
        },
        "grade_input_clear" => CommandPayload::SetInputGrade {
            input: fields.canonical_input("input")?,
            source: None,
        },
        "grade_program_set" => CommandPayload::SetProgramGrade {
            source: Some(decode_required_grade_source(fields)?),
        },
        "grade_program_clear" => CommandPayload::SetProgramGrade { source: None },
        "grade_library_add" => CommandPayload::AddLibraryGrade {
            grade: parse_grade_id("grade_id", &fields.required("grade_id")?)?,
            name: decode_grade_name(fields)?,
            value: decode_grade(&fields.required("grade")?)?,
            index: fields
                .optional("index")
                .map(decode_list_index)
                .transpose()?,
        },
        "grade_library_update" => CommandPayload::UpdateLibraryGrade {
            grade: parse_grade_id("grade_id", &fields.required("grade_id")?)?,
            name: decode_grade_name(fields)?,
            value: decode_grade(&fields.required("grade")?)?,
        },
        "grade_library_remove" => CommandPayload::RemoveLibraryGrade {
            grade: parse_grade_id("grade_id", &fields.required("grade_id")?)?,
        },
        "grade_lut_import" => CommandPayload::ImportGradeLut {
            lut: parse_grade_id("lut_id", &fields.required("lut_id")?)?,
            name: decode_grade_name(fields)?,
            data: decode_lut_data(&fields.required("data")?)?,
        },
        "grade_lut_remove" => CommandPayload::RemoveGradeLut {
            lut: parse_grade_id("lut_id", &fields.required("lut_id")?)?,
        },
        _ => {
            return Err(CodecError::InvalidField {
                field: "payload",
                value: name.to_owned(),
            });
        }
    })
}

fn decode_grade_state(fields: &mut Fields) -> Result<GradeStateMessage, CodecError> {
    let revision = fields.canonical_required("revision")?;
    let entry = fields.required("entry")?;
    let entry = match entry.as_str() {
        "lut" => {
            let (lut, name, kind, size) = decode_lut_metadata(fields)?;
            GradeEntry::Lut {
                lut,
                name,
                kind,
                size,
            }
        }
        "library" => GradeEntry::Library {
            grade: parse_grade_id("grade_id", &fields.required("grade_id")?)?,
            name: decode_grade_name(fields)?,
            value: decode_grade(&fields.required("grade")?)?,
        },
        "input" => GradeEntry::Input {
            input: fields.canonical_input("input")?,
            source: decode_required_grade_source(fields)?,
        },
        "program" => GradeEntry::Program {
            source: decode_required_grade_source(fields)?,
        },
        _ => {
            return Err(CodecError::InvalidField {
                field: "entry",
                value: entry,
            });
        }
    };
    Ok(GradeStateMessage { revision, entry })
}

fn decode_grade_change(fields: &mut Fields) -> Result<GradeChange, CodecError> {
    let change = fields.required("change")?;
    Ok(match change.as_str() {
        "lut_imported" => {
            let (lut, name, kind, size) = decode_lut_metadata(fields)?;
            GradeChange::LutImported {
                lut,
                name,
                kind,
                size,
            }
        }
        "lut_removed" => GradeChange::LutRemoved {
            lut: parse_grade_id("lut_id", &fields.required("lut_id")?)?,
        },
        "library" => GradeChange::Library {
            grade: parse_grade_id("grade_id", &fields.required("grade_id")?)?,
            index: decode_list_index(fields.required("index")?)?,
            name: decode_grade_name(fields)?,
            value: decode_grade(&fields.required("grade")?)?,
        },
        "library_removed" => GradeChange::LibraryRemoved {
            grade: parse_grade_id("grade_id", &fields.required("grade_id")?)?,
        },
        "input" => GradeChange::Input {
            input: fields.canonical_input("input")?,
            source: decode_grade_source(fields, true)?,
        },
        "program" => GradeChange::Program {
            source: decode_grade_source(fields, true)?,
        },
        _ => {
            return Err(CodecError::InvalidField {
                field: "change",
                value: change,
            });
        }
    })
}

fn decode_playlist_state_record(fields: &mut Fields) -> Result<PlaylistStateMessage, CodecError> {
    Ok(PlaylistStateMessage {
        revision: fields.canonical_required("revision")?,
        frame: fields.canonical_required("frame")?,
        loop_mode: parse_playlist_loop(fields.required("loop")?)?,
        entries: decode_playlist_entries(&fields.required("entries")?)?,
        state: decode_playlist_state(fields.required("state")?)?,
    })
}

fn decode_input_audio_strips(
    fields: &mut Fields,
) -> Result<Vec<InputAudioStripStatus>, CodecError> {
    let value = fields.required("input_audio_strips")?;
    if value.is_empty() || value.split(',').take(MAX_LIST_ITEMS + 1).count() > MAX_LIST_ITEMS {
        return Err(CodecError::InvalidField {
            field: "input_audio_strips",
            value,
        });
    }
    let mut inputs = BTreeSet::new();
    value
        .split(',')
        .map(|entry| {
            let invalid = || CodecError::InvalidField {
                field: "input_audio_strips",
                value: entry.to_owned(),
            };
            let mut parts = entry.split(':');
            let input = parts.next().ok_or_else(invalid)?;
            let gain_millidb = parts.next().ok_or_else(invalid)?;
            let balance_basis_points = parts.next().ok_or_else(invalid)?;
            let muted = parts.next().ok_or_else(invalid)?;
            let soloed = parts.next().ok_or_else(invalid)?;
            let follow_video = parts.next().ok_or_else(invalid)?;
            let delay_samples = parts.next().ok_or_else(invalid)?;
            if parts.next().is_some() {
                return Err(invalid());
            }
            let status = InputAudioStripStatus {
                input: parse_input(input).ok_or_else(invalid)?,
                gain_millidb: gain_millidb.parse().map_err(|_| invalid())?,
                balance_basis_points: balance_basis_points.parse().map_err(|_| invalid())?,
                muted: match muted {
                    "0" => false,
                    "1" => true,
                    _ => return Err(invalid()),
                },
                soloed: match soloed {
                    "0" => false,
                    "1" => true,
                    _ => return Err(invalid()),
                },
                follow_video: match follow_video {
                    "0" => false,
                    "1" => true,
                    _ => return Err(invalid()),
                },
                delay_samples: delay_samples.parse().map_err(|_| invalid())?,
            };
            if !(-96_000..=24_000).contains(&status.gain_millidb)
                || !(-10_000..=10_000).contains(&status.balance_basis_points)
                || status.delay_samples > 48_000
                || !inputs.insert(status.input.get())
            {
                return Err(CodecError::InvalidField {
                    field: "input_audio_strips",
                    value: entry.to_owned(),
                });
            }
            Ok(status)
        })
        .collect()
}

fn decode_overlays(
    fields: &mut Fields,
    field: &'static str,
) -> Result<Vec<OverlayStatus>, CodecError> {
    let value = fields.required(field)?;
    let entries = value.split(';').collect::<Vec<_>>();
    if entries.len() != 8 {
        return Err(CodecError::InvalidField { field, value });
    }
    let mut seen = [false; 8];
    entries
        .into_iter()
        .map(|entry| decode_overlay_status(entry, field, &mut seen))
        .collect()
}

fn decode_overlay_status(
    entry: &str,
    field: &'static str,
    seen: &mut [bool; 8],
) -> Result<OverlayStatus, CodecError> {
    let parts = entry.split(':').collect::<Vec<_>>();
    let [
        channel,
        source,
        active,
        opacity,
        transition,
        duration_frames,
        position,
        border,
        queue,
        outputs,
    ] = parts.as_slice()
    else {
        return Err(CodecError::InvalidField {
            field,
            value: entry.to_owned(),
        });
    };
    let channel_number: u8 = channel.parse().map_err(|_| CodecError::InvalidField {
        field,
        value: entry.to_owned(),
    })?;
    let channel =
        WireOverlayChannelId::new(channel_number).ok_or_else(|| CodecError::InvalidField {
            field,
            value: entry.to_owned(),
        })?;
    let index = usize::from(channel_number - 1);
    if seen[index] {
        return Err(CodecError::InvalidField {
            field,
            value: entry.to_owned(),
        });
    }
    seen[index] = true;
    let has_source = *source != "0";
    let source = if has_source {
        parse_input(source)
    } else {
        None
    };
    if has_source && source.is_none() {
        return Err(CodecError::InvalidField {
            field,
            value: entry.to_owned(),
        });
    }
    let active = match *active {
        "0" => false,
        "1" => true,
        _ => {
            return Err(CodecError::InvalidField {
                field,
                value: entry.to_owned(),
            });
        }
    };
    let opacity = opacity.parse().map_err(|_| CodecError::InvalidField {
        field,
        value: entry.to_owned(),
    })?;
    let transition = decode_overlay_transition((*transition).to_owned(), field)?;
    let duration_frames = duration_frames
        .parse()
        .map_err(|_| CodecError::InvalidField {
            field,
            value: entry.to_owned(),
        })?;
    validate_overlay_duration(duration_frames, field)?;
    let position = decode_overlay_position((*position).to_owned(), field)?;
    let border = decode_overlay_border((*border).to_owned(), field)?;
    let queued_sources = decode_overlay_queue(queue, entry, field)?;
    let included_outputs = decode_overlay_included_outputs(outputs, entry, field)?;
    Ok(OverlayStatus {
        channel,
        source,
        active,
        opacity,
        transition,
        duration_frames,
        position,
        border,
        queued_sources,
        included_outputs,
    })
}

fn decode_overlay_included_outputs(
    outputs: &str,
    entry: &str,
    field: &'static str,
) -> Result<Vec<WireOutputId>, CodecError> {
    if outputs.is_empty() {
        return Ok(Vec::new());
    }
    let included_outputs = outputs
        .split(',')
        .map(|output| NonZeroU128::new(output.parse().ok()?).map(WireOutputId::new))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| CodecError::InvalidField {
            field,
            value: entry.to_owned(),
        })?;
    let mut output_ids = BTreeSet::new();
    for output in &included_outputs {
        if !output_ids.insert(output.to_domain().get()) {
            return Err(CodecError::InvalidField {
                field,
                value: entry.to_owned(),
            });
        }
    }
    Ok(included_outputs)
}

fn decode_overlay_queue(
    value: &str,
    entry: &str,
    field: &'static str,
) -> Result<Vec<WireInputId>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let values = value
        .split(',')
        .map(parse_input)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| CodecError::InvalidField {
            field,
            value: entry.to_owned(),
        })?;
    if values.len() > 64 {
        return Err(CodecError::TooManyItems("overlay queue"));
    }
    Ok(values)
}

fn decode_stingers(fields: &mut Fields) -> Result<Vec<StingerStatus>, CodecError> {
    let value = fields.required("stingers")?;
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<_> = value.split(',').collect();
    if entries.len() > 8 {
        return Err(CodecError::TooManyItems("stingers"));
    }
    let mut seen = [false; 8];
    let mut stingers = Vec::with_capacity(entries.len());
    for entry in entries {
        let status = decode_stinger_status(entry).ok_or_else(|| invalid_stingers(&value))?;
        let slot_index = usize::from(status.slot.number() - 1);
        if seen[slot_index] {
            return Err(invalid_stingers(&value));
        }
        seen[slot_index] = true;
        stingers.push(status);
    }
    Ok(stingers)
}

fn decode_stinger_status(entry: &str) -> Option<StingerStatus> {
    let parts: Vec<_> = entry.split(':').collect();
    let [
        slot,
        media_input,
        preload,
        cut_point_frames,
        audio_policy,
        fallback,
        readiness,
    ] = parts.as_slice()
    else {
        return None;
    };
    Some(StingerStatus {
        slot: slot.parse().ok().and_then(WireStingerSlotId::new)?,
        media_input: parse_input(media_input)?,
        preload: match *preload {
            "0" => false,
            "1" => true,
            _ => return None,
        },
        cut_point_frames: cut_point_frames.parse().ok()?,
        audio_policy: match *audio_policy {
            "muted" => StingerAudioPolicy::Muted,
            "stinger_only" => StingerAudioPolicy::StingerOnly,
            "mix_with_program" => StingerAudioPolicy::MixWithProgram,
            _ => return None,
        },
        missing_media_fallback: match *fallback {
            "cut" => StingerMissingMediaFallback::Cut,
            "fade" => StingerMissingMediaFallback::Fade,
            "keep_program" => StingerMissingMediaFallback::KeepProgram,
            _ => return None,
        },
        readiness: match *readiness {
            "not_requested" => StingerReadiness::NotRequested,
            "ready" => StingerReadiness::Ready,
            "missing" => StingerReadiness::Missing,
            _ => return None,
        },
    })
}

fn invalid_stingers(value: &str) -> CodecError {
    CodecError::InvalidField {
        field: "stingers",
        value: value.to_owned(),
    }
}

fn decode_event(fields: &mut Fields) -> Result<EventMessage, CodecError> {
    let event = fields.required("event")?;
    let payload = match event.as_str() {
        "input_order_changed" => EventPayload::InputOrderChanged {
            inputs: parse_input_ids(&fields.required("inputs")?)?,
        },
        "input_renamed" => EventPayload::InputRenamed {
            input: fields.input("input")?,
            name: fields.required("name")?,
        },
        "input_added" => EventPayload::InputAdded {
            input: fields.canonical_input("input")?,
            name: decode_new_input_name(fields)?,
        },
        "input_removed" => EventPayload::InputRemoved {
            input: fields.canonical_input("input")?,
        },
        "desired_switcher" => EventPayload::DesiredSwitcher {
            program: fields.input("program")?,
            preview: fields.input("preview")?,
            manual_transition: decode_manual_status(fields, ManualStatusFields::Unqualified)?,
            fade_to_black: decode_fade_to_black_state(fields, FadeToBlackStateFields::Unqualified)?,
            overlays: decode_overlays(fields, "overlays")?,
            input_audio_strips: decode_input_audio_strips(fields)?,
        },
        "stinger_slots_changed" => EventPayload::StingerSlotsChanged {
            program: fields.input("program")?,
            preview: fields.input("preview")?,
            manual_transition: decode_manual_status(fields, ManualStatusFields::Unqualified)?,
            fade_to_black: decode_fade_to_black_state(fields, FadeToBlackStateFields::Unqualified)?,
            stingers: decode_stingers(fields)?,
            overlays: decode_overlays(fields, "overlays")?,
            input_audio_strips: decode_input_audio_strips(fields)?,
        },
        "streams_changed" => EventPayload::StreamsChanged {
            streams: parse_stream_statuses(&fields.required("streams")?)?,
        },
        "recording_changed" => EventPayload::RecordingChanged {
            recorder: decode_recorder_slot(fields)?,
            active: fields.boolean("active")?,
        },
        "edit_lock_changed" => EventPayload::EditLockChanged {
            scope: decode_edit_lock_scope(fields)?,
            locked: fields.boolean("locked")?,
        },
        "title_changed" => EventPayload::TitleChanged {
            input: fields.canonical_input("input")?,
            field_revision: fields.canonical_required("field_revision")?,
            fields: decode_title_fields(&fields.required("fields")?)?,
            overlays: decode_overlays(fields, "overlays")?,
        },
        "playlist_changed" => EventPayload::PlaylistChanged {
            frame: fields.canonical_required("frame")?,
            loop_mode: parse_playlist_loop(fields.required("loop")?)?,
            entries: decode_playlist_entries(&fields.required("entries")?)?,
            state: decode_playlist_state(fields.required("state")?)?,
            program: fields.canonical_input("program")?,
            preview: fields.canonical_input("preview")?,
        },
        "grade_changed" => EventPayload::GradeChanged(decode_grade_change(fields)?),
        "title_animation_changed" => {
            let inputs = parse_input_ids(&fields.required("inputs")?)?;
            validate_animation_inputs(&inputs)?;
            EventPayload::TitleAnimationChanged {
                inputs,
                animation_revision: fields.canonical_required("animation_revision")?,
                clip: Box::new(decode_clip(&fields.required("clip")?)?),
            }
        }
        "scene_animation_changed" => {
            let inputs = parse_input_ids(&fields.required("inputs")?)?;
            validate_animation_inputs(&inputs)?;
            EventPayload::SceneAnimationChanged {
                inputs,
                animation_revision: fields.canonical_required("animation_revision")?,
                clip: Box::new(decode_scene_clip(&fields.required("clip")?)?),
            }
        }
        _ => {
            return Err(CodecError::InvalidField {
                field: "event",
                value: event,
            });
        }
    };
    let cursor = decode_cursor(fields)?;
    let edit_history = decode_edit_history(fields, cursor.revision)?;
    Ok(EventMessage {
        cursor,
        payload,
        edit_history,
    })
}

fn parse_input(value: &str) -> Option<WireInputId> {
    Some(WireInputId::new(NonZeroU128::new(value.parse().ok()?)?))
}

fn decode_server_identity(fields: &mut Fields) -> Result<ServerIdentity, CodecError> {
    Ok(ServerIdentity {
        engine_id: fields.required("engine_id")?,
        project_id: fields.required("project_id")?,
        state_epoch: fields.parse_required("state_epoch")?,
        log_id: fields.required("log_id")?,
    })
}

fn decode_resume_cursor(
    fields: &mut Fields,
    prefix: &'static str,
) -> Result<ResumeCursor, CodecError> {
    let (engine, project, epoch, log, revision) = match prefix {
        "resume" => (
            "resume_engine_id",
            "resume_project_id",
            "resume_state_epoch",
            "resume_log_id",
            "resume_revision",
        ),
        "applied" => (
            "applied_engine_id",
            "applied_project_id",
            "applied_state_epoch",
            "applied_log_id",
            "applied_revision",
        ),
        _ => unreachable!("cursor prefixes are fixed by the wire schema"),
    };
    Ok(ResumeCursor {
        server: ServerIdentity {
            engine_id: fields.required(engine)?,
            project_id: fields.required(project)?,
            state_epoch: fields.parse_required(epoch)?,
            log_id: fields.required(log)?,
        },
        revision: fields.parse_required(revision)?,
    })
}

fn decode_handshake_request(fields: &mut Fields) -> Result<HandshakeRequest, CodecError> {
    let protocol_value = fields.required("protocol")?;
    let protocol = parse_version(&protocol_value).ok_or(CodecError::InvalidField {
        field: "protocol",
        value: protocol_value,
    })?;
    let client_type_value = fields.required("client_type")?;
    let client_type = parse_client_type(&client_type_value).ok_or(CodecError::InvalidField {
        field: "client_type",
        value: client_type_value,
    })?;
    let role_value = fields.required("desired_role")?;
    let desired_role = parse_role(&role_value).ok_or(CodecError::InvalidField {
        field: "desired_role",
        value: role_value,
    })?;
    let resume_cursor = if fields.boolean("resume")? {
        Some(decode_resume_cursor(fields, "resume")?)
    } else {
        None
    };
    Ok(HandshakeRequest {
        protocol,
        build: fields.required("build")?,
        client_type,
        desired_role,
        resume_cursor,
    })
}

fn decode_handshake_response(fields: &mut Fields) -> Result<HandshakeResponse, CodecError> {
    let protocol_value = fields.required("protocol")?;
    let protocol = parse_version(&protocol_value).ok_or(CodecError::InvalidField {
        field: "protocol",
        value: protocol_value,
    })?;
    let role_value = fields.required("granted_role")?;
    let granted_role = parse_role(&role_value).ok_or(CodecError::InvalidField {
        field: "granted_role",
        value: role_value,
    })?;
    let permissions = parse_string_list(&fields.required("permissions")?)?;
    let capabilities = decode_capability_summary(fields)?;
    let server = decode_server_identity(fields)?;
    let current_revision = fields.parse_required("current_revision")?;
    let outcome_value = fields.required("outcome")?;
    let outcome = match outcome_value.as_str() {
        "snapshot" => {
            let reason_value = fields.required("snapshot_reason")?;
            let reason = parse_snapshot_reason(&reason_value).ok_or(CodecError::InvalidField {
                field: "snapshot_reason",
                value: reason_value,
            })?;
            HandshakeOutcome::Snapshot { reason }
        }
        "resume" => {
            let cursor = decode_resume_cursor(fields, "resume")?;
            if cursor.server != server || cursor.revision > current_revision {
                return Err(CodecError::InvalidField {
                    field: "resume_revision",
                    value: cursor.revision.to_string(),
                });
            }
            HandshakeOutcome::Resume { cursor }
        }
        "rejected" => HandshakeOutcome::Rejected {
            error: decode_structured_error(fields)?,
        },
        _ => {
            return Err(CodecError::InvalidField {
                field: "outcome",
                value: outcome_value,
            });
        }
    };
    Ok(HandshakeResponse {
        protocol,
        granted_role,
        permissions,
        capabilities,
        server,
        current_revision,
        outcome,
    })
}

fn decode_durable_batch(fields: &mut Fields) -> Result<DurableEventBatch, CodecError> {
    let server = decode_server_identity(fields)?;
    let revision = fields.parse_required("revision")?;
    let events_value = fields.required("events")?;
    let events = parse_durable_events(&events_value)?;
    for (expected, event) in events.iter().enumerate() {
        if usize::from(event.sequence) != expected {
            return Err(CodecError::InvalidField {
                field: "events",
                value: events_value,
            });
        }
    }
    Ok(DurableEventBatch {
        cursor: ResumeCursor { server, revision },
        events,
    })
}

fn decode_durable_gap(fields: &mut Fields) -> Result<DurableGap, CodecError> {
    let gap = DurableGap {
        server: decode_server_identity(fields)?,
        requested_after_revision: fields.parse_required("requested_after_revision")?,
        available_from_revision: fields.parse_required("available_from_revision")?,
        current_revision: fields.parse_required("current_revision")?,
    };
    if gap.requested_after_revision.saturating_add(1) >= gap.available_from_revision
        || gap.available_from_revision > gap.current_revision
    {
        return Err(CodecError::InvalidField {
            field: "available_from_revision",
            value: gap.available_from_revision.to_string(),
        });
    }
    Ok(gap)
}

fn decode_runtime_event(fields: &mut Fields) -> Result<RuntimeEventMessage, CodecError> {
    let server = decode_server_identity(fields)?;
    let revision = fields.parse_required("revision")?;
    let generation = fields.parse_required("generation")?;
    let sequence = fields.parse_required("sequence")?;
    let event_value = fields.required("event")?;
    let event = match event_value.as_str() {
        "accepted" => RuntimeLifecycleEvent::Accepted,
        "preparing" => RuntimeLifecycleEvent::Preparing,
        "scheduled" => RuntimeLifecycleEvent::Scheduled {
            domains: parse_runtime_domains(&fields.required("domains")?)?,
        },
        "realized" => RuntimeLifecycleEvent::Realized {
            domain: fields.required("domain")?,
            manual_transition: decode_manual_status(fields, ManualStatusFields::Unqualified)?,
            fade_to_black: decode_fade_to_black_state(fields, FadeToBlackStateFields::Unqualified)?,
        },
        "failed" => {
            let disposition_value = fields.required("disposition")?;
            let disposition =
                parse_failure_disposition(&disposition_value).ok_or(CodecError::InvalidField {
                    field: "disposition",
                    value: disposition_value,
                })?;
            RuntimeLifecycleEvent::Failed {
                error: decode_structured_error(fields)?,
                disposition,
            }
        }
        "superseded" => RuntimeLifecycleEvent::Superseded {
            by_revision: fields.parse_required("by_revision")?,
        },
        _ => {
            return Err(CodecError::InvalidField {
                field: "event",
                value: event_value,
            });
        }
    };
    Ok(RuntimeEventMessage {
        server,
        revision,
        generation,
        sequence,
        event,
    })
}

#[derive(Clone, Copy)]
enum ManualStatusFields {
    Desired,
    Realized,
    Unqualified,
}

fn decode_manual_status(
    fields: &mut Fields,
    names: ManualStatusFields,
) -> Result<ManualTransitionStatus, CodecError> {
    let (active, kind, from, to, interval_start, position) = match names {
        ManualStatusFields::Desired => (
            "desired_manual_active",
            "desired_manual_kind",
            "desired_manual_from",
            "desired_manual_to",
            "desired_manual_interval_start_basis_points",
            "desired_manual_position_basis_points",
        ),
        ManualStatusFields::Realized => (
            "realized_manual_active",
            "realized_manual_kind",
            "realized_manual_from",
            "realized_manual_to",
            "realized_manual_interval_start_basis_points",
            "realized_manual_position_basis_points",
        ),
        ManualStatusFields::Unqualified => (
            "manual_active",
            "manual_kind",
            "manual_from",
            "manual_to",
            "manual_interval_start_basis_points",
            "manual_position_basis_points",
        ),
    };
    let active_value = fields.boolean(active)?;
    if !active_value {
        return Ok(ManualTransitionStatus::Inactive);
    }
    let kind_value = fields.required(kind)?;
    let kind = match kind_value.as_str() {
        "fade" => ManualTransitionKind::Fade,
        "wipe" => ManualTransitionKind::Wipe,
        "alpha_fade" => ManualTransitionKind::AlphaFade,
        "slide" => ManualTransitionKind::Slide,
        _ => {
            return Err(CodecError::InvalidField {
                field: kind,
                value: kind_value,
            });
        }
    };
    let from_value = fields.required(from)?;
    let from_input = parse_input(&from_value).ok_or(CodecError::InvalidField {
        field: from,
        value: from_value,
    })?;
    let to_value = fields.required(to)?;
    let to_input = parse_input(&to_value).ok_or(CodecError::InvalidField {
        field: to,
        value: to_value,
    })?;
    let interval_value: u16 = fields.parse_required(interval_start)?;
    let interval_start_position =
        ManualTransitionPosition::new(interval_value).ok_or(CodecError::InvalidField {
            field: interval_start,
            value: interval_value.to_string(),
        })?;
    let position_value: u16 = fields.parse_required(position)?;
    let exact_position =
        ManualTransitionPosition::new(position_value).ok_or(CodecError::InvalidField {
            field: position,
            value: position_value.to_string(),
        })?;
    Ok(ManualTransitionStatus::Active(ManualTransitionState {
        kind,
        from: from_input,
        to: to_input,
        interval_start: interval_start_position,
        position: exact_position,
    }))
}

#[derive(Clone, Copy)]
enum FadeToBlackStateFields {
    Desired,
    Realized,
    Unqualified,
}

fn decode_fade_to_black_state(
    fields: &mut Fields,
    names: FadeToBlackStateFields,
) -> Result<FadeToBlackState, CodecError> {
    let (target_active, position) = match names {
        FadeToBlackStateFields::Desired => (
            "desired_ftb_target_active",
            "desired_ftb_position_numerator",
        ),
        FadeToBlackStateFields::Realized => (
            "realized_ftb_target_active",
            "realized_ftb_position_numerator",
        ),
        FadeToBlackStateFields::Unqualified => ("ftb_target_active", "ftb_position_numerator"),
    };
    Ok(FadeToBlackState {
        target_active: fields.boolean(target_active)?,
        position: FadeToBlackPosition::new(fields.parse_required(position)?),
    })
}

fn decode_heartbeat(fields: &mut Fields) -> Result<HeartbeatMessage, CodecError> {
    let server = decode_server_identity(fields)?;
    let sequence = fields.parse_required("sequence")?;
    let sent_at_ms = fields.parse_required("sent_at_ms")?;
    let last_applied = if fields.boolean("applied")? {
        let cursor = decode_resume_cursor(fields, "applied")?;
        if cursor.server != server {
            return Err(CodecError::InvalidField {
                field: "applied_engine_id",
                value: cursor.server.engine_id,
            });
        }
        Some(cursor)
    } else {
        None
    };
    Ok(HeartbeatMessage {
        server,
        sequence,
        sent_at_ms,
        last_applied,
    })
}

fn decode_heartbeat_acknowledgement(
    fields: &mut Fields,
) -> Result<HeartbeatAcknowledgementMessage, CodecError> {
    Ok(HeartbeatAcknowledgementMessage {
        server: decode_server_identity(fields)?,
        heartbeat_sequence: fields.parse_required("heartbeat_sequence")?,
        received_at_ms: fields.parse_required("received_at_ms")?,
    })
}

fn decode_stream_status(fields: &mut Fields) -> Result<StreamStatusMessage, CodecError> {
    let server = decode_server_identity(fields)?;
    let sequence = fields.parse_required("sequence")?;
    let samples_value = fields.required("samples")?;
    if samples_value.is_empty() {
        return Err(CodecError::InvalidField {
            field: "samples",
            value: samples_value,
        });
    }
    let entries: Vec<_> = samples_value.split(';').collect();
    if entries.len() > MAX_STREAM_SAMPLES {
        return Err(CodecError::TooManyItems("samples"));
    }
    let mut previous = None;
    let samples = entries
        .into_iter()
        .map(|entry| decode_stream_status_sample(entry, &mut previous))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StreamStatusMessage {
        server,
        sequence,
        samples,
    })
}

/// The seven required colon-separated `stream_status` sample fields plus the
/// optional trailing `local=<segments>:<target_duration_ms>:<free_bytes>`
/// triple, split from one sample entry.
struct StreamSampleFields<'a> {
    target: &'a str,
    realized: &'a str,
    connected: &'a str,
    muxed_bytes: &'a str,
    enqueued_pairs: &'a str,
    dropped_pairs: &'a str,
    failure: &'a str,
    local: Option<(&'a str, &'a str, &'a str)>,
}

/// Splits one sample entry into its fields, refusing any shape other than the
/// exact 2.18 seven-field form or that form plus the local triple.
fn split_stream_status_sample(entry: &str) -> Option<StreamSampleFields<'_>> {
    let parts = entry.split(':').collect::<Vec<_>>();
    match parts.as_slice() {
        [
            target,
            realized,
            connected,
            muxed_bytes,
            enqueued_pairs,
            dropped_pairs,
            failure,
        ] => Some(StreamSampleFields {
            target,
            realized,
            connected,
            muxed_bytes,
            enqueued_pairs,
            dropped_pairs,
            failure,
            local: None,
        }),
        [
            target,
            realized,
            connected,
            muxed_bytes,
            enqueued_pairs,
            dropped_pairs,
            failure,
            local_segments,
            local_target_duration_ms,
            local_free_bytes,
        ] => Some(StreamSampleFields {
            target,
            realized,
            connected,
            muxed_bytes,
            enqueued_pairs,
            dropped_pairs,
            failure,
            local: Some((
                local_segments.strip_prefix("local=")?,
                local_target_duration_ms,
                local_free_bytes,
            )),
        }),
        _ => None,
    }
}

/// Parses one sample's trailing local triple and enforces the wire bounds.
fn parse_stream_local_status(
    entry: &str,
    segments: &str,
    target_duration_ms: &str,
    free_bytes: &str,
) -> Result<StreamLocalStatus, CodecError> {
    let invalid = || CodecError::InvalidField {
        field: "samples",
        value: entry.to_owned(),
    };
    let segments: u32 = segments.parse().map_err(|_| invalid())?;
    let target_duration_ms: u32 = target_duration_ms.parse().map_err(|_| invalid())?;
    let free_bytes: u64 = free_bytes.parse().map_err(|_| invalid())?;
    if segments > MAX_STREAM_LOCAL_SEGMENTS
        || target_duration_ms > MAX_STREAM_LOCAL_TARGET_DURATION_MS
    {
        return Err(invalid());
    }
    Ok(StreamLocalStatus {
        segments,
        target_duration_ms,
        free_bytes,
    })
}

/// Decodes one `;`-separated `stream_status` sample, enforcing the strictly
/// ascending target order through `previous`.
fn decode_stream_status_sample(
    entry: &str,
    previous: &mut Option<NonZeroU128>,
) -> Result<StreamStatusSample, CodecError> {
    let invalid = || CodecError::InvalidField {
        field: "samples",
        value: entry.to_owned(),
    };
    let Some(fields) = split_stream_status_sample(entry) else {
        return Err(invalid());
    };
    let target = fields
        .target
        .parse::<u128>()
        .ok()
        .and_then(NonZeroU128::new)
        .map(WireStreamTargetId::new)
        .ok_or_else(invalid)?;
    if previous.is_some_and(|id| target.get() <= id) {
        return Err(invalid());
    }
    *previous = Some(target.get());
    let realized = parse_stream_realized_state(fields.realized).ok_or_else(invalid)?;
    let connected = match fields.connected {
        "0" => false,
        "1" => true,
        _ => return Err(invalid()),
    };
    let failure = if fields.failure.is_empty() {
        None
    } else {
        let failure = unescape(fields.failure)?;
        if failure.len() > super::MAX_STREAM_DETAIL_BYTES {
            return Err(invalid());
        }
        Some(failure)
    };
    let local = match fields.local {
        Some((segments, target_duration_ms, free_bytes)) => Some(parse_stream_local_status(
            entry,
            segments,
            target_duration_ms,
            free_bytes,
        )?),
        None => None,
    };
    Ok(StreamStatusSample {
        target,
        realized,
        connected,
        muxed_bytes: fields.muxed_bytes.parse().map_err(|_| invalid())?,
        enqueued_pairs: fields.enqueued_pairs.parse().map_err(|_| invalid())?,
        dropped_pairs: fields.dropped_pairs.parse().map_err(|_| invalid())?,
        failure,
        local,
    })
}

fn decode_monitor_frame(fields: &mut Fields) -> Result<MonitorFrameMessage, CodecError> {
    let server = decode_server_identity(fields)?;
    let tap_value = fields.required("tap")?;
    let tap = match tap_value.as_str() {
        "preview" => MonitorTap::Preview,
        "program" => MonitorTap::Program,
        _ => {
            return Err(CodecError::InvalidField {
                field: "tap",
                value: tap_value,
            });
        }
    };
    let sequence = fields.parse_required("sequence")?;
    let frame = fields.parse_required("frame")?;
    let input = fields.input("input")?;
    let origin_value = fields.required("origin")?;
    let origin = match origin_value.as_str() {
        "simulated" => MonitorFrameOrigin::Simulated,
        "native" => MonitorFrameOrigin::Native,
        _ => {
            return Err(CodecError::InvalidField {
                field: "origin",
                value: origin_value,
            });
        }
    };
    let width = fields.parse_required("width")?;
    let height = fields.parse_required("height")?;
    let part = fields.parse_required("part")?;
    let parts = fields.parse_required("parts")?;
    let data_value = fields.required("data")?;
    let encoded_limit = (super::MAX_MONITOR_FRAME_PART_BYTES * 4).div_ceil(3);
    let data = if data_value.len() > encoded_limit {
        None
    } else {
        crate::monitor_image::base64url_decode(&data_value)
    }
    .ok_or_else(|| CodecError::InvalidField {
        field: "data",
        value: format!("{} characters", data_value.len()),
    })?;
    super::validate_monitor_frame(width, height, part, parts, data.len())?;
    Ok(MonitorFrameMessage {
        server,
        tap,
        sequence,
        frame,
        input,
        origin,
        width,
        height,
        part,
        parts,
        data,
    })
}

fn decode_audio_meters(fields: &mut Fields) -> Result<AudioMetersMessage, CodecError> {
    let server = decode_server_identity(fields)?;
    let sequence = fields.parse_required("sequence")?;
    let frame = fields.parse_required("frame")?;
    let start_sample: u64 = fields.parse_required("start_sample")?;
    let end_sample: u64 = fields.parse_required("end_sample")?;
    let master_value = fields.required("master")?;
    let master = decode_meter_channels(&master_value, "master")?;
    if !(1..=MAX_AUDIO_METER_CHANNELS).contains(&master.len()) {
        return Err(CodecError::InvalidField {
            field: "master",
            value: master_value,
        });
    }
    if end_sample <= start_sample {
        return Err(CodecError::InvalidField {
            field: "end_sample",
            value: end_sample.to_string(),
        });
    }
    let inputs_value = fields.required("inputs")?;
    let inputs = if inputs_value.is_empty() {
        Vec::new()
    } else {
        let entries: Vec<_> = inputs_value.split(';').collect();
        if entries.len() > MAX_LIST_ITEMS {
            return Err(CodecError::TooManyItems("inputs"));
        }
        let mut previous = None;
        entries
            .into_iter()
            .map(|entry| {
                let (input, channels) =
                    entry
                        .split_once('~')
                        .ok_or_else(|| CodecError::InvalidField {
                            field: "inputs",
                            value: inputs_value.clone(),
                        })?;
                let input = input
                    .parse::<u128>()
                    .ok()
                    .and_then(core::num::NonZeroU128::new)
                    .map(WireInputId::new)
                    .ok_or_else(|| CodecError::InvalidField {
                        field: "inputs",
                        value: inputs_value.clone(),
                    })?;
                let channels = decode_meter_channels(channels, "inputs")?;
                if channels.len() != master.len() || previous.is_some_and(|id| input.get() <= id) {
                    return Err(CodecError::InvalidField {
                        field: "inputs",
                        value: inputs_value.clone(),
                    });
                }
                previous = Some(input.get());
                Ok(InputAudioMeters { input, channels })
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(AudioMetersMessage {
        server,
        sequence,
        frame,
        start_sample,
        end_sample,
        master,
        inputs,
    })
}

fn decode_meter_channels(
    value: &str,
    field: &'static str,
) -> Result<Vec<AudioMeterChannel>, CodecError> {
    let channels: Vec<_> = value.split(',').collect();
    if value.is_empty() || channels.len() > MAX_AUDIO_METER_CHANNELS {
        return Err(CodecError::InvalidField {
            field,
            value: value.to_owned(),
        });
    }
    channels
        .into_iter()
        .map(|channel| {
            let (peak, rms) = channel
                .split_once(':')
                .ok_or_else(|| CodecError::InvalidField {
                    field,
                    value: value.to_owned(),
                })?;
            let peak_millionths = peak.parse().map_err(|_| CodecError::InvalidField {
                field,
                value: value.to_owned(),
            })?;
            let rms_millionths = rms.parse().map_err(|_| CodecError::InvalidField {
                field,
                value: value.to_owned(),
            })?;
            let channel = AudioMeterChannel {
                peak_millionths,
                rms_millionths,
            };
            if !channel.is_valid() {
                return Err(CodecError::InvalidField {
                    field,
                    value: value.to_owned(),
                });
            }
            Ok(channel)
        })
        .collect()
}

fn decode_capability_report(fields: &mut Fields) -> Result<CapabilityReportMessage, CodecError> {
    Ok(CapabilityReportMessage {
        server: decode_server_identity(fields)?,
        revision: fields.parse_required("revision")?,
        summary: decode_capability_summary(fields)?,
    })
}

fn decode_capability_summary(fields: &mut Fields) -> Result<CapabilityReportSummary, CodecError> {
    let summary = CapabilityReportSummary {
        digest: fields.required("capability_digest")?,
        total: fields.parse_required("capability_total")?,
        available: fields.parse_required("capability_available")?,
        degraded: fields.parse_required("capability_degraded")?,
        unavailable: fields.parse_required("capability_unavailable")?,
    };
    let classified = summary
        .available
        .checked_add(summary.degraded)
        .and_then(|value| value.checked_add(summary.unavailable));
    if classified != Some(summary.total) {
        return Err(CodecError::InvalidField {
            field: "capability_total",
            value: summary.total.to_string(),
        });
    }
    Ok(summary)
}

fn decode_diagnostics_request(fields: &mut Fields) -> Result<DiagnosticsRequest, CodecError> {
    let protocol = fields.required("protocol")?;
    let request_id = fields.required("request_id")?;
    validate_request_id(&request_id)?;
    Ok(DiagnosticsRequest {
        protocol: parse_version(&protocol).ok_or(CodecError::InvalidField {
            field: "protocol",
            value: protocol,
        })?,
        request_id,
    })
}

fn decode_diagnostics_response(fields: &mut Fields) -> Result<DiagnosticsResponse, CodecError> {
    let protocol = fields.required("protocol")?;
    let request_id = fields.required("request_id")?;
    validate_request_id(&request_id)?;
    Ok(DiagnosticsResponse {
        protocol: parse_version(&protocol).ok_or(CodecError::InvalidField {
            field: "protocol",
            value: protocol,
        })?,
        request_id,
        engine: decode_identity(fields)?,
        current_revision: fields.parse_required("current_revision")?,
        oldest_retained_revision: fields.parse_optional("oldest_retained_revision")?,
        newest_retained_revision: fields.parse_optional("newest_retained_revision")?,
        subscriber_count: fields.parse_required("subscriber_count")?,
        retained_events_limit: fields.parse_required("retained_events_limit")?,
        subscriber_limit: fields.parse_required("subscriber_limit")?,
        subscriber_queue_limit: fields.parse_required("subscriber_queue_limit")?,
    })
}

fn decode_error_message(fields: &mut Fields) -> Result<ErrorMessage, CodecError> {
    Ok(ErrorMessage {
        request_id: fields.optional("request_id"),
        current_revision: fields.parse_optional("current_revision")?,
        error: decode_structured_error(fields)?,
    })
}

fn decode_structured_error(fields: &mut Fields) -> Result<StructuredError, CodecError> {
    let error = StructuredError {
        code: fields.required("code")?,
        message: fields.required("message")?,
        fields: parse_field_issues(&fields.required("fields")?)?,
        retryable: fields.boolean("retryable")?,
    };
    if error.code.is_empty()
        || error.code.bytes().any(|byte| {
            !(byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b'.'))
        })
    {
        return Err(CodecError::InvalidField {
            field: "code",
            value: error.code,
        });
    }
    Ok(error)
}

fn parse_snapshot_reason(value: &str) -> Option<SnapshotReason> {
    match value {
        "no_cursor" => Some(SnapshotReason::NoCursor),
        "identity_changed" => Some(SnapshotReason::IdentityChanged),
        "cursor_ahead" => Some(SnapshotReason::CursorAhead),
        "history_unavailable" => Some(SnapshotReason::HistoryUnavailable),
        _ => None,
    }
}

fn parse_failure_disposition(value: &str) -> Option<RuntimeFailureDisposition> {
    match value {
        "rolled_back" => Some(RuntimeFailureDisposition::RolledBack),
        "retained_for_retry" => Some(RuntimeFailureDisposition::RetainedForRetry),
        "fallback_realized" => Some(RuntimeFailureDisposition::FallbackRealized),
        _ => None,
    }
}
