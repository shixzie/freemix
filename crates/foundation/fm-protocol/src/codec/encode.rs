use core::num::NonZeroU32;
use std::collections::BTreeSet;

use fm_grade::{CubeKind, Grade};

use crate::{
    AnimationPresetChange, AnimationPresetEntry, AnimationPresetStateMessage, AudioMeterChannel,
    AudioMetersMessage, CapabilityReportMessage, CapabilityReportSummary, CommandMessage,
    CommandPayload, CommandResult, DiagnosticsRequest, DiagnosticsResponse, DurableEventBatch,
    DurableGap, EditHistoryStatus, EditLockScope, EditLockStatus, EngineIdentity, ErrorMessage,
    EventCursor, EventMessage, EventPayload, FadeToBlackState, GradeChange, GradeEntry,
    GradeStateMessage, HandshakeOutcome, HandshakeRequest, HandshakeResponse,
    HeartbeatAcknowledgementMessage, HeartbeatMessage, InputAudioStripStatus, LiveInputSource,
    MAX_ANIMATION_PRESETS, MAX_AUDIO_METER_CHANNELS, MAX_GRADE_STATES, MAX_MEDIA_STATES,
    ManualTransitionStatus, MonitorFrameMessage, OverlayStatus, PlaylistOperation,
    PlaylistStateMessage, ResumeCursor, RuntimeEventMessage, RuntimeFailureDisposition,
    RuntimeLifecycleEvent, SceneAnimationStateMessage, SceneLayoutStateMessage,
    ScenePreviewRequest, ServerIdentity, SnapshotMessage, SnapshotReason, StingerAudioPolicy,
    StingerMissingMediaFallback, StingerReadiness, StingerStatus, StreamStatusMessage,
    StructuredError, TitleAnimationStateMessage, TitleFieldUpdate, TitleLayoutStateMessage,
    TitlePreviewFrameMessage, TitlePreviewRequest, TitleStateMessage, WireGradeSource, WireMessage,
};

use super::animation::{
    encode_animation_phase, encode_clip, encode_motion, encode_preview_sample,
    validate_animation_inputs, validate_phase_frames, validate_preview_part,
    validate_preview_sample, validate_preview_size,
};
use super::grade::{
    encode_cube_kind, encode_grade, encode_lut_data, validate_grade_name, validate_lut_size,
    validate_preset_name,
};
use super::layout::encode_layout_elements;
use super::media::{
    encode_media_events, encode_media_operation_fields, encode_media_status, event_operation_field,
    media_payload_name,
};
use super::playlist::{
    encode_frame_rate, encode_optional_entry, encode_playlist_entries, encode_playlist_start,
    encode_playlist_state, playlist_loop,
};
use super::scene_layout::{
    encode_layout_scenes, encode_scene_clip, encode_scene_layers, encode_scene_list,
};
use super::title::{
    encode_title_field_names, encode_title_fields, encode_title_phase, encode_title_summaries,
    title_air_requirement, validate_title_name, validate_title_phase,
};
use super::transition::{encode_favorite, encode_favorites, validate_favorite_slot};
use super::value::{
    client_type, durable_events, field_issues, input_ids, input_statuses, output_statuses,
    recording_statuses, role, runtime_domains, stream_realized_state, stream_statuses, string_list,
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
#[allow(clippy::too_many_lines)] // one arm per message kind
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
        WireMessage::RecorderStatus(message) => {
            let recorders = super::recorder_status::encode_samples(&message.samples)?;
            record.kind("recorder_status");
            encode_server_identity(&mut record, &message.server)?;
            record.field("sequence", message.sequence)?;
            record.field_string("recorders", recorders)?;
        }
        WireMessage::SourceSyncStatus(message) => {
            let sources = super::source_sync::encode_samples("sources", &message.sources)?;
            record.kind("source_sync_status");
            encode_server_identity(&mut record, &message.server)?;
            record.field("sequence", message.sequence)?;
            record.field_string("sources", sources)?;
        }
        WireMessage::MonitorFrame(message) => encode_monitor_frame(&mut record, message)?,
        WireMessage::InputThumbnailSubscription(message) => {
            record.kind("input_thumbnail_subscription");
            record.field_str("enabled", if message.enabled { "1" } else { "0" })?;
        }
        WireMessage::InputThumbnail(message) => encode_input_thumbnail(&mut record, message)?,
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
        WireMessage::AnimationPresetState(message) => {
            encode_animation_preset_state(&mut record, message)?;
        }
        WireMessage::AudioStripDspState(message) => {
            record.kind("audio_strip_dsp_state");
            record.field("revision", message.revision)?;
            record.field("input", message.input)?;
            record.field_string(
                "chain",
                super::audio_mixer::encode_chain("chain", &message.chain)?,
            )?;
        }
        WireMessage::DisplayInventoryRequest(message) => {
            record.kind("display_inventory_request");
            validate_request_id(&message.request_id)?;
            record.field_str("request_id", &message.request_id)?;
        }
        WireMessage::CaptureDevicesRequest(message) => {
            record.kind("capture_devices_request");
            validate_request_id(&message.request_id)?;
            record.field_str("request_id", &message.request_id)?;
        }
        WireMessage::CaptureDevices(message) => {
            record.kind("capture_devices");
            validate_request_id(&message.request_id)?;
            record.field_str("request_id", &message.request_id)?;
            super::capture::encode_inventory(&message.inventory, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        WireMessage::SystemAudioDevicesRequest(message) => {
            record.kind("system_audio_devices_request");
            validate_request_id(&message.request_id)?;
            record.field_str("request_id", &message.request_id)?;
        }
        WireMessage::SystemAudioDevices(message) => {
            record.kind("system_audio_devices");
            validate_request_id(&message.request_id)?;
            record.field_str("request_id", &message.request_id)?;
            super::system_audio::encode_inventory(&message.inventory, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        WireMessage::WebAccessRequest(message) => {
            record.kind("web_access_request");
            validate_request_id(&message.request_id)?;
            record.field_str("request_id", &message.request_id)?;
            super::web_access::encode_action(&message.action, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        WireMessage::WebAccess(message) => {
            record.kind("web_access");
            validate_request_id(&message.request_id)?;
            record.field_str("request_id", &message.request_id)?;
            if let Some(refusal) = super::web_access::refusal_token(message.refusal) {
                record.field_str("refusal", refusal)?;
            }
            record.field_str("gateway", if message.gateway.is_some() { "1" } else { "0" })?;
            if let Some(gateway) = &message.gateway {
                super::web_access::encode_gateway(gateway, &mut |name, value| {
                    record.field_string(name, value)
                })?;
            }
        }
        WireMessage::DisplayInventory(message) => {
            record.kind("display_inventory");
            validate_request_id(&message.request_id)?;
            record.field_str("request_id", &message.request_id)?;
            super::display_output::encode_inventory(&message.inventory, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        WireMessage::CueListState(message) => {
            record.kind("cue_list_state");
            record.field("revision", message.revision)?;
            record.field("frame", message.frame)?;
            super::cue::encode_list(&message.list, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        WireMessage::ShortcutState(message) => {
            record.kind("shortcut_state");
            record.field("revision", message.revision)?;
            super::shortcut::encode_status(&message.shortcut, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        WireMessage::ScriptState(message) => {
            record.kind("script_state");
            record.field("revision", message.revision)?;
            super::script::encode_status(&message.script, &mut |name, value| {
                record.field_string(name, value)
            })?;
            record.field_string("run", super::script::encode_run(&message.run)?)?;
        }
        WireMessage::StillState(message) => {
            record.kind("still_state");
            record.field("revision", message.revision)?;
            record.field("input", message.input)?;
            super::still::encode_source(&message.source, &mut |name, value| {
                record.field_string(name, value)
            })?;
            super::still::encode_page_status(&message.status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        WireMessage::SlideshowSlidesRequest(message) => {
            record.kind("slideshow_slides_request");
            validate_request_id(&message.request_id)?;
            super::still::check_window(message.offset, message.limit)?;
            record.field_str("request_id", &message.request_id)?;
            record.field("input", message.input)?;
            record.field("offset", message.offset)?;
            record.field("limit", message.limit)?;
        }
        WireMessage::SlideshowSlides(message) => {
            record.kind("slideshow_slides");
            validate_request_id(&message.request_id)?;
            if message.total as usize > crate::MAX_SLIDESHOW_SLIDES
                || (!message.slides.is_empty()
                    && u64::from(message.offset) + message.slides.len() as u64
                        > u64::from(message.total))
            {
                return Err(CodecError::InvalidField {
                    field: "total",
                    value: message.total.to_string(),
                });
            }
            record.field_str("request_id", &message.request_id)?;
            record.field("input", message.input)?;
            record.field("generation", message.generation)?;
            record.field("offset", message.offset)?;
            record.field("total", message.total)?;
            record.field_string("slides", super::still::encode_slides(&message.slides)?)?;
        }
        WireMessage::MediaListState(message) => {
            record.kind("media_list_state");
            record.field("revision", message.revision)?;
            super::media_list::encode_status(&message.status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        WireMessage::MediaState(message) => {
            record.kind("media_state");
            record.field("revision", message.revision)?;
            record.field("frame", message.frame)?;
            encode_media_status(&message.status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        WireMessage::PagedState(message) => {
            record.kind("paged_state");
            record.field("revision", message.revision)?;
            super::paged::encode_status(&message.status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        WireMessage::PagedPageRequest(message) => {
            record.kind("paged_page_request");
            super::paged::encode_request(message, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        WireMessage::PagedPage(message) => {
            record.kind("paged_page");
            super::paged::encode_page(message, &mut |name, value| {
                record.field_string(name, value)
            })?;
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
        CommandPayload::SetCueList { list, spec } => {
            record.field("payload", "cue_list_set")?;
            super::cue::encode_spec(*list, spec, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::RemoveCueList { list } => {
            record.field("payload", "cue_list_remove")?;
            if *list == 0 {
                return Err(CodecError::InvalidField {
                    field: "list",
                    value: "0".to_owned(),
                });
            }
            record.field("list", list)?;
        }
        CommandPayload::CueControl { list, operation } => {
            record.field("payload", operation.payload())?;
            super::cue::encode_operation(*list, *operation, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::SetScript { script, spec } => {
            record.field("payload", "script_set")?;
            super::script::encode_spec(*script, spec, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::RemoveScript { script } | CommandPayload::RunScript { script } => {
            record.field(
                "payload",
                if matches!(&message.payload, CommandPayload::RunScript { .. }) {
                    "script_run"
                } else {
                    "script_remove"
                },
            )?;
            super::script::validate_script_id(*script)?;
            record.field("script", script)?;
        }
        CommandPayload::CueAdvance { list, frame } => {
            // List 0 is the waiting script run (Protocol 2.62).
            record.field("payload", "cue_advance")?;
            record.field("list", list)?;
            record.field("frame", frame)?;
        }
        CommandPayload::SetShortcut { shortcut, spec } => {
            record.field("payload", "shortcut_set")?;
            super::shortcut::encode_spec(*shortcut, spec, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::RemoveShortcut { shortcut } | CommandPayload::FireShortcut { shortcut } => {
            record.field(
                "payload",
                if matches!(&message.payload, CommandPayload::FireShortcut { .. }) {
                    "shortcut_fire"
                } else {
                    "shortcut_remove"
                },
            )?;
            super::shortcut::validate_shortcut_id(*shortcut)?;
            record.field("shortcut", shortcut)?;
        }
        CommandPayload::StopScript { run } => {
            record.field("payload", "script_stop")?;
            if let Some(run) = run {
                super::script::validate_run_id(*run)?;
                record.field("run", run)?;
            }
        }
        payload @ (CommandPayload::SetInputGrade { .. }
        | CommandPayload::SetLayerGrade { .. }
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
        payload @ (CommandPayload::AddAnimationPreset { .. }
        | CommandPayload::RenameAnimationPreset { .. }
        | CommandPayload::RemoveAnimationPreset { .. }
        | CommandPayload::ApplyTitleAnimationUserPreset { .. }
        | CommandPayload::ApplySceneAnimationUserPreset { .. }) => {
            encode_animation_preset_command(record, payload)?;
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
        CommandPayload::Transition {
            kind,
            duration_frames,
        } => {
            record.field("payload", "transition")?;
            record.field("kind", kind.token())?;
            record.field("duration_frames", duration_frames)?;
        }
        CommandPayload::AudioMixer(operation) => {
            record.field("payload", operation.payload())?;
            super::audio_mixer::encode_operation(operation, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::SetFadeToBlackOutput { output, affected } => {
            record.field("payload", "fade_to_black_output")?;
            record.field("output", output)?;
            record.field("affected", u8::from(*affected))?;
        }
        CommandPayload::SetFadeToBlackOutputDuration {
            output,
            duration_frames,
        } => {
            record.field("payload", "fade_to_black_output_duration")?;
            record.field("output", output)?;
            record.field_string(
                "duration_frames",
                super::audio_mixer::encode_optional_duration(*duration_frames)?,
            )?;
        }
        CommandPayload::SetFadeToBlackCurve { curve } => {
            record.field("payload", "fade_to_black_curve")?;
            record.field("curve", curve.token())?;
        }
        CommandPayload::SetDisplayOutput { slot, config } => {
            record.field("payload", "set_display_output")?;
            record.field("slot", slot)?;
            super::display_output::encode_config(config, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::ClearDisplayOutput { slot } => {
            record.field("payload", "clear_display_output")?;
            record.field("slot", slot)?;
        }
        CommandPayload::SetLiveLan { config, pin } => {
            record.field("payload", "live_lan_set")?;
            super::live_lan::encode_set(config, pin, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::SetCaptureSettings { input, settings } => {
            record.field("payload", "input_capture_set")?;
            record.field("input", input)?;
            super::capture::encode_settings(settings, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::SetNetworkStream {
            input,
            url,
            settings,
            stream_id,
            stream_key,
            passphrase,
        } => {
            record.field("payload", "input_stream_set")?;
            super::network_stream::encode_set(
                *input,
                &super::network_stream::SetFields {
                    url: url.clone(),
                    settings: *settings,
                    stream_id: stream_id.clone(),
                    stream_key: stream_key.clone(),
                    passphrase: passphrase.clone(),
                },
                &mut |name, value| record.field_string(name, value),
            )?;
        }
        CommandPayload::SceneLayer(edit) => {
            record.field("payload", edit.operation.payload())?;
            super::scene_layer::encode_scene_layer_edit(edit, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::InputLifecycle(operation) => {
            record.field("payload", operation.payload())?;
            super::input_lifecycle::encode_lifecycle_operation(operation, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::PageNavigate { input, navigation } => {
            record.field("payload", "page_navigate")?;
            record.field("input", input)?;
            super::still::encode_navigation(*navigation, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::MediaList { input, operation } => {
            record.field("payload", "media_list")?;
            super::media_list::encode_command(*input, operation, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::PagedInput { input, navigation } => {
            record.field("payload", "paged_input")?;
            super::paged::encode_command(*input, *navigation, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::Media { input, operation } => {
            record.field("payload", media_payload_name(*operation))?;
            record.field("input", input)?;
            encode_media_operation_fields(*operation, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        CommandPayload::SetTransitionFavorite { slot, favorite } => {
            record.field("payload", "transition_favorite_set")?;
            validate_favorite_slot(*slot, "slot")?;
            record.field("slot", slot)?;
            record.field_string("favorite", encode_favorite(*favorite, "favorite")?)?;
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
        CommandPayload::StreamRetry { target } => {
            record.field("payload", "stream_retry")?;
            record.field("target", *target)?;
        }
        CommandPayload::StreamStartAll => record.field("payload", "stream_start_all")?,
        CommandPayload::StreamStopAll => record.field("payload", "stream_stop_all")?,
        CommandPayload::SetStreamTarget {
            target,
            config,
            stream_key,
            srt_passphrase,
            position,
        } => {
            record.field("payload", "stream_target_set")?;
            super::stream_target::encode_set(
                *target,
                config,
                stream_key,
                srt_passphrase,
                *position,
                &mut |name, value| record.field_string(name, value),
            )?;
        }
        CommandPayload::RemoveStreamTarget { target } => {
            record.field("payload", "stream_target_remove")?;
            record.field("target", *target)?;
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
            match source {
                LiveInputSource::Solid {
                    red,
                    green,
                    blue,
                    alpha,
                } => {
                    record.field("kind", "solid")?;
                    record.field_string("color", encode_rgba(*red, *green, *blue, *alpha))
                }
                LiveInputSource::Bars => record.field("kind", "bars"),
                LiveInputSource::NetworkStream(config) => {
                    record.field("kind", "network_stream")?;
                    super::network_stream::encode_config(
                        config,
                        super::network_stream::Secrets::Exact,
                        &mut |name, value| record.field_string(name, value),
                    )
                }
                LiveInputSource::Capture(config) => {
                    record.field("kind", "capture")?;
                    super::capture::encode_config(config, &mut |name, value| {
                        record.field_string(name, value)
                    })
                }
                LiveInputSource::SystemAudio(config) => {
                    record.field("kind", "system_audio")?;
                    super::system_audio::encode_config(config, &mut |name, value| {
                        record.field_string(name, value)
                    })
                }
                LiveInputSource::Image(config) => super::still::encode_source(
                    &crate::WireStillSource::Image((**config).clone()),
                    &mut |name, value| record.field_string(name, value),
                ),
                LiveInputSource::Slideshow(config) => super::still::encode_source(
                    &crate::WireStillSource::Slideshow((**config).clone()),
                    &mut |name, value| record.field_string(name, value),
                ),
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

/// Encodes the animation preset library commands and the user preset
/// applications (Protocol 2.34).
fn encode_animation_preset_command(
    record: &mut Record,
    payload: &CommandPayload,
) -> Result<(), CodecError> {
    match payload {
        CommandPayload::AddAnimationPreset {
            preset,
            name,
            phase,
            motion,
            index,
        } => {
            if index.is_some_and(|index| index >= MAX_ANIMATION_PRESETS) {
                return Err(CodecError::InvalidField {
                    field: "index",
                    value: index.unwrap_or_default().to_string(),
                });
            }
            record.field("payload", "animation_preset_add")?;
            encode_animation_preset_entry(
                record,
                &AnimationPresetEntry {
                    preset: *preset,
                    name: name.clone(),
                    phase: *phase,
                    motion: motion.clone(),
                },
            )?;
            record.optional("index", *index)
        }
        CommandPayload::RenameAnimationPreset { preset, name } => {
            validate_preset_name(name)?;
            record.field("payload", "animation_preset_rename")?;
            record.field("preset_id", preset)?;
            record.field_str("name", name)
        }
        CommandPayload::RemoveAnimationPreset { preset } => {
            record.field("payload", "animation_preset_remove")?;
            record.field("preset_id", preset)
        }
        CommandPayload::ApplyTitleAnimationUserPreset {
            input,
            base_revision,
            element,
            phase,
            preset,
            frames,
        } => {
            validate_phase_frames(*frames)?;
            record.field("payload", "title_animation_user_preset")?;
            record.field("input", input)?;
            record.field("base_revision", base_revision)?;
            record.field("element", element)?;
            record.field("phase", encode_animation_phase(*phase))?;
            record.field("preset_id", preset)?;
            record.field("frames", frames)
        }
        CommandPayload::ApplySceneAnimationUserPreset {
            input,
            base_revision,
            layer,
            phase,
            preset,
            frames,
        } => {
            validate_phase_frames(*frames)?;
            record.field("payload", "scene_animation_user_preset")?;
            record.field("input", input)?;
            record.field("base_revision", base_revision)?;
            record.field("layer", layer)?;
            record.field("phase", encode_animation_phase(*phase))?;
            record.field("preset_id", preset)?;
            record.field("frames", frames)
        }
        _ => unreachable!("only animation preset commands are delegated"),
    }
}

/// Writes `preset_id`, `name`, `phase`, and `motion` of one library preset.
fn encode_animation_preset_entry(
    record: &mut Record,
    entry: &AnimationPresetEntry,
) -> Result<(), CodecError> {
    validate_preset_name(&entry.name)?;
    let motion = encode_motion(&entry.motion)?;
    record.field("preset_id", entry.preset)?;
    record.field_str("name", &entry.name)?;
    record.field("phase", encode_animation_phase(entry.phase))?;
    record.field_string("motion", motion)
}

/// Encodes one `animation_preset_state` record (Protocol 2.34).
fn encode_animation_preset_state(
    record: &mut Record,
    message: &AnimationPresetStateMessage,
) -> Result<(), CodecError> {
    record.kind("animation_preset_state");
    record.field("revision", message.revision)?;
    encode_animation_preset_entry(record, &message.entry)
}

/// Encodes the fields of one `animation_preset_changed` event.
fn encode_animation_preset_changed(
    record: &mut Record,
    change: &AnimationPresetChange,
) -> Result<(), CodecError> {
    record.field("event", "animation_preset_changed")?;
    match change {
        AnimationPresetChange::Set { index, entry } => {
            if *index >= MAX_ANIMATION_PRESETS {
                return Err(CodecError::InvalidField {
                    field: "index",
                    value: index.to_string(),
                });
            }
            record.field("change", "set")?;
            record.field("index", index)?;
            encode_animation_preset_entry(record, entry)
        }
        AnimationPresetChange::Removed { preset } => {
            record.field("change", "removed")?;
            record.field("preset_id", preset)
        }
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
    record.field("scene", message.scene)?;
    record.field("animation_revision", message.animation_revision)?;
    record.field_string("clip", clip)
}

fn encode_scene_layout_state(
    record: &mut Record,
    message: &SceneLayoutStateMessage,
) -> Result<(), CodecError> {
    let layers = encode_scene_layers(&message.layout)?;
    if message.name.len() > crate::MAX_SCENE_NAME_BYTES
        || message.name.chars().any(char::is_control)
    {
        return Err(CodecError::InvalidField {
            field: "name",
            value: format!("{} bytes", message.name.len()),
        });
    }
    record.kind("scene_layout_state");
    record.field("revision", message.revision)?;
    record.field("scene", message.scene)?;
    record.field_string("name", message.name.clone())?;
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
            record.field_string("transition", kind.token())?;
            if let Some(duration_frames) = kind.duration_frames() {
                if duration_frames == 0 {
                    return Err(CodecError::InvalidField {
                        field: "duration_frames",
                        value: "0".to_owned(),
                    });
                }
                record.field("duration_frames", duration_frames)?;
            }
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
        pan_basis_points,
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
    record.field("pan_basis_points", pan_basis_points)?;
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
            record.field("transition", transition.token())?;
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
            reason,
        } => {
            record.field("status", "rejected")?;
            record.field_str("id", id)?;
            record.field_str("code", code)?;
            record.field_str("message", message)?;
            record.field_string("fields", field_issues(fields)?)?;
            record.field("current_revision", current_revision)?;
            record.field("retryable", u8::from(*retryable))?;
            if let Some(reason) = reason {
                record.field_string("reason", super::encode_refusal_reason(reason)?)?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // one field per snapshot section
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
    record.field_string(
        "layout_scenes",
        encode_layout_scenes(&message.layout_scenes)?,
    )?;
    record.field_string("frame_rate", encode_frame_rate(message.frame_rate))?;
    record.field("playlist", u8::from(message.playlist_follows))?;
    if message.grade_states > MAX_GRADE_STATES {
        return Err(CodecError::InvalidField {
            field: "grade_states",
            value: message.grade_states.to_string(),
        });
    }
    record.field("grade_states", message.grade_states)?;
    if message.animation_preset_states > MAX_ANIMATION_PRESETS {
        return Err(CodecError::InvalidField {
            field: "animation_preset_states",
            value: message.animation_preset_states.to_string(),
        });
    }
    record.field("animation_preset_states", message.animation_preset_states)?;
    record.field_string(
        "transition_favorites",
        encode_favorites(&message.transition_favorites)?,
    )?;
    if message.media_states > MAX_MEDIA_STATES {
        return Err(CodecError::InvalidField {
            field: "media_states",
            value: message.media_states.to_string(),
        });
    }
    record.field("media_states", message.media_states)?;
    if message.paged_states > crate::MAX_PAGED_STATES {
        return Err(CodecError::InvalidField {
            field: "paged_states",
            value: message.paged_states.to_string(),
        });
    }
    record.field("paged_states", message.paged_states)?;
    if !super::cue::list_count_is_valid(message.cue_list_states) {
        return Err(CodecError::InvalidField {
            field: "cue_list_states",
            value: message.cue_list_states.to_string(),
        });
    }
    record.field("cue_list_states", message.cue_list_states)?;
    if !super::script::state_count_is_valid(message.script_states) {
        return Err(CodecError::InvalidField {
            field: "script_states",
            value: message.script_states.to_string(),
        });
    }
    record.field("script_states", message.script_states)?;
    if !super::shortcut::state_count_is_valid(message.shortcut_states) {
        return Err(CodecError::InvalidField {
            field: "shortcut_states",
            value: message.shortcut_states.to_string(),
        });
    }
    record.field("shortcut_states", message.shortcut_states)?;
    if message.still_states > crate::MAX_STILL_STATES {
        return Err(CodecError::InvalidField {
            field: "still_states",
            value: message.still_states.to_string(),
        });
    }
    record.field("still_states", message.still_states)?;
    if !super::media_list::state_count_is_valid(message.media_list_states) {
        return Err(CodecError::InvalidField {
            field: "media_list_states",
            value: message.media_list_states.to_string(),
        });
    }
    record.field("media_list_states", message.media_list_states)?;
    record.field_string(
        "input_categories",
        super::input_lifecycle::encode_categories(&message.input_categories)?,
    )?;
    super::audio_mixer::encode_status(&message.audio_mixer, &mut |name, value| {
        record.field_string(name, value)
    })?;
    record.field_string(
        "fade_to_black_excluded_outputs",
        super::audio_mixer::encode_excluded_outputs(&message.fade_to_black_excluded_outputs)?,
    )?;
    record.field_string(
        "fade_to_black_output_durations",
        super::audio_mixer::encode_output_durations(&message.fade_to_black_output_durations)?,
    )?;
    record.field("fade_to_black_curve", message.fade_to_black_curve.token())?;
    record.field_string(
        "display_outputs",
        super::display_output::encode_entries(&message.display_outputs)?,
    )?;
    record.field_string(
        "display_output_states",
        super::display_output::encode_states(&message.display_output_states)?,
    )?;
    record.field_string(
        "network_streams",
        super::network_stream::encode_entries(&message.network_streams)?,
    )?;
    record.field_string(
        "capture_inputs",
        super::capture::encode_entries(&message.capture_inputs)?,
    )?;
    record.field_string(
        "system_audio_inputs",
        super::system_audio::encode_entries(&message.system_audio_inputs)?,
    )?;
    record.field_string(
        "stream_targets",
        super::stream_target::encode_entries(&message.stream_targets)?,
    )?;
    record.field_string(
        "live_lan",
        super::live_lan::encode_entry(&message.live_lan)?,
    )?;
    record.field_string(
        "live_lan_status",
        super::live_lan::encode_snapshot_status(&message.live_lan_status)?,
    )
}

/// Writes a grade source as `library=<id>` or `grade=<grade>`.
/// Encodes the live drag of a grade set or clear (Protocol 2.56), if any.
fn encode_grade_gesture(
    record: &mut Record,
    gesture: Option<crate::GradeGesture>,
) -> Result<(), CodecError> {
    let Some(gesture) = gesture else {
        return Ok(());
    };
    record.field("gesture", gesture.token)?;
    record.field("continues", gesture.continues)
}

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
        CommandPayload::SetInputGrade {
            input,
            source,
            gesture,
        } => {
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
                .map_or(Ok(()), |source| encode_grade_source(record, source))?;
            encode_grade_gesture(record, *gesture)
        }
        CommandPayload::SetLayerGrade {
            scene,
            layer,
            source,
            gesture,
        } => {
            record.field(
                "payload",
                if source.is_some() {
                    "grade_layer_set"
                } else {
                    "grade_layer_clear"
                },
            )?;
            record.field("scene", scene)?;
            record.field("layer", layer)?;
            source
                .as_ref()
                .map_or(Ok(()), |source| encode_grade_source(record, source))?;
            encode_grade_gesture(record, *gesture)
        }
        CommandPayload::SetProgramGrade { source, gesture } => {
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
                .map_or(Ok(()), |source| encode_grade_source(record, source))?;
            encode_grade_gesture(record, *gesture)
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
        GradeEntry::Layer {
            scene,
            layer,
            source,
        } => {
            record.field("entry", "layer")?;
            record.field("scene", scene)?;
            record.field("layer", layer)?;
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
        GradeChange::Layer {
            scene,
            layer,
            source,
        } => {
            record.field("change", "layer")?;
            record.field("scene", scene)?;
            record.field("layer", layer)?;
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
                overlay.transition.token(),
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
    let mut encoded = Vec::with_capacity(strips.len());
    for status in strips {
        let entry = format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}",
            status.input,
            status.gain_millidb,
            status.balance_basis_points,
            status.pan_basis_points,
            u8::from(status.muted),
            u8::from(status.soloed),
            u8::from(status.follow_video),
            status.delay_samples,
            super::audio_mixer::encode_strip_group(status.group)?,
        );
        if !(-96_000..=24_000).contains(&status.gain_millidb)
            || !(-10_000..=10_000).contains(&status.balance_basis_points)
            || !(-10_000..=10_000).contains(&status.pan_basis_points)
            || status.delay_samples > 48_000
            || !inputs.insert(status.input.get())
        {
            return Err(CodecError::InvalidField {
                field: "input_audio_strips",
                value: entry,
            });
        }
        encoded.push(entry);
    }
    record.field_string("input_audio_strips", encoded.join(","))
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
        EventPayload::InputAdded {
            input,
            name,
            network_stream,
            capture,
            system_audio,
            still,
        } => {
            validate_new_input_name(name)?;
            record.field("event", "input_added")?;
            record.field("input", input)?;
            record.field_str("name", name)?;
            match (network_stream, capture, system_audio, still) {
                (None, None, None, Some(source)) => {
                    super::still::encode_source(source, &mut |name, value| {
                        record.field_string(name, value)
                    })?;
                }
                (Some(config), None, None, None) => {
                    record.field("kind", "network_stream")?;
                    super::network_stream::encode_config(
                        config,
                        super::network_stream::Secrets::Redacted,
                        &mut |name, value| record.field_string(name, value),
                    )?;
                }
                (None, Some(config), None, None) => {
                    record.field("kind", "capture")?;
                    super::capture::encode_config(config, &mut |name, value| {
                        record.field_string(name, value)
                    })?;
                }
                (None, None, Some(config), None) => {
                    record.field("kind", "system_audio")?;
                    super::system_audio::encode_config(config, &mut |name, value| {
                        record.field_string(name, value)
                    })?;
                }
                (None, None, None, None) => {}
                _ => {
                    return Err(CodecError::InvalidField {
                        field: "kind",
                        value: "more than one input source".to_owned(),
                    });
                }
            }
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
        EventPayload::StreamTargetChanged {
            target,
            entry,
            streams,
        } => {
            record.field("event", "stream_target_changed")?;
            super::stream_target::encode_changed(*target, entry.as_deref(), &mut |name, value| {
                record.field_string(name, value)
            })?;
            record.field_string("streams", stream_statuses(streams)?)?;
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
        EventPayload::CueListChanged(list) => {
            record.field("event", "cue_list_changed")?;
            super::cue::encode_list(list, &mut |name, value| record.field_string(name, value))?;
        }
        EventPayload::CueListRemoved { list } => {
            record.field("event", "cue_list_removed")?;
            super::cue::encode_operation(*list, crate::CueOperation::Back, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        EventPayload::CueRunChanged(change) => {
            record.field("event", "cue_run_changed")?;
            super::cue::encode_operation(
                change.list,
                crate::CueOperation::Back,
                &mut |name, value| record.field_string(name, value),
            )?;
            record.field("frame", change.frame)?;
            record.field_string("state", super::cue::encode_state(&change.state)?)?;
            record.field_string("fired", super::cue::encode_fired(&change.fired))?;
            record.field("program", change.program)?;
            record.field("preview", change.preview)?;
            encode_manual_status(
                record,
                change.manual_transition,
                ManualStatusFields::Unqualified,
            )?;
            encode_fade_to_black_state(
                record,
                change.fade_to_black,
                FadeToBlackStateFields::Unqualified,
            )?;
            encode_overlays(record, "overlays", &change.overlays)?;
            encode_input_audio_strips(record, &change.input_audio_strips)?;
            record.field_string("streams", stream_statuses(&change.streams)?)?;
            record.field_string("recordings", recording_statuses(&change.recordings)?)?;
            record.field_string(
                "playlist",
                super::playlist::encode_playlist_state(change.playlist),
            )?;
            record.field_string("titles", super::cue::encode_titles(&change.titles)?)?;
            record.field_string("media", super::cue::encode_media(&change.media)?)?;
            record.field_string("scripts", super::cue::encode_scripts(&change.scripts)?)?;
        }
        EventPayload::ScriptChanged(status) => {
            record.field("event", "script_changed")?;
            super::script::encode_status(status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        EventPayload::ScriptRemoved { script } => {
            record.field("event", "script_removed")?;
            super::script::validate_script_id(*script)?;
            record.field("script", script)?;
        }
        EventPayload::ScriptRan(change) => {
            record.field("event", "script_ran")?;
            super::script::validate_script_id(change.script)?;
            record.field("script", change.script)?;
            record.field("frame", change.frame)?;
            record.field("actions", change.actions)?;
            record.field("instructions", change.instructions)?;
            record.field_string("run", super::script::encode_run(&change.run)?)?;
            record.field("program", change.program)?;
            record.field("preview", change.preview)?;
            encode_manual_status(
                record,
                change.manual_transition,
                ManualStatusFields::Unqualified,
            )?;
            encode_fade_to_black_state(
                record,
                change.fade_to_black,
                FadeToBlackStateFields::Unqualified,
            )?;
            encode_overlays(record, "overlays", &change.overlays)?;
            encode_input_audio_strips(record, &change.input_audio_strips)?;
            record.field_string("streams", stream_statuses(&change.streams)?)?;
            record.field_string("recordings", recording_statuses(&change.recordings)?)?;
            record.field_string(
                "playlist",
                super::playlist::encode_playlist_state(change.playlist),
            )?;
            record.field_string("titles", super::cue::encode_titles(&change.titles)?)?;
            record.field_string("media", super::cue::encode_media(&change.media)?)?;
        }
        EventPayload::ShortcutChanged(status) => {
            record.field("event", "shortcut_changed")?;
            super::shortcut::encode_status(status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        EventPayload::ShortcutRemoved { shortcut } => {
            record.field("event", "shortcut_removed")?;
            super::shortcut::validate_shortcut_id(*shortcut)?;
            record.field("shortcut", shortcut)?;
        }
        EventPayload::ShortcutFired(change) => {
            record.field("event", "shortcut_fired")?;
            super::shortcut::validate_shortcut_id(change.shortcut)?;
            record.field("shortcut", change.shortcut)?;
            record.field("frame", change.frame)?;
            record.field("actions", change.actions)?;
            record.field("program", change.program)?;
            record.field("preview", change.preview)?;
            encode_manual_status(
                record,
                change.manual_transition,
                ManualStatusFields::Unqualified,
            )?;
            encode_fade_to_black_state(
                record,
                change.fade_to_black,
                FadeToBlackStateFields::Unqualified,
            )?;
            encode_overlays(record, "overlays", &change.overlays)?;
            encode_input_audio_strips(record, &change.input_audio_strips)?;
            record.field_string("streams", stream_statuses(&change.streams)?)?;
            record.field_string("recordings", recording_statuses(&change.recordings)?)?;
            record.field_string(
                "playlist",
                super::playlist::encode_playlist_state(change.playlist),
            )?;
            record.field_string("titles", super::cue::encode_titles(&change.titles)?)?;
            record.field_string("media", super::cue::encode_media(&change.media)?)?;
            record.field_string("scripts", super::cue::encode_scripts(&change.scripts)?)?;
        }
        EventPayload::ScriptRunChanged { script, frame, run } => {
            record.field("event", "script_run_changed")?;
            super::script::validate_script_id(*script)?;
            record.field("script", script)?;
            record.field("frame", frame)?;
            record.field_string("run", super::script::encode_run(run)?)?;
        }
        EventPayload::ScriptRunsStopped { frame, scripts } => {
            record.field("event", "script_runs_stopped")?;
            record.field("frame", frame)?;
            record.field_string("scripts", super::script::encode_script_list(scripts)?)?;
        }
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
        EventPayload::TransitionFavoriteChanged { slot, favorite } => {
            record.field("event", "transition_favorite_changed")?;
            validate_favorite_slot(*slot, "slot")?;
            record.field("slot", slot)?;
            record.field_string("favorite", encode_favorite(*favorite, "favorite")?)?;
        }
        EventPayload::AnimationPresetChanged(change) => {
            encode_animation_preset_changed(record, change)?;
        }
        EventPayload::AudioMixerChanged(operation) => {
            record.field("event", "audio_mixer_changed")?;
            record.field("operation", operation.payload())?;
            super::audio_mixer::encode_operation(operation, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        EventPayload::FadeToBlackOutputChanged { output, affected } => {
            record.field("event", "fade_to_black_output_changed")?;
            record.field("output", output)?;
            record.field("affected", u8::from(*affected))?;
        }
        EventPayload::FadeToBlackOutputDurationChanged {
            output,
            duration_frames,
        } => {
            record.field("event", "fade_to_black_output_duration_changed")?;
            record.field("output", output)?;
            record.field_string(
                "duration_frames",
                super::audio_mixer::encode_optional_duration(*duration_frames)?,
            )?;
        }
        EventPayload::FadeToBlackCurveChanged { curve } => {
            record.field("event", "fade_to_black_curve_changed")?;
            record.field("curve", curve.token())?;
        }
        EventPayload::LiveLanChanged(entry) => {
            record.field("event", "live_lan_changed")?;
            super::live_lan::encode_changed(entry, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        EventPayload::DisplayOutputChanged { slot, config } => {
            record.field("event", "display_output_changed")?;
            super::display_output::encode_changed(*slot, config.as_deref(), &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        EventPayload::CaptureSettingsChanged { input, settings } => {
            record.field("event", "capture_settings_changed")?;
            record.field("input", input)?;
            super::capture::encode_settings(settings, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        EventPayload::NetworkStreamChanged { input, config } => {
            record.field("event", "network_stream_changed")?;
            record.field("input", input)?;
            super::network_stream::encode_config(
                config,
                super::network_stream::Secrets::Redacted,
                &mut |name, value| record.field_string(name, value),
            )?;
        }
        EventPayload::SceneLayoutChanged { edit, layout } => {
            let layers = encode_scene_layers(layout)?;
            record.field("event", "scene_layout_changed")?;
            record.field("operation", edit.operation.payload())?;
            super::scene_layer::encode_scene_layer_edit(edit, &mut |name, value| {
                record.field_string(name, value)
            })?;
            record.field("layout_width", layout.width)?;
            record.field("layout_height", layout.height)?;
            record.field_string("layers", layers)?;
        }
        EventPayload::InputLifecycleChanged {
            operation,
            added,
            refused,
        } => {
            record.field("event", "input_lifecycle_changed")?;
            record.field("operation", operation.payload())?;
            super::input_lifecycle::encode_lifecycle_operation(operation, &mut |name, value| {
                record.field_string(name, value)
            })?;
            record.field_string("added", super::input_lifecycle::encode_added(added)?)?;
            record.field_string("refused", super::input_lifecycle::encode_refused(refused)?)?;
        }
        EventPayload::PageNavigated {
            input,
            frame,
            navigation,
        } => {
            record.field("event", "page_navigated")?;
            record.field("input", input)?;
            record.field("frame", frame)?;
            super::still::encode_navigation(*navigation, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        EventPayload::PagedChanged { navigation, status } => {
            record.field("event", "paged_changed")?;
            super::paged::encode_navigation(*navigation, "target", &mut |name, value| {
                record.field_string(name, value)
            })?;
            super::paged::encode_status(status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        EventPayload::MediaChanged {
            frame,
            operation,
            status,
        } => {
            record.field("event", "media_changed")?;
            record.field("frame", frame)?;
            record.field("operation", operation.token())?;
            encode_media_operation_fields(*operation, &mut |name, value| {
                record.field_string(event_operation_field(name), value)
            })?;
            encode_media_status(status, &mut |name, value| record.field_string(name, value))?;
        }
        EventPayload::MediaListChanged {
            frame,
            operation,
            created,
            status,
        } => {
            record.field("event", "media_list_changed")?;
            record.field("frame", frame)?;
            record.field("operation", operation)?;
            match created {
                Some(name) => {
                    super::validate_new_input_name(name)?;
                    record.field_str("created", name)?;
                }
                None => record.field("created", "-")?,
            }
            super::media_list::encode_status(status, &mut |name, value| {
                record.field_string(name, value)
            })?;
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

#[allow(clippy::too_many_lines)]
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
        RuntimeLifecycleEvent::InputAvailability { input, offline } => {
            record.field("event", "input_availability")?;
            record.field("input", input)?;
            record.field(
                "offline",
                offline.map_or("-", crate::InputOfflineReason::token),
            )?;
        }
        RuntimeLifecycleEvent::DisplayOutputStatus(status) => {
            record.field("event", "display_output_status")?;
            super::display_output::encode_status(status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        RuntimeLifecycleEvent::LiveLanStatus(status) => {
            record.field("event", "live_lan_status")?;
            super::live_lan::encode_status(status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        RuntimeLifecycleEvent::SlideshowPage { input, status } => {
            record.field("event", "slideshow_page")?;
            record.field("input", input)?;
            super::still::encode_page_status(status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        RuntimeLifecycleEvent::SystemAudioStatus { input, status } => {
            record.field("event", "system_audio_status")?;
            super::system_audio::encode_status(*input, status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        RuntimeLifecycleEvent::CaptureStatus { input, status } => {
            record.field("event", "capture_status")?;
            super::capture::encode_status(*input, status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        RuntimeLifecycleEvent::CaptureDevices(inventory) => {
            record.field("event", "capture_devices")?;
            super::capture::encode_inventory(inventory, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        RuntimeLifecycleEvent::NetworkStreamStatus { input, status } => {
            record.field("event", "network_stream_status")?;
            super::network_stream::encode_status(*input, status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        RuntimeLifecycleEvent::DisplayInventory(inventory) => {
            record.field("event", "display_inventory")?;
            super::display_output::encode_inventory(inventory, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        RuntimeLifecycleEvent::Paged { status } => {
            record.field("event", "paged")?;
            super::paged::encode_status(status, &mut |name, value| {
                record.field_string(name, value)
            })?;
        }
        RuntimeLifecycleEvent::Media {
            frame,
            events,
            status,
        } => {
            record.field("event", "media")?;
            record.field("frame", frame)?;
            record.field_string("media_events", encode_media_events(events)?)?;
            encode_media_status(status, &mut |name, value| record.field_string(name, value))?;
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
            record.field_string(kind, state.kind.token())?;
            if let Some(duration_frames) = state.kind.duration_frames() {
                record.field(manual_duration_field(fields), duration_frames)?;
            }
            record.field(from, state.from)?;
            record.field(to, state.to)?;
            record.field(interval_start, state.interval_start.basis_points())?;
            record.field(position, state.position.basis_points())
        }
    }
}

/// The clip-length field a manual Stinger status adds (Protocol 2.48).
const fn manual_duration_field(fields: ManualStatusFields) -> &'static str {
    match fields {
        ManualStatusFields::Desired => "desired_manual_duration_frames",
        ManualStatusFields::Realized => "realized_manual_duration_frames",
        ManualStatusFields::Unqualified => "manual_duration_frames",
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
                "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}{}",
                sample.target,
                stream_realized_state(sample.realized),
                u8::from(sample.connected),
                sample.muxed_bytes,
                sample.enqueued_pairs,
                sample.dropped_pairs,
                sample.reconnects,
                sample.uptime_ms,
                sample.bitrate_kbps,
                sample
                    .failure
                    .map_or_else(String::new, |failure| failure.to_string()),
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

fn encode_input_thumbnail(
    record: &mut Record,
    message: &crate::InputThumbnailMessage,
) -> Result<(), CodecError> {
    super::validate_input_thumbnail(message.width, message.height, message.data.len())?;
    record.kind("input_thumbnail");
    encode_server_identity(record, &message.server)?;
    record.field("input", message.input)?;
    record.field("sequence", message.sequence)?;
    record.field_str("origin", message.origin.as_str())?;
    record.field("age_ms", message.age_ms)?;
    record.field("width", message.width)?;
    record.field("height", message.height)?;
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
    let monitor = match &message.monitor {
        None => "-".to_owned(),
        Some(monitor) => {
            if monitor.channels.len() != channels {
                return Err(CodecError::InvalidField {
                    field: "monitor",
                    value: monitor.channels.len().to_string(),
                });
            }
            format!(
                "{}~{}~{}",
                if monitor.solo { "solo" } else { "master" },
                monitor.output.token(),
                encode_meter_channels(&monitor.channels, "monitor")?
            )
        }
    };
    record.kind("audio_meters");
    encode_server_identity(record, &message.server)?;
    record.field("sequence", message.sequence)?;
    record.field("frame", message.frame)?;
    record.field("start_sample", message.start_sample)?;
    record.field("end_sample", message.end_sample)?;
    record.field_string("master", master)?;
    record.field_string("inputs", inputs.join(";"))?;
    record.field_string("monitor", monitor)?;
    record.field_string(
        "dynamics",
        super::audio_mixer::encode_dynamics(&message.dynamics)?,
    )
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
    record.field("subscriber_queue_limit", message.subscriber_queue_limit)?;
    let sources = super::source_sync::encode_samples("source_sync", &message.source_sync)?;
    record.field_string("source_sync", sources)
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
