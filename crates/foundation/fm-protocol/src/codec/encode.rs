use core::num::NonZeroU32;
use std::collections::BTreeSet;

use fm_grade::{CubeKind, Grade};

use crate::{
    AudioMeterChannel, AudioMetersMessage, CapabilityReportMessage, CapabilityReportSummary,
    CommandMessage, CommandPayload, CommandResult, DiagnosticsRequest, DiagnosticsResponse,
    DurableEventBatch, DurableGap, EditHistoryStatus, EditLockScope, EditLockStatus,
    EngineIdentity, ErrorMessage, EventCursor, EventMessage, EventPayload, FadeToBlackState,
    GradeChange, GradeEntry, GradeStateMessage, HandshakeOutcome, HandshakeRequest,
    HandshakeResponse, HeartbeatAcknowledgementMessage, HeartbeatMessage, InputAudioStripStatus,
    LiveInputSource, MAX_AUDIO_METER_CHANNELS, MAX_GRADE_STATES, ManualTransitionKind,
    ManualTransitionStatus, MonitorFrameMessage, OverlayStatus, PlaylistOperation,
    PlaylistStateMessage, ResumeCursor, RuntimeEventMessage, RuntimeFailureDisposition,
    RuntimeLifecycleEvent, SceneAnimationStateMessage, SceneLayoutStateMessage,
    ScenePreviewRequest, ServerIdentity, SnapshotMessage, SnapshotReason, StingerAudioPolicy,
    StingerMissingMediaFallback, StingerReadiness, StingerStatus, StreamStatusMessage,
    StructuredError, TitleAnimationStateMessage, TitleFieldUpdate, TitleLayoutStateMessage,
    TitlePreviewFrameMessage, TitlePreviewRequest, TitleStateMessage, WireGradeSource, WireMessage,
};

use super::animation::{
    encode_animation_phase, encode_clip, encode_preview_sample, validate_animation_inputs,
    validate_phase_frames, validate_preview_part, validate_preview_sample, validate_preview_size,
};
use super::grade::{
    encode_cube_kind, encode_grade, encode_lut_data, validate_grade_name, validate_lut_size,
};
use super::layout::encode_layout_elements;
use super::playlist::{
    encode_frame_rate, encode_optional_entry, encode_playlist_entries, encode_playlist_start,
    encode_playlist_state, playlist_loop,
};
use super::scene_layout::{encode_scene_clip, encode_scene_layers, encode_scene_list};
use super::title::{
    encode_title_field_names, encode_title_fields, encode_title_phase, encode_title_summaries,
    title_air_requirement, validate_title_name, validate_title_phase,
};
use super::value::{
    client_type, durable_events, escape_stream_failure, field_issues, input_ids, input_statuses,
    output_statuses, recording_statuses, role, runtime_domains, stream_realized_state,
    stream_statuses, string_list,
};
use super::{
    CodecError, MAX_FIELD_VALUE_BYTES, MAX_LINE_BYTES, MAX_LIST_ITEMS, MAX_STREAM_LOCAL_SEGMENTS,
    MAX_STREAM_LOCAL_TARGET_DURATION_MS, MAX_STREAM_SAMPLES, encode_rgba, validate_edit_history,
    validate_locked_inputs, validate_new_input_name, validate_overlay_duration,
    validate_request_id,
};

/// Encodes one message as a single newline-terminated record.
///
/// # Errors
///
/// Returns [`CodecError::LineTooLong`] if the encoded record exceeds
/// [`MAX_LINE_BYTES`].
pub fn encode_line(message: &WireMessage) -> Result<String, CodecError> {
    let mut record = Record::default();
    match message {
        WireMessage::Command(message) => encode_command(&mut record, message)?,
        WireMessage::CommandResult(message) => encode_result(&mut record, message)?,
        WireMessage::Snapshot(message) => encode_snapshot(&mut record, message)?,
        WireMessage::Event(message) => encode_event(&mut record, message)?,
        WireMessage::HandshakeRequest(message) => encode_handshake_request(&mut record, message)?,
        WireMessage::HandshakeResponse(message) => {
            encode_handshake_response(&mut record, message)?;
        }
        WireMessage::DurableEventBatch(message) => encode_durable_batch(&mut record, message)?,
        WireMessage::DurableGap(message) => encode_durable_gap(&mut record, message)?,
        WireMessage::RuntimeEvent(message) => encode_runtime_event(&mut record, message)?,
        WireMessage::Heartbeat(message) => encode_heartbeat(&mut record, message)?,
        WireMessage::HeartbeatAcknowledgement(message) => {
            encode_heartbeat_acknowledgement(&mut record, message)?;
        }
        WireMessage::AudioMeters(message) => encode_audio_meters(&mut record, message)?,
        WireMessage::StreamStatus(message) => encode_stream_status(&mut record, message)?,
        WireMessage::MonitorFrame(message) => encode_monitor_frame(&mut record, message)?,
        WireMessage::CapabilityReport(message) => encode_capability_report(&mut record, message)?,
        WireMessage::DiagnosticsRequest(message) => {
            encode_diagnostics_request(&mut record, message)?;
        }
        WireMessage::DiagnosticsResponse(message) => {
            encode_diagnostics_response(&mut record, message)?;
        }
        WireMessage::Error(message) => encode_error_message(&mut record, message)?,
        WireMessage::TitleState(message) => encode_title_state(&mut record, message)?,
        WireMessage::PlaylistState(message) => {
            encode_playlist_state_record(&mut record, message)?;
        }
        WireMessage::GradeState(message) => encode_grade_state(&mut record, message)?,
        WireMessage::TitleAnimationState(message) => {
            encode_title_animation_state(&mut record, message)?;
        }
        WireMessage::TitleLayoutState(message) => {
            encode_title_layout_state(&mut record, message)?;
        }
        WireMessage::SceneAnimationState(message) => {
            encode_scene_animation_state(&mut record, message)?;
        }
        WireMessage::SceneLayoutState(message) => {
            encode_scene_layout_state(&mut record, message)?;
        }
        WireMessage::TitlePreviewRequest(message) => {
            encode_title_preview_request(&mut record, message)?;
        }
        WireMessage::TitlePreviewFrame(message) => {
            encode_preview_frame(&mut record, "title_preview_frame", message)?;
        }
        WireMessage::ScenePreviewRequest(message) => {
            encode_scene_preview_request(&mut record, message)?;
        }
        WireMessage::ScenePreviewFrame(message) => {
            encode_preview_frame(&mut record, "scene_preview_frame", message)?;
        }
    }
    record.finish()
}

#[derive(Default)]
struct Record {
    kind: &'static str,
    fields: Vec<(&'static str, String)>,
}

impl Record {
    fn kind(&mut self, kind: &'static str) {
        self.kind = kind;
    }

    #[allow(clippy::needless_pass_by_value)]
    fn field(&mut self, name: &'static str, value: impl ToString) -> Result<(), CodecError> {
        let value = value.to_string();
        self.field_string(name, value)
    }

    fn field_str(&mut self, name: &'static str, value: &str) -> Result<(), CodecError> {
        if value.len() > MAX_FIELD_VALUE_BYTES {
            return Err(CodecError::FieldValueTooLong);
        }
        self.fields.push((name, value.to_owned()));
        Ok(())
    }

    fn field_string(&mut self, name: &'static str, value: String) -> Result<(), CodecError> {
        if value.len() > MAX_FIELD_VALUE_BYTES {
            return Err(CodecError::FieldValueTooLong);
        }
        self.fields.push((name, value));
        Ok(())
    }

    fn optional(
        &mut self,
        name: &'static str,
        value: Option<impl ToString>,
    ) -> Result<(), CodecError> {
        if let Some(value) = value {
            self.field(name, value)?;
        }
        Ok(())
    }

    fn finish(self) -> Result<String, CodecError> {
        let mut line = self.kind.to_owned();
        for (name, value) in self.fields {
            if line.len() + name.len() + 2 > MAX_LINE_BYTES {
                return Err(CodecError::LineTooLong);
            }
            line.push('\t');
            line.push_str(name);
            line.push('=');
            // The decoder bounds the escaped bytes, so bound them here too.
            let value_start = line.len();
            for byte in value.bytes() {
                let additional =
                    if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                        1
                    } else {
                        3
                    };
                if line.len() + additional + 1 > MAX_LINE_BYTES {
                    return Err(CodecError::LineTooLong);
                }
                if line.len() - value_start + additional > MAX_FIELD_VALUE_BYTES {
                    return Err(CodecError::FieldValueTooLong);
                }
                if additional == 1 {
                    line.push(char::from(byte));
                } else {
                    use core::fmt::Write;
                    write!(line, "%{byte:02X}").expect("writing to a string cannot fail");
                }
            }
        }
        if line.len() == MAX_LINE_BYTES {
            return Err(CodecError::LineTooLong);
        }
        line.push('\n');
        Ok(line)
    }
}

fn encode_identity(record: &mut Record, identity: &EngineIdentity) -> Result<(), CodecError> {
    record.field_str("engine_id", &identity.engine_id)?;
    record.field("state_epoch", identity.state_epoch)?;
    record.field_str("log_id", &identity.log_id)
}

fn encode_cursor(record: &mut Record, cursor: &EventCursor) -> Result<(), CodecError> {
    encode_identity(record, &cursor.engine)?;
    record.field("revision", cursor.revision)
}

#[allow(clippy::too_many_lines)] // one dispatch over every command payload
fn encode_command(record: &mut Record, message: &CommandMessage) -> Result<(), CodecError> {
    record.kind("command");
    record.field("protocol", message.protocol)?;
    record.field_str("id", &message.id)?;
    record.field_str("idempotency_key", &message.idempotency_key)?;
    record.optional("expected_revision", message.expected_revision)?;
    record.optional("deadline_ms", message.deadline_ms)?;
    match &message.payload {
        payload @ (CommandPayload::ReorderInputs { .. }
        | CommandPayload::RenameInput { .. }
        | CommandPayload::AddInput { .. }
        | CommandPayload::RemoveInput { .. }) => encode_input_catalogue_command(record, payload)?,
        payload @ (CommandPayload::SetTitleFields { .. }
        | CommandPayload::TakeTitle { .. }
        | CommandPayload::TitleOffAir { .. }) => encode_title_command(record, payload)?,
        payload @ (CommandPayload::SetPlaylist { .. }
        | CommandPayload::PlaylistControl { .. }
        | CommandPayload::PlaylistAdvance { .. }) => encode_playlist_command(record, payload)?,
        payload @ (CommandPayload::SetInputGrade { .. }
        | CommandPayload::SetProgramGrade { .. }
        | CommandPayload::AddLibraryGrade { .. }
        | CommandPayload::UpdateLibraryGrade { .. }
        | CommandPayload::RemoveLibraryGrade { .. }
        | CommandPayload::ImportGradeLut { .. }
        | CommandPayload::RemoveGradeLut { .. }) => encode_grade_command(record, payload)?,
        payload @ (CommandPayload::SetTitleAnimation { .. }
        | CommandPayload::ApplyTitleAnimationPreset { .. }
        | CommandPayload::StretchTitleAnimation { .. }) => {
            encode_title_animation_command(record, payload)?;
        }
        payload @ (CommandPayload::SetSceneAnimation { .. }
        | CommandPayload::ApplySceneAnimationPreset { .. }
        | CommandPayload::StretchSceneAnimation { .. }) => {
            encode_scene_animation_command(record, payload)?;
        }
        payload @ CommandPayload::SetInputAudioStrip { .. } => {
            encode_input_audio_strip_command(record, payload)?;
        }
        CommandPayload::SelectPreview { input } => {
            record.field("payload", "select_preview")?;
            record.field("input", input)?;
        }
        CommandPayload::Cut => record.field("payload", "cut")?,
        CommandPayload::Fade { duration_frames } => {
            record.field("payload", "fade")?;
            record.field("duration_frames", duration_frames)?;
        }
        CommandPayload::AlphaFade { duration_frames } => {
            record.field("payload", "alpha_fade")?;
            record.field("duration_frames", duration_frames)?;
        }
        CommandPayload::Slide { duration_frames } => {
            record.field("payload", "slide")?;
            record.field("duration_frames", duration_frames)?;
        }
        CommandPayload::Zoom { duration_frames } => {
            record.field("payload", "zoom")?;
            record.field("duration_frames", duration_frames)?;
        }
        CommandPayload::Stinger {
            slot,
            duration_frames,
        } => {
            record.field("payload", "stinger")?;
            record.field("slot", slot)?;
            record.field("duration_frames", duration_frames)?;
        }
        payload @ (CommandPayload::ConfigureStinger { .. }
        | CommandPayload::RemoveStinger { .. }) => encode_stinger_mutation(record, payload)?,
        payload @ (CommandPayload::TakeOverlay { .. }
        | CommandPayload::UpdateOverlay { .. }
        | CommandPayload::OverlayOff { .. }
        | CommandPayload::SetOverlayOutputInclusion { .. }
        | CommandPayload::ConfigureOverlayTransition { .. }
        | CommandPayload::ConfigureOverlayAppearance { .. }
        | CommandPayload::QueueOverlay { .. }
        | CommandPayload::TakeNextOverlay { .. }) => {
            encode_overlay_command(record, payload)?;
        }
        payload @ (CommandPayload::StreamStart { .. } | CommandPayload::StreamStop { .. }) => {
            encode_stream_command(record, payload)?;
        }
        payload @ (CommandPayload::RecordStart { .. } | CommandPayload::RecordStop { .. }) => {
            encode_record_command(record, payload)?;
        }
        CommandPayload::Wipe { duration_frames } => {
            record.field("payload", "wipe")?;
            record.field("duration_frames", duration_frames)?;
        }
        CommandPayload::FadeToBlack {
            active,
            duration_frames,
        } => {
            record.field("payload", "fade_to_black")?;
            record.field("active", u8::from(*active))?;
            record.field("duration_frames", duration_frames)?;
        }
        payload @ (CommandPayload::StartManualTransition { .. }
        | CommandPayload::SetManualTransitionPosition { .. }
        | CommandPayload::CommitManualTransition
        | CommandPayload::CancelManualTransition) => encode_manual_command(record, payload)?,
        payload @ (CommandPayload::Undo { .. }
        | CommandPayload::Redo { .. }
        | CommandPayload::SetEditLock { .. }) => encode_edit_command(record, payload)?,
    }
    Ok(())
}

fn encode_input_catalogue_command(
    record: &mut Record,
    payload: &CommandPayload,
) -> Result<(), CodecError> {
    match payload {
        CommandPayload::ReorderInputs { inputs } => {
            record.field("payload", "input_reorder")?;
            record.field_string("inputs", input_ids(inputs)?)
        }
        CommandPayload::RenameInput { input, name } => {
            record.field("payload", "input_rename")?;
            record.field("input", input)?;
            record.field_str("name", name)
        }
        CommandPayload::AddInput {
            input,
            name,
            source,
        } => {
            validate_new_input_name(name)?;
            record.field("payload", "input_add")?;
            record.field("input", input)?;
            record.field_str("name", name)?;
            match *source {
                LiveInputSource::Solid {
                    red,
                    green,
                    blue,
                    alpha,
                } => {
                    record.field("kind", "solid")?;
                    record.field_string("color", encode_rgba(red, green, blue, alpha))
                }
                LiveInputSource::Bars => record.field("kind", "bars"),
            }
        }
        CommandPayload::RemoveInput { input } => {
            record.field("payload", "input_remove")?;
            record.field("input", input)
        }
        _ => unreachable!("only input catalogue commands are delegated"),
    }
}

fn encode_title_command(record: &mut Record, payload: &CommandPayload) -> Result<(), CodecError> {
    match payload {
        CommandPayload::SetTitleFields {
            input,
            base_revision,
            require,
            fields,
        } => {
            let fields = encode_title_fields(fields)?;
            record.field("payload", "title_fields")?;
            record.field("input", input)?;
            record.field("base_revision", base_revision)?;
            record.field("require", title_air_requirement(*require))?;
            record.field_string("fields", fields)
        }
        CommandPayload::TakeTitle {
            input,
            channel,
            base_revision,
            fields,
        } => {
            let fields = encode_title_fields(fields)?;
            record.field("payload", "title_take")?;
            record.field("input", input)?;
            record.field("channel", channel)?;
            record.field("base_revision", base_revision)?;
            record.field_string("fields", fields)
        }
        CommandPayload::TitleOffAir { input } => {
            record.field("payload", "title_off_air")?;
            record.field("input", input)
        }
        _ => unreachable!("only title commands are delegated"),
    }
}

fn encode_title_animation_command(
    record: &mut Record,
    payload: &CommandPayload,
) -> Result<(), CodecError> {
    match payload {
        CommandPayload::SetTitleAnimation {
            input,
            base_revision,
            clip,
        } => {
            let clip = encode_clip(clip)?;
            record.field("payload", "title_animation_set")?;
            record.field("input", input)?;
            record.field("base_revision", base_revision)?;
            record.field_string("clip", clip)
        }
        CommandPayload::ApplyTitleAnimationPreset {
            input,
            base_revision,
            element,
            phase,
            preset,
            frames,
        } => {
            validate_phase_frames(*frames)?;
            record.field("payload", "title_animation_preset")?;
            record.field("input", input)?;
            record.field("base_revision", base_revision)?;
            record.field("element", element)?;
            record.field("phase", encode_animation_phase(*phase))?;
            record.field("preset", preset.id())?;
            record.field("frames", frames)
        }
        CommandPayload::StretchTitleAnimation {
            input,
            base_revision,
            phase,
            frames,
        } => {
            validate_phase_frames(*frames)?;
            record.field("payload", "title_animation_stretch")?;
            record.field("input", input)?;
            record.field("base_revision", base_revision)?;
            record.field("phase", encode_animation_phase(*phase))?;
            record.field("frames", frames)
        }
        _ => unreachable!("only title animation commands are delegated"),
    }
}

fn encode_scene_animation_command(
    record: &mut Record,
    payload: &CommandPayload,
) -> Result<(), CodecError> {
    match payload {
        CommandPayload::SetSceneAnimation {
            input,
            base_revision,
            clip,
        } => {
            let clip = encode_scene_clip(clip)?;
            record.field("payload", "scene_animation_set")?;
            record.field("input", input)?;
            record.field("base_revision", base_revision)?;
            record.field_string("clip", clip)
        }
        CommandPayload::ApplySceneAnimationPreset {
            input,
            base_revision,
            layer,
            phase,
            preset,
            frames,
        } => {
            validate_phase_frames(*frames)?;
            record.field("payload", "scene_animation_preset")?;
            record.field("input", input)?;
            record.field("base_revision", base_revision)?;
            record.field("layer", layer)?;
            record.field("phase", encode_animation_phase(*phase))?;
            record.field("preset", preset.id())?;
            record.field("frames", frames)
        }
        CommandPayload::StretchSceneAnimation {
            input,
            base_revision,
            phase,
            frames,
        } => {
            validate_phase_frames(*frames)?;
            record.field("payload", "scene_animation_stretch")?;
            record.field("input", input)?;
            record.field("base_revision", base_revision)?;
            record.field("phase", encode_animation_phase(*phase))?;
            record.field("frames", frames)
        }
        _ => unreachable!("only scene animation commands are delegated"),
    }
}

fn encode_scene_animation_changed(
    record: &mut Record,
    payload: &EventPayload,
) -> Result<(), CodecError> {
    let EventPayload::SceneAnimationChanged {
        inputs,
        animation_revision,
        clip,
    } = payload
    else {
        unreachable!("only scene animation changes are delegated")
    };
    validate_animation_inputs(inputs)?;
    let clip = encode_scene_clip(clip)?;
    record.field("event", "scene_animation_changed")?;
    record.field_string("inputs", input_ids(inputs)?)?;
    record.field("animation_revision", animation_revision)?;
    record.field_string("clip", clip)
}

fn encode_scene_animation_state(
    record: &mut Record,
    message: &SceneAnimationStateMessage,
) -> Result<(), CodecError> {
    let clip = encode_scene_clip(&message.clip)?;
    record.kind("scene_animation_state");
    record.field("revision", message.revision)?;
    record.field("input", message.input)?;
    record.field("animation_revision", message.animation_revision)?;
    record.field_string("clip", clip)
}

fn encode_scene_layout_state(
    record: &mut Record,
    message: &SceneLayoutStateMessage,
) -> Result<(), CodecError> {
    let layers = encode_scene_layers(&message.layout)?;
    record.kind("scene_layout_state");
    record.field("revision", message.revision)?;
    record.field("input", message.input)?;
    record.field("width", message.layout.width)?;
    record.field("height", message.layout.height)?;
    record.field_string("layers", layers)
}

fn encode_title_animation_changed(
    record: &mut Record,
    payload: &EventPayload,
) -> Result<(), CodecError> {
    let EventPayload::TitleAnimationChanged {
        inputs,
        animation_revision,
        clip,
    } = payload
    else {
        unreachable!("only title animation changes are delegated")
    };
    validate_animation_inputs(inputs)?;
    let clip = encode_clip(clip)?;
    record.field("event", "title_animation_changed")?;
    record.field_string("inputs", input_ids(inputs)?)?;
    record.field("animation_revision", animation_revision)?;
    record.field_string("clip", clip)
}

fn encode_title_animation_state(
    record: &mut Record,
    message: &TitleAnimationStateMessage,
) -> Result<(), CodecError> {
    let clip = encode_clip(&message.clip)?;
    record.kind("title_animation_state");
    record.field("revision", message.revision)?;
    record.field("input", message.input)?;
    record.field("animation_revision", message.animation_revision)?;
    record.field_string("clip", clip)
}

fn encode_title_layout_state(
    record: &mut Record,
    message: &TitleLayoutStateMessage,
) -> Result<(), CodecError> {
    let elements = encode_layout_elements(&message.layout)?;
    record.kind("title_layout_state");
    record.field("revision", message.revision)?;
    record.field("input", message.input)?;
    record.field("width", message.layout.width)?;
    record.field("height", message.layout.height)?;
    record.field_string("elements", elements)
}

/// Writes the fields every exact preview request carries, after its kind
/// and before its clip.
fn encode_preview_head(
    record: &mut Record,
    kind: &'static str,
    protocol: crate::ProtocolVersion,
    request_id: &str,
    input: crate::WireInputId,
    sample: crate::TitlePreviewSample,
    (width, height): (u16, u16),
) -> Result<(), CodecError> {
    validate_request_id(request_id)?;
    validate_preview_sample(sample)?;
    validate_preview_size(width, height)?;
    let (phase, frame) = encode_preview_sample(sample);
    record.kind(kind);
    record.field("protocol", protocol)?;
    record.field_str("request_id", request_id)?;
    record.field("input", input)?;
    record.field("phase", phase)?;
    record.field("frame", frame)?;
    record.field("width", width)?;
    record.field("height", height)
}

fn encode_title_preview_request(
    record: &mut Record,
    message: &TitlePreviewRequest,
) -> Result<(), CodecError> {
    let clip = message.clip.as_deref().map(encode_clip).transpose()?;
    encode_preview_head(
        record,
        "title_preview_request",
        message.protocol,
        &message.request_id,
        message.input,
        message.sample,
        (message.width, message.height),
    )?;
    if let Some(clip) = clip {
        record.field_string("clip", clip)?;
    }
    Ok(())
}

fn encode_scene_preview_request(
    record: &mut Record,
    message: &ScenePreviewRequest,
) -> Result<(), CodecError> {
    let clip = message.clip.as_deref().map(encode_scene_clip).transpose()?;
    encode_preview_head(
        record,
        "scene_preview_request",
        message.protocol,
        &message.request_id,
        message.input,
        message.sample,
        (message.width, message.height),
    )?;
    if let Some(clip) = clip {
        record.field_string("clip", clip)?;
    }
    Ok(())
}

/// One `title_preview_frame` or `scene_preview_frame` part: the same fields
/// under `kind`.
fn encode_preview_frame(
    record: &mut Record,
    kind: &'static str,
    message: &TitlePreviewFrameMessage,
) -> Result<(), CodecError> {
    validate_request_id(&message.request_id)?;
    validate_preview_part(
        message.width,
        message.height,
        message.part,
        message.parts,
        message.data.len(),
    )?;
    record.kind(kind);
    encode_server_identity(record, &message.server)?;
    record.field_str("request_id", &message.request_id)?;
    record.field("input", message.input)?;
    record.field("animation_revision", message.animation_revision)?;
    record.field("draft", u8::from(message.draft))?;
    record.field("width", message.width)?;
    record.field("height", message.height)?;
    record.field("part", message.part)?;
    record.field("parts", message.parts)?;
    record.field_string(
        "data",
        crate::monitor_image::base64url_encode(&message.data),
    )
}

fn encode_playlist_command(
    record: &mut Record,
    payload: &CommandPayload,
) -> Result<(), CodecError> {
    match payload {
        CommandPayload::SetPlaylist { loop_mode, entries } => {
            let entries = encode_playlist_entries(entries)?;
            record.field("payload", "playlist_set")?;
            record.field("loop", playlist_loop(*loop_mode))?;
            record.field_string("entries", entries)
        }
        CommandPayload::PlaylistControl { operation } => match *operation {
            PlaylistOperation::Start { at, from } => {
                record.field("payload", "playlist_start")?;
                record.field_string("start", encode_playlist_start(at))?;
                record.field_string("from", encode_optional_entry(from))
            }
            PlaylistOperation::Stop => record.field("payload", "playlist_stop"),
            PlaylistOperation::Pause => record.field("payload", "playlist_pause"),
            PlaylistOperation::Resume => record.field("payload", "playlist_resume"),
            PlaylistOperation::Next => record.field("payload", "playlist_next"),
            PlaylistOperation::Previous => record.field("payload", "playlist_previous"),
            PlaylistOperation::Jump { entry } => {
                record.field("payload", "playlist_jump")?;
                record.field("entry", entry)
            }
        },
        CommandPayload::PlaylistAdvance { frame } => {
            record.field("payload", "playlist_advance")?;
            record.field("frame", frame)
        }
        _ => unreachable!("only playlist commands are delegated"),
    }
}

/// Encodes one `title_state` record: the title's name and complete fields,
/// values in `fields` and their names positionally in `field_names`.
fn encode_title_state(record: &mut Record, message: &TitleStateMessage) -> Result<(), CodecError> {
    validate_title_name(&message.name)?;
    validate_title_phase(message.phase, message.phase_frame, message.phase_frames)?;
    let updates = message
        .fields
        .iter()
        .map(|status| TitleFieldUpdate {
            field: status.field,
            value: status.value.clone(),
        })
        .collect::<Vec<_>>();
    let fields = encode_title_fields(&updates)?;
    let names = encode_title_field_names(message.fields.iter().map(|status| status.name.as_str()))?;
    record.kind("title_state");
    record.field("revision", message.revision)?;
    record.field("input", message.input)?;
    record.field_str("name", &message.name)?;
    record.field("field_revision", message.field_revision)?;
    record.field("phase", encode_title_phase(message.phase))?;
    record.field("phase_frame", message.phase_frame)?;
    record.field("phase_frames", message.phase_frames)?;
    record.field_string("fields", fields)?;
    record.field_string("field_names", names)
}

fn encode_manual_command(record: &mut Record, payload: &CommandPayload) -> Result<(), CodecError> {
    match payload {
        CommandPayload::StartManualTransition { kind } => {
            record.field("payload", "manual_start")?;
            record.field(
                "transition",
                match kind {
                    crate::ManualTransitionKind::Fade => "fade",
                    crate::ManualTransitionKind::Wipe => "wipe",
                    crate::ManualTransitionKind::AlphaFade => "alpha_fade",
                    crate::ManualTransitionKind::Slide => "slide",
                },
            )?;
        }
        CommandPayload::SetManualTransitionPosition { position } => {
            record.field("payload", "manual_position")?;
            record.field("position_basis_points", position.basis_points())?;
        }
        CommandPayload::CommitManualTransition => record.field("payload", "manual_commit")?,
        CommandPayload::CancelManualTransition => record.field("payload", "manual_cancel")?,
        _ => unreachable!("only manual transition commands are delegated"),
    }
    Ok(())
}

fn encode_edit_command(record: &mut Record, payload: &CommandPayload) -> Result<(), CodecError> {
    match payload {
        CommandPayload::Undo { expected_edit } | CommandPayload::Redo { expected_edit } => {
            record.field(
                "payload",
                if matches!(payload, CommandPayload::Undo { .. }) {
                    "undo"
                } else {
                    "redo"
                },
            )?;
            if *expected_edit == Some(0) {
                return Err(CodecError::InvalidField {
                    field: "expected_edit",
                    value: "0".to_owned(),
                });
            }
            record.optional("expected_edit", *expected_edit)
        }
        CommandPayload::SetEditLock { scope, locked } => {
            record.field("payload", "edit_lock")?;
            encode_edit_lock_scope(record, *scope)?;
            record.field("locked", u8::from(*locked))
        }
        _ => unreachable!("only edit-history and padlock commands are delegated"),
    }
}

fn encode_edit_lock_scope(record: &mut Record, scope: EditLockScope) -> Result<(), CodecError> {
    match scope {
        EditLockScope::Project => record.field("scope", "project"),
        EditLockScope::Input(input) => {
            record.field("scope", "input")?;
            record.field("input", input)
        }
    }
}

fn encode_edit_history(
    record: &mut Record,
    history: &EditHistoryStatus,
    revision: u64,
) -> Result<(), CodecError> {
    validate_edit_history(history, revision)?;
    record.field("undo_depth", history.undo_depth)?;
    record.optional("undo_edit", history.next_undo)?;
    record.field("redo_depth", history.redo_depth)?;
    record.optional("redo_edit", history.next_redo)
}

fn encode_edit_locks(
    record: &mut Record,
    locks: &EditLockStatus,
    inputs: &[crate::InputStatus],
) -> Result<(), CodecError> {
    let inputs = inputs.iter().map(|status| status.input).collect::<Vec<_>>();
    validate_locked_inputs(&locks.inputs, &inputs)?;
    record.field("project_locked", u8::from(locks.project))?;
    record.field_string(
        "locked_inputs",
        locks
            .inputs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(","),
    )
}

fn encode_input_audio_strip_command(
    record: &mut Record,
    payload: &CommandPayload,
) -> Result<(), CodecError> {
    let CommandPayload::SetInputAudioStrip {
        input,
        gain_millidb,
        balance_basis_points,
        muted,
        soloed,
        follow_video,
        delay_samples,
    } = payload
    else {
        unreachable!("input audio strip encoder received another command")
    };
    record.field("payload", "input_audio_strip")?;
    record.field("input", input)?;
    record.field("gain_millidb", gain_millidb)?;
    record.field("balance_basis_points", balance_basis_points)?;
    record.field("muted", u8::from(*muted))?;
    record.field("soloed", u8::from(*soloed))?;
    record.field("follow_video", u8::from(*follow_video))?;
    record.field("delay_samples", delay_samples)
}

fn encode_overlay_command(record: &mut Record, payload: &CommandPayload) -> Result<(), CodecError> {
    match payload {
        CommandPayload::TakeOverlay { channel, source }
        | CommandPayload::UpdateOverlay { channel, source } => {
            record.field(
                "payload",
                if matches!(payload, CommandPayload::TakeOverlay { .. }) {
                    "overlay_take"
                } else {
                    "overlay_update"
                },
            )?;
            record.field("channel", channel)?;
            record.field("source", source)
        }
        CommandPayload::OverlayOff { channel } => {
            record.field("payload", "overlay_off")?;
            record.field("channel", channel)
        }
        CommandPayload::SetOverlayOutputInclusion {
            channel,
            output,
            included,
        } => {
            record.field("payload", "overlay_output")?;
            record.field("channel", channel)?;
            record.field("output", output)?;
            record.field("included", u8::from(*included))
        }
        CommandPayload::ConfigureOverlayTransition {
            channel,
            transition,
            duration_frames,
        } => {
            record.field("payload", "overlay_transition")?;
            record.field("channel", channel)?;
            record.field(
                "transition",
                match transition {
                    crate::OverlayTransitionKind::Cut => "cut",
                    crate::OverlayTransitionKind::Fade => "fade",
                },
            )?;
            validate_overlay_duration(*duration_frames, "duration_frames")?;
            record.field("duration_frames", duration_frames)
        }
        CommandPayload::ConfigureOverlayAppearance {
            channel,
            position,
            border,
        } => {
            record.field("payload", "overlay_appearance")?;
            record.field("channel", channel)?;
            record.field("position", overlay_position(*position))?;
            record.field("border", overlay_border(*border))
        }
        CommandPayload::QueueOverlay { channel, source } => {
            record.field("payload", "overlay_queue")?;
            record.field("channel", channel)?;
            record.field("source", source)
        }
        CommandPayload::TakeNextOverlay { channel } => {
            record.field("payload", "overlay_next")?;
            record.field("channel", channel)
        }
        _ => unreachable!("only overlay commands are delegated"),
    }
}

fn encode_stinger_mutation(
    record: &mut Record,
    payload: &CommandPayload,
) -> Result<(), CodecError> {
    let CommandPayload::ConfigureStinger {
        slot,
        media_input,
        preload,
        cut_point_frames,
        audio_policy,
        missing_media_fallback,
    } = payload
    else {
        let CommandPayload::RemoveStinger { slot } = payload else {
            unreachable!("only Stinger mutation payloads are delegated")
        };
        record.field("payload", "remove_stinger")?;
        return record.field("slot", slot);
    };
    record.field("payload", "configure_stinger")?;
    record.field("slot", slot)?;
    record.field("media_input", media_input)?;
    record.field("preload", u8::from(*preload))?;
    record.field("cut_point_frames", cut_point_frames)?;
    record.field(
        "audio_policy",
        match audio_policy {
            StingerAudioPolicy::Muted => "muted",
            StingerAudioPolicy::StingerOnly => "stinger_only",
            StingerAudioPolicy::MixWithProgram => "mix_with_program",
        },
    )?;
    record.field(
        "missing_media_fallback",
        match missing_media_fallback {
            StingerMissingMediaFallback::Cut => "cut",
            StingerMissingMediaFallback::Fade => "fade",
            StingerMissingMediaFallback::KeepProgram => "keep_program",
        },
    )
}

fn encode_stream_command(record: &mut Record, payload: &CommandPayload) -> Result<(), CodecError> {
    let (name, target) = match payload {
        CommandPayload::StreamStart { target } => ("stream_start", target),
        CommandPayload::StreamStop { target } => ("stream_stop", target),
        _ => unreachable!("only stream commands are delegated"),
    };
    record.field("payload", name)?;
    record.field("target", *target)
}

fn encode_record_command(record: &mut Record, payload: &CommandPayload) -> Result<(), CodecError> {
    let (name, recorder) = match payload {
        CommandPayload::RecordStart { recorder } => ("record_start", recorder),
        CommandPayload::RecordStop { recorder } => ("record_stop", recorder),
        _ => unreachable!("only record commands are delegated"),
    };
    record.field("payload", name)?;
    record.field("recorder", recorder.get())
}

fn encode_result(record: &mut Record, message: &CommandResult) -> Result<(), CodecError> {
    record.kind("command_result");
    match message {
        CommandResult::Accepted {
            id,
            revision,
            scheduled_frame,
        } => {
            record.field("status", "accepted")?;
            record.field_str("id", id)?;
            record.field("revision", revision)?;
            record.optional("scheduled_frame", *scheduled_frame)?;
        }
        CommandResult::Rejected {
            id,
            code,
            message,
            fields,
            current_revision,
            retryable,
        } => {
            record.field("status", "rejected")?;
            record.field_str("id", id)?;
            record.field_str("code", code)?;
            record.field_str("message", message)?;
            record.field_string("fields", field_issues(fields)?)?;
            record.field("current_revision", current_revision)?;
            record.field("retryable", u8::from(*retryable))?;
        }
    }
    Ok(())
}

fn encode_snapshot(record: &mut Record, message: &SnapshotMessage) -> Result<(), CodecError> {
    record.kind("snapshot");
    encode_identity(record, &message.engine)?;
    record.field("revision", message.revision)?;
    record.field_str("show_name", &message.show_name)?;
    record.field_string("inputs", input_statuses(&message.inputs)?)?;
    record.field_string("outputs", output_statuses(&message.outputs)?)?;
    encode_input_audio_strips(record, &message.input_audio_strips)?;
    record.field("desired_program", message.desired_program)?;
    record.field("desired_preview", message.desired_preview)?;
    record.field("realized_program", message.realized_program)?;
    record.field("realized_preview", message.realized_preview)?;
    encode_manual_status(
        record,
        message.desired_manual_transition,
        ManualStatusFields::Desired,
    )?;
    encode_manual_status(
        record,
        message.realized_manual_transition,
        ManualStatusFields::Realized,
    )?;
    encode_fade_to_black_state(
        record,
        message.desired_fade_to_black,
        FadeToBlackStateFields::Desired,
    )?;
    encode_fade_to_black_state(
        record,
        message.realized_fade_to_black,
        FadeToBlackStateFields::Realized,
    )?;
    encode_stingers(record, &message.stingers)?;
    record.field_string("streams", stream_statuses(&message.streams)?)?;
    record.field_string("recordings", recording_statuses(&message.recordings)?)?;
    encode_overlays(record, "desired_overlays", &message.desired_overlays)?;
    encode_overlays(record, "realized_overlays", &message.realized_overlays)?;
    encode_edit_locks(record, &message.edit_locks, &message.inputs)?;
    encode_edit_history(record, &message.edit_history, message.revision)?;
    let inputs = message
        .inputs
        .iter()
        .map(|input| input.input)
        .collect::<Vec<_>>();
    record.field_string("titles", encode_title_summaries(&message.titles, &inputs)?)?;
    record.field_string("scenes", encode_scene_list(&message.scenes, &inputs)?)?;
    record.field_string("frame_rate", encode_frame_rate(message.frame_rate))?;
    record.field("playlist", u8::from(message.playlist_follows))?;
    if message.grade_states > MAX_GRADE_STATES {
        return Err(CodecError::InvalidField {
            field: "grade_states",
            value: message.grade_states.to_string(),
        });
    }
    record.field("grade_states", message.grade_states)
}

/// Writes a grade source as `library=<id>` or `grade=<grade>`.
fn encode_grade_source(record: &mut Record, source: &WireGradeSource) -> Result<(), CodecError> {
    match source {
        WireGradeSource::Library(grade) => record.field("library", grade),
        WireGradeSource::Inline(grade) => record.field_string("grade", encode_grade(grade)?),
    }
}

fn encode_library_grade(
    record: &mut Record,
    grade: NonZeroU32,
    name: &str,
    value: &Grade,
) -> Result<(), CodecError> {
    validate_grade_name(name)?;
    let value = encode_grade(value)?;
    record.field("grade_id", grade)?;
    record.field_str("name", name)?;
    record.field_string("grade", value)
}

fn encode_lut_metadata(
    record: &mut Record,
    lut: NonZeroU32,
    name: &str,
    kind: CubeKind,
    size: u32,
) -> Result<(), CodecError> {
    validate_grade_name(name)?;
    validate_lut_size(kind, size)?;
    record.field("lut_id", lut)?;
    record.field_str("name", name)?;
    record.field("kind", encode_cube_kind(kind))?;
    record.field("size", size)
}

/// Encodes the grade commands (Protocol 2.28).
fn encode_grade_command(record: &mut Record, payload: &CommandPayload) -> Result<(), CodecError> {
    match payload {
        CommandPayload::SetInputGrade { input, source } => {
            record.field(
                "payload",
                if source.is_some() {
                    "grade_input_set"
                } else {
                    "grade_input_clear"
                },
            )?;
            record.field("input", input)?;
            source
                .as_ref()
                .map_or(Ok(()), |source| encode_grade_source(record, source))
        }
        CommandPayload::SetProgramGrade { source } => {
            record.field(
                "payload",
                if source.is_some() {
                    "grade_program_set"
                } else {
                    "grade_program_clear"
                },
            )?;
            source
                .as_ref()
                .map_or(Ok(()), |source| encode_grade_source(record, source))
        }
        CommandPayload::AddLibraryGrade {
            grade,
            name,
            value,
            index,
        } => {
            if index.is_some_and(|index| usize::from(index) >= MAX_LIST_ITEMS) {
                return Err(CodecError::InvalidField {
                    field: "index",
                    value: index.unwrap_or_default().to_string(),
                });
            }
            record.field("payload", "grade_library_add")?;
            encode_library_grade(record, *grade, name, value)?;
            record.optional("index", *index)
        }
        CommandPayload::UpdateLibraryGrade { grade, name, value } => {
            record.field("payload", "grade_library_update")?;
            encode_library_grade(record, *grade, name, value)
        }
        CommandPayload::RemoveLibraryGrade { grade } => {
            record.field("payload", "grade_library_remove")?;
            record.field("grade_id", grade)
        }
        CommandPayload::ImportGradeLut { lut, name, data } => {
            validate_grade_name(name)?;
            let data = encode_lut_data(data)?;
            record.field("payload", "grade_lut_import")?;
            record.field("lut_id", lut)?;
            record.field_str("name", name)?;
            record.field_string("data", data)
        }
        CommandPayload::RemoveGradeLut { lut } => {
            record.field("payload", "grade_lut_remove")?;
            record.field("lut_id", lut)
        }
        _ => unreachable!("only grade commands are delegated"),
    }
}

/// Encodes one `grade_state` record.
fn encode_grade_state(record: &mut Record, message: &GradeStateMessage) -> Result<(), CodecError> {
    record.kind("grade_state");
    record.field("revision", message.revision)?;
    match &message.entry {
        GradeEntry::Lut {
            lut,
            name,
            kind,
            size,
        } => {
            record.field("entry", "lut")?;
            encode_lut_metadata(record, *lut, name, *kind, *size)
        }
        GradeEntry::Library { grade, name, value } => {
            record.field("entry", "library")?;
            encode_library_grade(record, *grade, name, value)
        }
        GradeEntry::Input { input, source } => {
            record.field("entry", "input")?;
            record.field("input", input)?;
            encode_grade_source(record, source)
        }
        GradeEntry::Program { source } => {
            record.field("entry", "program")?;
            encode_grade_source(record, source)
        }
    }
}

/// Encodes the fields of one `grade_changed` event.
fn encode_grade_change(record: &mut Record, change: &GradeChange) -> Result<(), CodecError> {
    match change {
        GradeChange::LutImported {
            lut,
            name,
            kind,
            size,
        } => {
            record.field("change", "lut_imported")?;
            encode_lut_metadata(record, *lut, name, *kind, *size)
        }
        GradeChange::LutRemoved { lut } => {
            record.field("change", "lut_removed")?;
            record.field("lut_id", lut)
        }
        GradeChange::Library {
            grade,
            index,
            name,
            value,
        } => {
            if usize::from(*index) >= MAX_LIST_ITEMS {
                return Err(CodecError::InvalidField {
                    field: "index",
                    value: index.to_string(),
                });
            }
            record.field("change", "library")?;
            encode_library_grade(record, *grade, name, value)?;
            record.field("index", index)
        }
        GradeChange::LibraryRemoved { grade } => {
            record.field("change", "library_removed")?;
            record.field("grade_id", grade)
        }
        GradeChange::Input { input, source } => {
            record.field("change", "input")?;
            record.field("input", input)?;
            source
                .as_ref()
                .map_or(Ok(()), |source| encode_grade_source(record, source))
        }
        GradeChange::Program { source } => {
            record.field("change", "program")?;
            source
                .as_ref()
                .map_or(Ok(()), |source| encode_grade_source(record, source))
        }
    }
}

/// Encodes one `playlist_state` record: the complete playlist after a
/// snapshot that announced it.
fn encode_playlist_state_record(
    record: &mut Record,
    message: &PlaylistStateMessage,
) -> Result<(), CodecError> {
    let entries = encode_playlist_entries(&message.entries)?;
    record.kind("playlist_state");
    record.field("revision", message.revision)?;
    record.field("frame", message.frame)?;
    record.field("loop", playlist_loop(message.loop_mode))?;
    record.field_string("entries", entries)?;
    record.field_string("state", encode_playlist_state(message.state))
}

fn encode_overlays(
    record: &mut Record,
    field: &'static str,
    overlays: &[OverlayStatus],
) -> Result<(), CodecError> {
    if overlays.len() != 8 {
        return Err(CodecError::InvalidField {
            field,
            value: format!("expected 8 channels, found {}", overlays.len()),
        });
    }
    let mut channels = [false; 8];
    let value = overlays
        .iter()
        .map(|overlay| {
            let channel = usize::from(overlay.channel.number() - 1);
            if channels[channel] {
                return Err(CodecError::InvalidField {
                    field,
                    value: format!("duplicate channel {}", overlay.channel.number()),
                });
            }
            channels[channel] = true;
            validate_overlay_duration(overlay.duration_frames, field)?;
            if overlay.queued_sources.len() > 64 {
                return Err(CodecError::TooManyItems("overlay queue"));
            }
            let mut included_outputs = BTreeSet::new();
            for output in &overlay.included_outputs {
                if !included_outputs.insert(output.to_domain().get()) {
                    return Err(CodecError::InvalidField {
                        field,
                        value: output.to_string(),
                    });
                }
            }
            let outputs = overlay
                .included_outputs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let queue = overlay
                .queued_sources
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            Ok(format!(
                "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
                overlay.channel,
                overlay
                    .source
                    .map_or_else(|| "0".to_owned(), |source| source.to_string()),
                u8::from(overlay.active),
                overlay.opacity,
                match overlay.transition {
                    crate::OverlayTransitionKind::Cut => "cut",
                    crate::OverlayTransitionKind::Fade => "fade",
                },
                overlay.duration_frames,
                overlay_position(overlay.position),
                overlay_border(overlay.border),
                queue,
                outputs,
            ))
        })
        .collect::<Result<Vec<_>, CodecError>>()?
        .join(";");
    record.field_string(field, value)
}

fn encode_input_audio_strips(
    record: &mut Record,
    strips: &[InputAudioStripStatus],
) -> Result<(), CodecError> {
    check_count(strips.len(), "input audio strips")?;
    if strips.is_empty() {
        return Err(CodecError::InvalidField {
            field: "input_audio_strips",
            value: String::new(),
        });
    }
    let mut inputs = BTreeSet::new();
    for status in strips {
        if !(-96_000..=24_000).contains(&status.gain_millidb)
            || !(-10_000..=10_000).contains(&status.balance_basis_points)
            || status.delay_samples > 48_000
            || !inputs.insert(status.input.get())
        {
            return Err(CodecError::InvalidField {
                field: "input_audio_strips",
                value: format!(
                    "{}:{}:{}:{}:{}:{}:{}",
                    status.input,
                    status.gain_millidb,
                    status.balance_basis_points,
                    u8::from(status.muted),
                    u8::from(status.soloed),
                    u8::from(status.follow_video),
                    status.delay_samples
                ),
            });
        }
    }
    record.field_string(
        "input_audio_strips",
        strips
            .iter()
            .map(|status| {
                format!(
                    "{}:{}:{}:{}:{}:{}:{}",
                    status.input,
                    status.gain_millidb,
                    status.balance_basis_points,
                    u8::from(status.muted),
                    u8::from(status.soloed),
                    u8::from(status.follow_video),
                    status.delay_samples
                )
            })
            .collect::<Vec<_>>()
            .join(","),
    )
}

const fn overlay_position(position: crate::OverlayPositionPreset) -> &'static str {
    match position {
        crate::OverlayPositionPreset::FullFrame => "full_frame",
        crate::OverlayPositionPreset::TopLeft => "top_left",
        crate::OverlayPositionPreset::TopRight => "top_right",
        crate::OverlayPositionPreset::BottomLeft => "bottom_left",
        crate::OverlayPositionPreset::BottomRight => "bottom_right",
    }
}

const fn overlay_border(border: crate::OverlayBorderPreset) -> &'static str {
    match border {
        crate::OverlayBorderPreset::None => "none",
        crate::OverlayBorderPreset::ThinWhite => "thin_white",
        crate::OverlayBorderPreset::ThickWhite => "thick_white",
    }
}

fn encode_stingers(record: &mut Record, stingers: &[StingerStatus]) -> Result<(), CodecError> {
    if stingers.len() > 8 {
        return Err(CodecError::TooManyItems("stingers"));
    }
    let mut seen = [false; 8];
    for status in stingers {
        let index = usize::from(status.slot.number() - 1);
        if seen[index] {
            return Err(CodecError::InvalidField {
                field: "stingers",
                value: format!("duplicate slot {}", status.slot.number()),
            });
        }
        seen[index] = true;
    }
    let value = stingers
        .iter()
        .map(|status| {
            format!(
                "{}:{}:{}:{}:{}:{}:{}",
                status.slot.number(),
                status.media_input,
                u8::from(status.preload),
                status.cut_point_frames,
                match status.audio_policy {
                    StingerAudioPolicy::Muted => "muted",
                    StingerAudioPolicy::StingerOnly => "stinger_only",
                    StingerAudioPolicy::MixWithProgram => "mix_with_program",
                },
                match status.missing_media_fallback {
                    StingerMissingMediaFallback::Cut => "cut",
                    StingerMissingMediaFallback::Fade => "fade",
                    StingerMissingMediaFallback::KeepProgram => "keep_program",
                },
                match status.readiness {
                    StingerReadiness::NotRequested => "not_requested",
                    StingerReadiness::Ready => "ready",
                    StingerReadiness::Missing => "missing",
                },
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    record.field_string("stingers", value)
}

#[allow(clippy::too_many_lines)]
fn encode_event(record: &mut Record, message: &EventMessage) -> Result<(), CodecError> {
    record.kind("event");
    encode_cursor(record, &message.cursor)?;
    match &message.payload {
        EventPayload::InputOrderChanged { inputs } => {
            record.field("event", "input_order_changed")?;
            record.field_string("inputs", input_ids(inputs)?)?;
        }
        EventPayload::InputRenamed { input, name } => {
            record.field("event", "input_renamed")?;
            record.field("input", input)?;
            record.field_str("name", name)?;
        }
        EventPayload::InputAdded { input, name } => {
            validate_new_input_name(name)?;
            record.field("event", "input_added")?;
            record.field("input", input)?;
            record.field_str("name", name)?;
        }
        EventPayload::InputRemoved { input } => {
            record.field("event", "input_removed")?;
            record.field("input", input)?;
        }
        EventPayload::DesiredSwitcher {
            program,
            preview,
            manual_transition,
            fade_to_black,
            overlays,
            input_audio_strips,
        } => {
            record.field("event", "desired_switcher")?;
            record.field("program", program)?;
            record.field("preview", preview)?;
            encode_manual_status(record, *manual_transition, ManualStatusFields::Unqualified)?;
            encode_fade_to_black_state(
                record,
                *fade_to_black,
                FadeToBlackStateFields::Unqualified,
            )?;
            encode_overlays(record, "overlays", overlays)?;
            encode_input_audio_strips(record, input_audio_strips)?;
        }
        EventPayload::StingerSlotsChanged {
            program,
            preview,
            manual_transition,
            fade_to_black,
            stingers,
            overlays,
            input_audio_strips,
        } => {
            record.field("event", "stinger_slots_changed")?;
            record.field("program", program)?;
            record.field("preview", preview)?;
            encode_manual_status(record, *manual_transition, ManualStatusFields::Unqualified)?;
            encode_fade_to_black_state(
                record,
                *fade_to_black,
                FadeToBlackStateFields::Unqualified,
            )?;
            encode_stingers(record, stingers)?;
            encode_overlays(record, "overlays", overlays)?;
            encode_input_audio_strips(record, input_audio_strips)?;
        }
        EventPayload::StreamsChanged { streams } => {
            record.field("event", "streams_changed")?;
            record.field_string("streams", stream_statuses(streams)?)?;
        }
        EventPayload::RecordingChanged { recorder, active } => {
            record.field("event", "recording_changed")?;
            record.field("recorder", recorder.get())?;
            record.field("active", u8::from(*active))?;
        }
        EventPayload::EditLockChanged { scope, locked } => {
            record.field("event", "edit_lock_changed")?;
            encode_edit_lock_scope(record, *scope)?;
            record.field("locked", u8::from(*locked))?;
        }
        EventPayload::TitleChanged {
            input,
            field_revision,
            fields,
            overlays,
        } => {
            let fields = encode_title_fields(fields)?;
            record.field("event", "title_changed")?;
            record.field("input", input)?;
            record.field("field_revision", field_revision)?;
            record.field_string("fields", fields)?;
            encode_overlays(record, "overlays", overlays)?;
        }
        payload @ EventPayload::PlaylistChanged { .. } => encode_playlist_changed(record, payload)?,
        EventPayload::GradeChanged(change) => {
            record.field("event", "grade_changed")?;
            encode_grade_change(record, change)?;
        }
        payload @ EventPayload::TitleAnimationChanged { .. } => {
            encode_title_animation_changed(record, payload)?;
        }
        payload @ EventPayload::SceneAnimationChanged { .. } => {
            encode_scene_animation_changed(record, payload)?;
        }
    }
    encode_edit_history(record, &message.edit_history, message.cursor.revision)
}

fn encode_playlist_changed(record: &mut Record, payload: &EventPayload) -> Result<(), CodecError> {
    let EventPayload::PlaylistChanged {
        frame,
        loop_mode,
        entries,
        state,
        program,
        preview,
    } = payload
    else {
        unreachable!("only playlist_changed is delegated");
    };
    let entries = encode_playlist_entries(entries)?;
    record.field("event", "playlist_changed")?;
    record.field("frame", frame)?;
    record.field("loop", playlist_loop(*loop_mode))?;
    record.field_string("entries", entries)?;
    record.field_string("state", encode_playlist_state(*state))?;
    record.field("program", program)?;
    record.field("preview", preview)
}

fn encode_server_identity(
    record: &mut Record,
    identity: &ServerIdentity,
) -> Result<(), CodecError> {
    record.field_str("engine_id", &identity.engine_id)?;
    record.field_str("project_id", &identity.project_id)?;
    record.field("state_epoch", identity.state_epoch)?;
    record.field_str("log_id", &identity.log_id)
}

fn encode_resume_cursor(
    record: &mut Record,
    cursor: &ResumeCursor,
    prefix: &'static str,
) -> Result<(), CodecError> {
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
    record.field_str(engine, &cursor.server.engine_id)?;
    record.field_str(project, &cursor.server.project_id)?;
    record.field(epoch, cursor.server.state_epoch)?;
    record.field_str(log, &cursor.server.log_id)?;
    record.field(revision, cursor.revision)
}

fn encode_handshake_request(
    record: &mut Record,
    message: &HandshakeRequest,
) -> Result<(), CodecError> {
    record.kind("handshake_request");
    record.field("protocol", message.protocol)?;
    record.field_str("build", &message.build)?;
    record.field("client_type", client_type(message.client_type))?;
    record.field("desired_role", role(message.desired_role))?;
    record.field("resume", u8::from(message.resume_cursor.is_some()))?;
    if let Some(cursor) = &message.resume_cursor {
        encode_resume_cursor(record, cursor, "resume")?;
    }
    Ok(())
}

fn encode_handshake_response(
    record: &mut Record,
    message: &HandshakeResponse,
) -> Result<(), CodecError> {
    record.kind("handshake_response");
    record.field("protocol", message.protocol)?;
    record.field("granted_role", role(message.granted_role))?;
    record.field_string("permissions", string_list(&message.permissions)?)?;
    encode_capability_summary(record, &message.capabilities)?;
    encode_server_identity(record, &message.server)?;
    record.field("current_revision", message.current_revision)?;
    match &message.outcome {
        HandshakeOutcome::Snapshot { reason } => {
            record.field("outcome", "snapshot")?;
            record.field("snapshot_reason", snapshot_reason(*reason))?;
        }
        HandshakeOutcome::Resume { cursor } => {
            if cursor.server != message.server || cursor.revision > message.current_revision {
                return Err(CodecError::InvalidField {
                    field: "resume_revision",
                    value: cursor.revision.to_string(),
                });
            }
            record.field("outcome", "resume")?;
            encode_resume_cursor(record, cursor, "resume")?;
        }
        HandshakeOutcome::Rejected { error } => {
            record.field("outcome", "rejected")?;
            encode_structured_error(record, error)?;
        }
    }
    Ok(())
}

fn encode_durable_batch(
    record: &mut Record,
    message: &DurableEventBatch,
) -> Result<(), CodecError> {
    record.kind("durable_event_batch");
    if message.events.is_empty()
        || message
            .events
            .iter()
            .enumerate()
            .any(|(expected, event)| usize::from(event.sequence) != expected)
    {
        return Err(CodecError::InvalidField {
            field: "events",
            value: "event sequences must be contiguous from zero".to_owned(),
        });
    }
    encode_server_identity(record, &message.cursor.server)?;
    record.field("revision", message.cursor.revision)?;
    record.field_string("events", durable_events(&message.events)?)
}

fn encode_durable_gap(record: &mut Record, message: &DurableGap) -> Result<(), CodecError> {
    record.kind("durable_gap");
    if message.requested_after_revision.saturating_add(1) >= message.available_from_revision
        || message.available_from_revision > message.current_revision
    {
        return Err(CodecError::InvalidField {
            field: "available_from_revision",
            value: message.available_from_revision.to_string(),
        });
    }
    encode_server_identity(record, &message.server)?;
    record.field("requested_after_revision", message.requested_after_revision)?;
    record.field("available_from_revision", message.available_from_revision)?;
    record.field("current_revision", message.current_revision)
}

fn encode_runtime_event(
    record: &mut Record,
    message: &RuntimeEventMessage,
) -> Result<(), CodecError> {
    record.kind("runtime_event");
    encode_server_identity(record, &message.server)?;
    record.field("revision", message.revision)?;
    record.field("generation", message.generation)?;
    record.field("sequence", message.sequence)?;
    match &message.event {
        RuntimeLifecycleEvent::Accepted => record.field("event", "accepted")?,
        RuntimeLifecycleEvent::Preparing => record.field("event", "preparing")?,
        RuntimeLifecycleEvent::Scheduled { domains } => {
            record.field("event", "scheduled")?;
            record.field_string("domains", runtime_domains(domains)?)?;
        }
        RuntimeLifecycleEvent::Realized {
            domain,
            manual_transition,
            fade_to_black,
        } => {
            record.field("event", "realized")?;
            record.field_str("domain", domain)?;
            encode_manual_status(record, *manual_transition, ManualStatusFields::Unqualified)?;
            encode_fade_to_black_state(
                record,
                *fade_to_black,
                FadeToBlackStateFields::Unqualified,
            )?;
        }
        RuntimeLifecycleEvent::Failed { error, disposition } => {
            record.field("event", "failed")?;
            record.field("disposition", failure_disposition(*disposition))?;
            encode_structured_error(record, error)?;
        }
        RuntimeLifecycleEvent::Superseded { by_revision } => {
            record.field("event", "superseded")?;
            record.field("by_revision", by_revision)?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ManualStatusFields {
    Desired,
    Realized,
    Unqualified,
}

fn encode_manual_status(
    record: &mut Record,
    status: ManualTransitionStatus,
    fields: ManualStatusFields,
) -> Result<(), CodecError> {
    let (active, kind, from, to, interval_start, position) = match fields {
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
    match status {
        ManualTransitionStatus::Inactive => record.field(active, 0),
        ManualTransitionStatus::Active(state) => {
            record.field(active, 1)?;
            record.field(
                kind,
                match state.kind {
                    ManualTransitionKind::Fade => "fade",
                    ManualTransitionKind::Wipe => "wipe",
                    ManualTransitionKind::AlphaFade => "alpha_fade",
                    ManualTransitionKind::Slide => "slide",
                },
            )?;
            record.field(from, state.from)?;
            record.field(to, state.to)?;
            record.field(interval_start, state.interval_start.basis_points())?;
            record.field(position, state.position.basis_points())
        }
    }
}

#[derive(Clone, Copy)]
enum FadeToBlackStateFields {
    Desired,
    Realized,
    Unqualified,
}

fn encode_fade_to_black_state(
    record: &mut Record,
    state: FadeToBlackState,
    fields: FadeToBlackStateFields,
) -> Result<(), CodecError> {
    let (target_active, position) = match fields {
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
    record.field(target_active, u8::from(state.target_active))?;
    record.field(position, state.position.numerator())
}

fn encode_heartbeat(record: &mut Record, message: &HeartbeatMessage) -> Result<(), CodecError> {
    record.kind("heartbeat");
    encode_server_identity(record, &message.server)?;
    record.field("sequence", message.sequence)?;
    record.field("sent_at_ms", message.sent_at_ms)?;
    record.field("applied", u8::from(message.last_applied.is_some()))?;
    if let Some(cursor) = &message.last_applied {
        if cursor.server != message.server {
            return Err(CodecError::InvalidField {
                field: "applied_engine_id",
                value: cursor.server.engine_id.clone(),
            });
        }
        encode_resume_cursor(record, cursor, "applied")?;
    }
    Ok(())
}

fn encode_heartbeat_acknowledgement(
    record: &mut Record,
    message: &HeartbeatAcknowledgementMessage,
) -> Result<(), CodecError> {
    record.kind("heartbeat_acknowledgement");
    encode_server_identity(record, &message.server)?;
    record.field("heartbeat_sequence", message.heartbeat_sequence)?;
    record.field("received_at_ms", message.received_at_ms)
}

fn encode_stream_status(
    record: &mut Record,
    message: &StreamStatusMessage,
) -> Result<(), CodecError> {
    if message.samples.is_empty() {
        return Err(CodecError::InvalidField {
            field: "samples",
            value: "empty".to_owned(),
        });
    }
    if message.samples.len() > MAX_STREAM_SAMPLES {
        return Err(CodecError::TooManyItems("samples"));
    }
    let mut previous = None;
    let samples = message
        .samples
        .iter()
        .map(|sample| {
            if previous.is_some_and(|id| sample.target.get() <= id) {
                return Err(CodecError::InvalidField {
                    field: "samples",
                    value: sample.target.to_string(),
                });
            }
            previous = Some(sample.target.get());
            if let Some(failure) = &sample.failure
                && (failure.is_empty() || failure.len() > super::MAX_STREAM_DETAIL_BYTES)
            {
                return Err(CodecError::InvalidField {
                    field: "samples",
                    value: sample.target.to_string(),
                });
            }
            let local = match sample.local {
                Some(local) => {
                    if local.segments > MAX_STREAM_LOCAL_SEGMENTS
                        || local.target_duration_ms > MAX_STREAM_LOCAL_TARGET_DURATION_MS
                    {
                        return Err(CodecError::InvalidField {
                            field: "samples",
                            value: format!(
                                "local={}:{}:{}",
                                local.segments, local.target_duration_ms, local.free_bytes
                            ),
                        });
                    }
                    format!(
                        ":local={}:{}:{}",
                        local.segments, local.target_duration_ms, local.free_bytes
                    )
                }
                None => String::new(),
            };
            Ok(format!(
                "{}:{}:{}:{}:{}:{}:{}{}",
                sample.target,
                stream_realized_state(sample.realized),
                u8::from(sample.connected),
                sample.muxed_bytes,
                sample.enqueued_pairs,
                sample.dropped_pairs,
                match &sample.failure {
                    Some(failure) => escape_stream_failure(failure)?,
                    None => String::new(),
                },
                local,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    record.kind("stream_status");
    encode_server_identity(record, &message.server)?;
    record.field("sequence", message.sequence)?;
    record.field_string("samples", samples.join(";"))
}

fn encode_monitor_frame(
    record: &mut Record,
    message: &MonitorFrameMessage,
) -> Result<(), CodecError> {
    super::validate_monitor_frame(
        message.width,
        message.height,
        message.part,
        message.parts,
        message.data.len(),
    )?;
    record.kind("monitor_frame");
    encode_server_identity(record, &message.server)?;
    record.field_str("tap", message.tap.as_str())?;
    record.field("sequence", message.sequence)?;
    record.field("frame", message.frame)?;
    record.field("input", message.input)?;
    record.field_str("origin", message.origin.as_str())?;
    record.field("width", message.width)?;
    record.field("height", message.height)?;
    record.field("part", message.part)?;
    record.field("parts", message.parts)?;
    record.field_string(
        "data",
        crate::monitor_image::base64url_encode(&message.data),
    )
}

fn encode_audio_meters(
    record: &mut Record,
    message: &AudioMetersMessage,
) -> Result<(), CodecError> {
    let channels = message.master.len();
    if !(1..=MAX_AUDIO_METER_CHANNELS).contains(&channels) {
        return Err(CodecError::InvalidField {
            field: "master",
            value: channels.to_string(),
        });
    }
    if message.end_sample <= message.start_sample {
        return Err(CodecError::InvalidField {
            field: "end_sample",
            value: message.end_sample.to_string(),
        });
    }
    let master = encode_meter_channels(&message.master, "master")?;
    let mut previous = None;
    let inputs = message
        .inputs
        .iter()
        .map(|input| {
            if input.channels.len() != channels
                || previous.is_some_and(|id| input.input.get() <= id)
            {
                return Err(CodecError::InvalidField {
                    field: "inputs",
                    value: input.input.to_string(),
                });
            }
            previous = Some(input.input.get());
            Ok(format!(
                "{}~{}",
                input.input,
                encode_meter_channels(&input.channels, "inputs")?
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if inputs.len() > super::MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems("inputs"));
    }
    record.kind("audio_meters");
    encode_server_identity(record, &message.server)?;
    record.field("sequence", message.sequence)?;
    record.field("frame", message.frame)?;
    record.field("start_sample", message.start_sample)?;
    record.field("end_sample", message.end_sample)?;
    record.field_string("master", master)?;
    record.field_string("inputs", inputs.join(";"))
}

fn encode_meter_channels(
    channels: &[AudioMeterChannel],
    field: &'static str,
) -> Result<String, CodecError> {
    channels
        .iter()
        .map(|channel| {
            if !channel.is_valid() {
                return Err(CodecError::InvalidField {
                    field,
                    value: channel.rms_millionths.to_string(),
                });
            }
            Ok(format!(
                "{}:{}",
                channel.peak_millionths, channel.rms_millionths
            ))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|channels| channels.join(","))
}

fn encode_capability_report(
    record: &mut Record,
    message: &CapabilityReportMessage,
) -> Result<(), CodecError> {
    record.kind("capability_report");
    encode_server_identity(record, &message.server)?;
    record.field("revision", message.revision)?;
    encode_capability_summary(record, &message.summary)
}

fn encode_capability_summary(
    record: &mut Record,
    summary: &CapabilityReportSummary,
) -> Result<(), CodecError> {
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
    record.field_str("capability_digest", &summary.digest)?;
    record.field("capability_total", summary.total)?;
    record.field("capability_available", summary.available)?;
    record.field("capability_degraded", summary.degraded)?;
    record.field("capability_unavailable", summary.unavailable)
}

fn encode_diagnostics_request(
    record: &mut Record,
    message: &DiagnosticsRequest,
) -> Result<(), CodecError> {
    record.kind("diagnostics_request");
    record.field("protocol", message.protocol)?;
    validate_request_id(&message.request_id)?;
    record.field_str("request_id", &message.request_id)
}

fn encode_diagnostics_response(
    record: &mut Record,
    message: &DiagnosticsResponse,
) -> Result<(), CodecError> {
    record.kind("diagnostics_response");
    record.field("protocol", message.protocol)?;
    validate_request_id(&message.request_id)?;
    record.field_str("request_id", &message.request_id)?;
    encode_identity(record, &message.engine)?;
    record.field("current_revision", message.current_revision)?;
    record.optional("oldest_retained_revision", message.oldest_retained_revision)?;
    record.optional("newest_retained_revision", message.newest_retained_revision)?;
    record.field("subscriber_count", message.subscriber_count)?;
    record.field("retained_events_limit", message.retained_events_limit)?;
    record.field("subscriber_limit", message.subscriber_limit)?;
    record.field("subscriber_queue_limit", message.subscriber_queue_limit)
}

fn encode_error_message(record: &mut Record, message: &ErrorMessage) -> Result<(), CodecError> {
    record.kind("error");
    if let Some(request_id) = &message.request_id {
        record.field_str("request_id", request_id)?;
    }
    record.optional("current_revision", message.current_revision)?;
    encode_structured_error(record, &message.error)
}

fn encode_structured_error(record: &mut Record, error: &StructuredError) -> Result<(), CodecError> {
    if error.code.is_empty()
        || error.code.bytes().any(|byte| {
            !(byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b'.'))
        })
    {
        return Err(CodecError::InvalidField {
            field: "code",
            value: error.code.clone(),
        });
    }
    record.field_str("code", &error.code)?;
    record.field_str("message", &error.message)?;
    record.field_string("fields", field_issues(&error.fields)?)?;
    record.field("retryable", u8::from(error.retryable))
}

const fn snapshot_reason(reason: SnapshotReason) -> &'static str {
    match reason {
        SnapshotReason::NoCursor => "no_cursor",
        SnapshotReason::IdentityChanged => "identity_changed",
        SnapshotReason::CursorAhead => "cursor_ahead",
        SnapshotReason::HistoryUnavailable => "history_unavailable",
    }
}

const fn failure_disposition(disposition: RuntimeFailureDisposition) -> &'static str {
    match disposition {
        RuntimeFailureDisposition::RolledBack => "rolled_back",
        RuntimeFailureDisposition::RetainedForRetry => "retained_for_retry",
        RuntimeFailureDisposition::FallbackRealized => "fallback_realized",
    }
}

fn check_count(count: usize, field: &'static str) -> Result<(), CodecError> {
    if count > MAX_LIST_ITEMS {
        Err(CodecError::TooManyItems(field))
    } else {
        Ok(())
    }
}
