//! Transport-neutral wire DTOs and newline-delimited control codec.

mod audio_mixer;
mod capture;
mod codec;
mod display_output;
mod dto;
mod input_lifecycle;
mod media;
mod monitor_image;
mod network_stream;
mod scene_layer;
mod version;

pub use audio_mixer::{
    AUDIO_GROUP_COUNT, AudioGroupStatus, AudioMixerOperation, AudioMixerStatus, AudioSoloMode,
    InputAudioLabel, MAX_AUDIO_LABEL_BYTES, audio_group_is_valid, audio_label_is_valid,
    fade_to_black_affects,
};
pub use capture::{
    CaptureAudio, CaptureDevice, CaptureDevicesMessage, CaptureDevicesRequest, CaptureError,
    CaptureFormat, CaptureInputConfig, CaptureInputEntry, CaptureInventory, CaptureLossReason,
    CaptureLossSlate, CapturePixelFormat, CaptureScan, CaptureSettings, CaptureSignalState,
    CaptureStatus, MAX_CAPTURE_BACKEND_BYTES, MAX_CAPTURE_DEVICE_FORMATS, MAX_CAPTURE_DEVICES,
    MAX_CAPTURE_ERROR_BYTES, MAX_CAPTURE_INPUTS, MAX_CAPTURE_KEY_BYTES, MAX_CAPTURE_TEXT_BYTES,
    bounded_capture_text, fit_capture_inventory, is_capture_backend_token, validate_stable_key,
    wire_capture_status,
};
pub use codec::{
    CodecError, LineDecoder, MAX_BATCH_EVENTS, MAX_FIELD_NAME_BYTES, MAX_FIELD_VALUE_BYTES,
    MAX_FIELDS_PER_MESSAGE, MAX_LINE_BYTES, MAX_LIST_ITEMS, MAX_MESSAGES_PER_PUSH,
    MAX_STREAM_LOCAL_SEGMENTS, MAX_STREAM_LOCAL_TARGET_DURATION_MS, MAX_STREAM_SAMPLES,
    decode_line, decode_refusal_reason, encode_line, encode_refusal_reason, motion_wire_bytes,
    scene_clip_wire_bytes, scene_layout_wire_bytes, title_clip_wire_bytes,
};
pub use display_output::{
    DisplayId, DisplayInventory, DisplayInventoryEntry, DisplayInventoryMessage,
    DisplayInventoryRequest, DisplayMode, DisplayOutputConfig, DisplayOutputEntry,
    DisplayOutputSlot, DisplayOutputSource, DisplayOutputStatus, DisplaySelector, HdrCapability,
    HdrRequest, HdrState, MAX_DISPLAY_INVENTORY_ERROR_BYTES, MAX_DISPLAY_OUTPUTS,
    MAX_INVENTORY_DISPLAY_MODES, MAX_INVENTORY_DISPLAYS, ModeRequest, WaitReason,
    WireActiveDisplay, WireDisplayOutputState, display_source_is_known,
};
pub use dto::{
    AUDIO_METER_LEVEL_SCALE, AudioMeterChannel, AudioMetersMessage, AudioMonitorMeters,
    CapabilityReportMessage, CapabilityReportSummary, ClientType, CommandMessage, CommandPayload,
    CommandResult, DiagnosticsRequest, DiagnosticsResponse, DurableEvent, DurableEventBatch,
    DurableGap, EditHistoryStatus, EditLockScope, EditLockStatus, EngineIdentity, ErrorMessage,
    EventCursor, EventMessage, EventPayload, FadeToBlackPosition, FadeToBlackState, FieldIssue,
    HandshakeOutcome, HandshakeRequest, HandshakeResponse, HeartbeatAcknowledgementMessage,
    HeartbeatMessage, InputAudioMeters, InputAudioStripStatus, InputStatus, LiveInputSource,
    MAX_AUDIO_METER_CHANNELS, MAX_EDIT_HISTORY_DEPTH, MAX_MONITOR_FRAME_HEIGHT,
    MAX_MONITOR_FRAME_PART_BYTES, MAX_MONITOR_FRAME_PARTS, MAX_MONITOR_FRAME_WIDTH,
    MAX_PLAYLIST_ENTRIES, MAX_PLAYLIST_ENTRY_FRAMES, MAX_PLAYLIST_TRANSITION_FRAMES,
    MAX_TITLE_CONTENT_BYTES, MAX_TITLE_FIELD_NAMES_BYTES, MAX_TITLE_FIELDS, MAX_TITLE_NAME_BYTES,
    MAX_TITLE_TEXT_CHARS, ManualTransitionKind, ManualTransitionPosition, ManualTransitionState,
    ManualTransitionStatus, MonitorFrameMessage, MonitorFrameOrigin, MonitorImage, MonitorTap,
    OutputStatus, OverlayBorderPreset, OverlayPositionPreset, OverlayStatus, OverlayTransitionKind,
    PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistRunState, PlaylistStart,
    PlaylistStateMessage, PlaylistTransitionKind, RecordingStatus, ResumeCursor, Role,
    RuntimeDomainBoundary, RuntimeEventMessage, RuntimeFailureDisposition, RuntimeLifecycleEvent,
    ServerHello, ServerIdentity, SnapshotMessage, SnapshotReason, StingerAudioPolicy,
    StingerMissingMediaFallback, StingerReadiness, StingerStatus, StreamLocalStatus,
    StreamRealizedState, StreamStatus, StreamStatusMessage, StreamStatusSample, StructuredError,
    TitleAirRequirement, TitleFieldStatus, TitleFieldUpdate, TitleFieldValue, TitlePhase,
    TitleStateMessage, TitleSummary, WireInputId, WireMessage, WireOutputId, WireOverlayChannelId,
    WireRecorderSlot, WireStingerSlotId, WireStreamTargetId, WireTitleFieldId,
    choose_handshake_outcome,
};
pub use dto::{
    AnimationPresetChange, AnimationPresetEntry, AnimationPresetStateMessage,
    MAX_ANIMATION_PRESET_BYTES, MAX_ANIMATION_PRESET_NAME_BYTES, MAX_ANIMATION_PRESETS,
};
pub use dto::{
    AutoTransitionKind, MAX_TRANSITION_FAVORITE_DURATION_FRAMES, TRANSITION_FAVORITE_COUNT,
    TransitionFavoriteEffect, TransitionFavoriteStatus,
};
pub use dto::{
    GradeChange, GradeEntry, GradeStateMessage, MAX_GRADE_LAYERS, MAX_GRADE_LUT_IMPORT_BYTES,
    MAX_GRADE_NAME_BYTES, MAX_GRADE_STATES, WireGradeSource,
};
pub use dto::{
    MAX_SCENE_ANIMATION_BYTES, MAX_SCENE_CANVAS_SIDE, MAX_SCENE_ENABLED_LAYERS,
    MAX_SCENE_LAYER_NAME_BYTES, MAX_SCENE_LAYOUT_BYTES, MAX_SCENE_LAYOUT_LAYERS,
    MAX_SCENE_NAME_BYTES, SceneAnimationStateMessage, SceneLayoutStateMessage,
    ScenePreviewFrameMessage, ScenePreviewImage, ScenePreviewRequest, WireSceneClip, WireSceneId,
    WireSceneLayerId, WireSceneLayerOutline, WireSceneLayerSource, WireSceneLayout,
};
pub use dto::{
    MAX_TITLE_ANIMATION_BYTES, MAX_TITLE_CANVAS_SIDE, MAX_TITLE_ELEMENT_NAME_BYTES,
    MAX_TITLE_LAYOUT_BYTES, MAX_TITLE_LAYOUT_ELEMENTS, MAX_TITLE_PREVIEW_HEIGHT,
    MAX_TITLE_PREVIEW_PART_BYTES, MAX_TITLE_PREVIEW_PARTS, MAX_TITLE_PREVIEW_WIDTH,
    TitleAnimationStateMessage, TitleLayoutStateMessage, TitlePreviewFrameMessage,
    TitlePreviewImage, TitlePreviewRequest, TitlePreviewSample, WireTitleClip, WireTitleElementId,
    WireTitleElementKind, WireTitleElementOutline, WireTitleLayout,
};
pub use fm_command::{IdKind, InputUse, MAX_REFUSAL_FIELD_BYTES, RefusalReason};
pub use input_lifecycle::{
    AddedInput, ImportItemSpec, ImportRefusal, InputCategoryStatus, InputColorLabel,
    InputLifecycleOperation, InputLifecycleStatus, InputOfflineReason, MAX_BATCH_IMPORT_ITEMS,
    MAX_INPUT_CATEGORIES, MAX_INPUT_CATEGORY_NAME_BYTES, MAX_INPUT_SOURCE_BYTES,
    WireGeneratorVideo, WireInputSource,
};
pub use media::{
    MAX_MEDIA_AUDIO_TRACKS, MAX_MEDIA_END_TRIGGER_LEAD_FRAMES, MAX_MEDIA_EVENTS, MAX_MEDIA_FRAME,
    MAX_MEDIA_IDLE_COMPLETION_REFRESHES, MAX_MEDIA_LIVE_EDGE_OFFSET_FRAMES,
    MAX_MEDIA_SPEED_BASIS_POINTS, MAX_MEDIA_STATES, MAX_MEDIA_TRACK_LANGUAGE_BYTES,
    MAX_MEDIA_TRACK_TITLE_BYTES, MIN_MEDIA_LIVE_EDGE_OFFSET_FRAMES, MIN_MEDIA_SPEED_BASIS_POINTS,
    MediaAudioPolicy, MediaAudioTrack, MediaEndAction, MediaEvent, MediaFault, MediaGrowingStatus,
    MediaOperation, MediaSeekTarget, MediaSettingsStatus, MediaStateMessage, MediaStatus,
    MediaTime, MediaTransportState, MediaTransportStatus,
};
pub use monitor_image::{
    MonitorFrameAssembler, MonitorImageError, TitlePreviewAssembler, decode_qoi_rgb,
    decode_qoi_rgba, encode_qoi_rgb, encode_qoi_rgba, monitor_frame_parts, title_preview_parts,
};
pub use network_stream::{
    GiveUp, MAX_NETWORK_STREAM_INPUTS, MAX_STREAM_ERROR_BYTES, MAX_STREAM_URL_BYTES,
    NetworkStreamConfig, NetworkStreamEntry, NetworkStreamError, NetworkStreamSettings,
    NetworkStreamState, NetworkStreamStatus, ReconnectPolicy, RtspTransport, StallSlate,
    StreamProtocol, StreamUrl, redact_stream_url, validate_stream_url,
};
pub use scene_layer::{
    MAX_SCENE_LAYER_EFFECTS, MAX_SCENE_LAYER_TILT_MILLIDEGREES, MIN_SCENE_LAYER_TILT_MILLIDEGREES,
    SCENE_LAYER_ANCHOR_CENTER, SCENE_LAYER_ANCHOR_MAX, SCENE_LAYER_EFFECT_KINDS, SceneLayerEdit,
    SceneLayerOperation, WireChromaKey, WireLayerCrop, WireLayerEffect, WireLayerGeometry,
    WireLayerKey, WireLayerMask, WireLumaKey,
};
pub use version::{CURRENT_PROTOCOL_VERSION, ProtocolVersion};
