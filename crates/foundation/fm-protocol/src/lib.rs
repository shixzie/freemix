//! Transport-neutral wire DTOs and newline-delimited control codec.

mod audio_mixer;
mod audio_output;
mod capture;
mod codec;
mod cue;
mod display_output;
mod dto;
mod health;
mod input_lifecycle;
mod input_thumbnail;
mod live_lan;
mod media;
mod media_list;
mod monitor_image;
mod network_stream;
mod output_failure;
mod paged;
mod recorder_status;
mod reel;
mod replay;
mod scene_layer;
pub mod screen_capture;
mod script;
mod shortcut;
mod source_sync;
mod still;
mod stream_target;
mod system_audio;
mod version;
mod web_access;

pub use fm_types::{
    CUE_GAIN_MAX_MILLIDB, CUE_GAIN_MIN_MILLIDB, CueGain, CueMute, FadeToBlackCurve, FireValue,
    MAX_FADE_TO_BLACK_DURATION_FRAMES,
};

pub use audio_mixer::{
    AUDIO_GROUP_COUNT, AudioDspTarget, AudioDynamicsMeter, AudioGroupStatus, AudioLoudnessMeter,
    AudioMixerOperation, AudioMixerStatus, AudioMonitorStatus, AudioSoloMode,
    AudioStripDspStateMessage, AuxBusStatus, InputAudioLabel, MAX_AUDIO_LABEL_BYTES,
    MAX_AUDIO_MONITOR_DEVICE_BYTES, MAX_AUDIO_STRIP_DSP_STATES, MAX_DYNAMICS_REDUCTION_MILLIDB,
    MAX_LOUDNESS_MILLI, MAX_LOUDNESS_RANGE_MILLILU, MIN_LOUDNESS_MILLI, audio_group_is_valid,
    audio_label_is_valid, audio_monitor_device_is_valid, fade_to_black_affects,
};
pub use audio_output::{
    AudioOutputDevice, AudioOutputDevicesMessage, AudioOutputDevicesRequest, AudioOutputInventory,
    MAX_AUDIO_OUTPUT_CHANNELS, MAX_AUDIO_OUTPUT_DESCRIPTION_BYTES, MAX_AUDIO_OUTPUT_DEVICE_BYTES,
    MAX_AUDIO_OUTPUT_DEVICES, MAX_AUDIO_OUTPUT_ERROR_BYTES, audio_output_device_is_valid,
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
    MAX_STREAM_DETAIL_BYTES, MAX_STREAM_LOCAL_SEGMENTS, MAX_STREAM_LOCAL_TARGET_DURATION_MS,
    MAX_STREAM_SAMPLES, cue_action_token, cue_name_is_valid, decode_line, decode_refusal_reason,
    encode_line, encode_refusal_reason, media_list_status_fits, motion_wire_bytes,
    parse_cue_action_token, parse_shortcut_action_text, scene_clip_wire_bytes,
    scene_layout_wire_bytes, shortcut_action_text, title_clip_wire_bytes, validate_shortcut_spec,
};
pub use cue::{
    CueArmedStatus, CueFaultStatus, CueListSpec, CueListStateMessage, CueListStatus,
    CueMediaTransport, CueOperation, CuePendingStatus, CueRunChange, CueRunStatus, CueScriptRun,
    CueTitleText, CueTitleValue, HIDDEN_TITLE_VALUE, MAX_CUE_ACTIONS, MAX_CUE_ARM_MS,
    MAX_CUE_DELAY_MS, MAX_CUE_LIST_ACTIONS, MAX_CUE_LIST_SCRIPT_STEPS, MAX_CUE_LIST_TEXT_BYTES,
    MAX_CUE_LISTS, MAX_CUE_NAME_BYTES, MAX_CUE_TEXT_BYTES, MAX_CUE_TITLE_NUMBER,
    MAX_CUE_TRANSITION_FRAMES, MAX_CUE_WAIT_FRAMES, MAX_CUES, WireCue, WireCueAction,
    WireCueMediaOperation, WireInputTarget, WireTargetBindings,
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
    AudioMonitorOutputState, AuxBusMeters, CapabilityReportMessage, CapabilityReportSummary,
    ClientType, CommandMessage, CommandPayload, CommandResult, DiagnosticsRequest,
    DiagnosticsResponse, DurableEvent, DurableEventBatch, DurableGap, EditHistoryStatus,
    EditLockScope, EditLockStatus, EngineIdentity, ErrorMessage, EventCursor, EventMessage,
    EventPayload, FadeToBlackPosition, FadeToBlackState, FieldIssue, HandshakeOutcome,
    HandshakeRequest, HandshakeResponse, HeartbeatAcknowledgementMessage, HeartbeatMessage,
    InputAudioMeters, InputAudioStripStatus, InputStatus, LiveInputSource,
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
    TitleAirRequirement, TitleFieldExposure, TitleFieldStatus, TitleFieldUpdate, TitleFieldValue,
    TitlePhase, TitlePhaseStatus, TitlePhasesMessage, TitleShownOn, TitleStateMessage,
    TitleSummary, TitleTimelineStatus, WireInputId, WireMessage, WireOutputId,
    WireOverlayChannelId, WireRecorderSlot, WireStingerSlotId, WireStreamTargetId,
    WireTitleFieldId, choose_handshake_outcome,
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
    GradeChange, GradeEntry, GradeGesture, GradeStateMessage, MAX_GRADE_LAYERS,
    MAX_GRADE_LUT_IMPORT_BYTES, MAX_GRADE_NAME_BYTES, MAX_GRADE_STATES, WireGradeSource,
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
    MAX_WEB_TITLE_PREVIEW_HEIGHT, MAX_WEB_TITLE_PREVIEW_WIDTH, TitleAnimationStateMessage,
    TitleLayoutStateMessage, TitlePreviewFrameMessage, TitlePreviewImage, TitlePreviewRequest,
    TitlePreviewSample, WireTitleClip, WireTitleElementId, WireTitleElementKind,
    WireTitleElementOutline, WireTitleLayout,
};
pub use fm_command::{
    FaultDetail, IdKind, InputUse, MAX_FAULT_MESSAGE_BYTES, MAX_REFUSAL_FIELD_BYTES, RefusalReason,
    ScriptFailureKind,
};
pub use health::{
    AlertReading, AlertRuleDefaults, AlertRuleError, AlertRuleSetting, MAX_ALERT_HOLD_MS,
    check_alert_rule_settings, effective_alert_rule,
};
pub use health::{
    DaemonHealth, HealthAlert, HealthAlertAction, HealthAlertRequest, HealthAlertRule,
    HealthNotification, HealthNotificationKind, HealthSeverity, HealthSilence, HealthStatusMessage,
    MAX_HEALTH_ALERT_HOLD_MS, MAX_HEALTH_NOTIFICATIONS, MAX_UTILIZATION_PERMILLE,
};
pub use input_lifecycle::{
    AddedInput, ImportItemSpec, ImportRefusal, InputCategoryStatus, InputColorLabel,
    InputLifecycleOperation, InputLifecycleStatus, InputOfflineReason, MAX_BATCH_IMPORT_ITEMS,
    MAX_INPUT_CATEGORIES, MAX_INPUT_CATEGORY_NAME_BYTES, MAX_INPUT_SOURCE_BYTES,
    WireGeneratorVideo, WireInputSource,
};
pub use input_thumbnail::{
    InputThumbnailImage, InputThumbnailMessage, InputThumbnailSubscription,
    MAX_INPUT_THUMBNAIL_BYTES, MAX_INPUT_THUMBNAIL_HEIGHT, MAX_INPUT_THUMBNAIL_WIDTH,
    decode_input_thumbnail, input_thumbnail_size_valid,
};
pub use live_lan::{
    DEFAULT_LIVE_LAN_VIEWERS, LiveLanConfig, LiveLanEntry, LiveLanPin, LiveLanPinError,
    LiveLanSource, LiveLanState, LiveLanStatus, MAX_LIVE_LAN_PIN_DIGITS, MAX_LIVE_LAN_URL_BYTES,
    MAX_LIVE_LAN_VIEWERS, MIN_LIVE_LAN_PIN_DIGITS, MIN_LIVE_LAN_VIEWERS, live_lan_pin_is_valid,
    live_lan_url_is_valid,
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
pub use media_list::{
    DEFAULT_MEDIA_LIST_TRANSITION_FRAMES, MAX_MEDIA_LIST_ITEM_ID, MAX_MEDIA_LIST_ITEMS,
    MAX_MEDIA_LIST_STATES, MAX_MEDIA_LIST_TEXT_BYTES, MAX_MEDIA_LIST_TITLE_BYTES,
    MAX_MEDIA_LIST_TRANSITION_FRAMES, MediaListItemLength, MediaListItemSpec, MediaListNavigation,
    MediaListOperation, MediaListOptionsSpec, MediaListStateMessage, MediaListStatus,
    MediaListTransitionKind, MediaListTransitionSpec,
};
pub use monitor_image::{
    MonitorFrameAssembler, MonitorImageError, TitlePreviewAssembler, decode_qoi_rgb,
    decode_qoi_rgba, encode_qoi_rgb, encode_qoi_rgba, monitor_frame_parts, title_preview_parts,
};
pub use network_stream::{
    AudioChannels, GiveUp, ListenEndpoint, ListenTransport, MAX_NETWORK_STREAM_INPUTS,
    MAX_STREAM_ERROR_BYTES, MAX_STREAM_URL_BYTES, NetworkStreamConfig, NetworkStreamEntry,
    NetworkStreamError, NetworkStreamSettings, NetworkStreamState, NetworkStreamStatus,
    ReconnectPolicy, RtspTransport, SecretEdit, SrtKeyLength, StallSlate, StreamMode,
    StreamProtocol, StreamUrl, omt_authority, redact_stream_url, validate_stream_url,
};
pub use output_failure::{OutputFailure, OutputFailureCode};
pub use paged::{
    DocumentFormat, DocumentState, MAX_DOCUMENT_INPUTS, MAX_PAGED_MESSAGE_BYTES,
    MAX_PAGED_NOTES_BYTES, MAX_PAGED_PAGES, MAX_PAGED_STATES, MAX_PAGED_THUMBNAIL_BYTES,
    MAX_PAGED_THUMBNAIL_HEIGHT, MAX_PAGED_THUMBNAIL_WIDTH, MAX_PAGED_TITLE_BYTES, PagedKind,
    PagedPageAnswer, PagedPageContent, PagedPageKey, PagedPageMessage, PagedPageRequest,
    PagedStateMessage, PagedStatus, bounded_paged_text, decode_paged_page, paged_notes_valid,
    paged_thumbnail_size_valid,
};
pub use recorder_status::{
    MAX_RECORDER_STATUS_SAMPLES, RecorderRealizedState, RecorderStatusMessage, RecorderStatusSample,
};
pub use reel::{
    MAX_REEL_CLIPS, MAX_REEL_DISSOLVE_FRAMES, MAX_REEL_ERROR_BYTES, MAX_REEL_EXPORTS,
    MAX_REEL_MUSIC_PATH_BYTES, MAX_REEL_MUSIC_PERCENT, MAX_REEL_NAME_BYTES, MAX_REEL_PATH_BYTES,
    MAX_REEL_TRIM_FRAMES, MAX_REELS, ReelClip, ReelExportState, ReelExportStatus, ReelMusic,
    ReelOperation, ReelSpec, ReelSpecError, ReelStatus, ReelStatusMessage, ReelStatusRequest,
    clip_dissolve_is_valid, reel_error_is_valid, reel_export_path_is_valid, reel_name_is_valid,
};
pub use replay::{
    DEFAULT_REPLAY_RETENTION_SECONDS, MAX_REPLAY_AUTO_RETURN_FRAMES, MAX_REPLAY_EVENT_NAME_BYTES,
    MAX_REPLAY_EVENTS, MAX_REPLAY_INPUTS, MAX_REPLAY_PLAYLIST_ENTRIES, MAX_REPLAY_RECORD_SOURCES,
    MAX_REPLAY_RETENTION_SECONDS, MAX_REPLAY_STEP_FRAMES, MAX_REPLAY_STORAGE_ROOT_BYTES,
    MIN_REPLAY_RETENTION_SECONDS, REPLAY_LAST_SECONDS, ReplayEventStatus, ReplayFeed,
    ReplayOperation, ReplayPlayerStatus, ReplayPlaylistEntry, ReplaySettings, ReplaySettingsError,
    ReplaySourceStatus, ReplaySpeed, ReplayStatusMessage, ReplayStatusRequest, ReplayStorageLevel,
    ReplayStorageStatus, replay_event_name_is_valid, replay_step_is_valid,
    replay_storage_root_is_valid,
};
pub use scene_layer::{MAX_KEY_CHOKE, MAX_KEY_EDGE_SOFTNESS, MAX_KEY_LIGHT_WRAP_WIDTH};
pub use scene_layer::{
    MAX_SCENE_LAYER_EFFECTS, MAX_SCENE_LAYER_TILT_MILLIDEGREES, MIN_SCENE_LAYER_TILT_MILLIDEGREES,
    SCENE_LAYER_ANCHOR_CENTER, SCENE_LAYER_ANCHOR_MAX, SCENE_LAYER_EFFECT_KINDS, SceneLayerEdit,
    SceneLayerOperation, WireChromaKey, WireFillKey, WireKeyDespill, WireKeyRefinement,
    WireLayerCrop, WireLayerEffect, WireLayerGeometry, WireLayerKey, WireLayerMask, WireLumaKey,
};
pub use screen_capture::{
    DEFAULT_SCREEN_CAPTURE_FPS, MAX_SCREEN_CAPTURE_APP_BYTES, MAX_SCREEN_CAPTURE_CROP,
    MAX_SCREEN_CAPTURE_ERROR_BYTES, MAX_SCREEN_CAPTURE_FPS, MAX_SCREEN_CAPTURE_INPUTS,
    MAX_SCREEN_CAPTURE_SCREEN_INDEX, MAX_SCREEN_CAPTURE_SCREEN_NAME_BYTES,
    MAX_SCREEN_CAPTURE_SCREENS, MAX_SCREEN_CAPTURE_TITLE_BYTES, MAX_SCREEN_CAPTURE_WINDOWS,
    MIN_SCREEN_CAPTURE_FPS, PORTAL_BACKEND, ScreenCaptureAudio, ScreenCaptureAudioState,
    ScreenCaptureConfig, ScreenCaptureCrop, ScreenCaptureEntry, ScreenCaptureError,
    ScreenCaptureInventory, ScreenCapturePickRequest, ScreenCaptureState, ScreenCaptureStatus,
    ScreenCaptureTarget, ScreenCaptureTargetsMessage, ScreenCaptureTargetsRequest, ScreenInfo,
    TitleMatch, WindowInfo, WindowRule, bound_screen_capture_error,
};
pub use script::{
    HIDDEN_SCRIPT_SOURCE, MAX_SCRIPT_NAME_BYTES, MAX_SCRIPT_RUNS, MAX_SCRIPT_SOURCE_BYTES,
    MAX_SCRIPTS, ScriptRunChange, ScriptRunStatus, ScriptSpec, ScriptStateMessage, ScriptStatus,
    script_name_is_valid, script_source_is_valid,
};
pub use shortcut::{
    MAX_SHORTCUT_ACTIONS, MAX_SHORTCUT_KEY_BYTES, MAX_SHORTCUT_NAME_BYTES, MAX_SHORTCUT_OSC_BYTES,
    MAX_SHORTCUT_TEXT_BYTES, MAX_SHORTCUTS, SHORTCUT_MOUSE_BUTTONS, SHORTCUT_OSC_PREFIX,
    SHORTCUT_SCOPES, SHORTCUT_TOOL_WINDOWS, SHORTCUT_WINDOW_SUFFIX, ShortcutFireChange,
    ShortcutSpec, ShortcutStateMessage, ShortcutStatus, WireShortcutAction,
    WireShortcutCueOperation, WireShortcutKey, shortcut_key_is_valid, shortcut_name_is_valid,
    shortcut_osc_is_valid, shortcut_scope_is_valid, shortcut_scope_panel, shortcut_scopes_overlap,
};
pub use source_sync::{
    MAX_SOURCE_SYNC_MPPM, MAX_SOURCE_SYNC_RATE_HZ, MAX_SOURCE_SYNC_SAMPLES, SourceClockState,
    SourceSyncKind, SourceSyncSample, SourceSyncStatusMessage,
};
pub use still::{
    ImageInputConfig, MAX_IMAGE_URI_BYTES, MAX_SLIDE_CAPTION_BYTES, MAX_SLIDE_FRAMES,
    MAX_SLIDE_LABEL_BYTES, MAX_SLIDE_TRANSITION_FRAMES, MAX_SLIDESHOW_FOLDER_BYTES,
    MAX_SLIDESHOW_INPUTS, MAX_SLIDESHOW_ITEMS, MAX_SLIDESHOW_ITEMS_BYTES, MAX_SLIDESHOW_SLIDES,
    MAX_SLIDESHOW_SLIDES_BYTES, MAX_SLIDESHOW_SLIDES_PAGE, MAX_STILL_STATES, PagedNavigation,
    SlideOrder, SlideTransitionStyle, SlideshowConfig, SlideshowItem, SlideshowPage,
    SlideshowPageStatus, SlideshowSlide, SlideshowSlidesMessage, SlideshowSlidesRequest,
    SlideshowSource, StillInputError, StillStateMessage, WireStillSource, slide_label_is_valid,
};
pub use stream_target::{
    DEFAULT_STREAM_TARGET_SRT_LATENCY_MS, MAX_STREAM_RECONNECT_ATTEMPTS,
    MAX_STREAM_RECONNECT_DELAY_MS, MAX_STREAM_TARGET_BITRATE_KBPS,
    MAX_STREAM_TARGET_ENDPOINT_BYTES, MAX_STREAM_TARGET_KEY_BYTES, MAX_STREAM_TARGET_LADDER_RUNGS,
    MAX_STREAM_TARGET_NAME_BYTES, MAX_STREAM_TARGET_PASSPHRASE_BYTES,
    MAX_STREAM_TARGET_SRT_LATENCY_MS, MAX_STREAM_TARGETS, MIN_STREAM_RECONNECT_DELAY_MS,
    MIN_STREAM_TARGET_BITRATE_KBPS, MIN_STREAM_TARGET_PASSPHRASE_BYTES,
    MIN_STREAM_TARGET_SRT_LATENCY_MS, StreamReconnect, StreamTargetConfig, StreamTargetEntry,
    StreamTargetProtocol, srt_passphrase_is_valid, stream_key_is_valid,
};
pub use system_audio::{
    MAX_SYSTEM_AUDIO_DESCRIPTION_BYTES, MAX_SYSTEM_AUDIO_DEVICE_BYTES,
    MAX_SYSTEM_AUDIO_DEVICE_CHANNELS, MAX_SYSTEM_AUDIO_DEVICES, MAX_SYSTEM_AUDIO_ERROR_BYTES,
    MAX_SYSTEM_AUDIO_INPUTS, SystemAudioBackend, SystemAudioBackendError, SystemAudioConfig,
    SystemAudioDevice, SystemAudioDevicesMessage, SystemAudioDevicesRequest, SystemAudioEntry,
    SystemAudioError, SystemAudioInventory, SystemAudioState, SystemAudioStatus,
    bound_system_audio_description, bound_system_audio_error, default_system_audio_device,
    validate_system_audio_device,
};
pub use version::{CURRENT_PROTOCOL_VERSION, ProtocolVersion};
pub use web_access::{
    MAX_WEB_DEVICE_ID_BYTES, MAX_WEB_DEVICE_NAME_BYTES, MAX_WEB_DEVICES, MAX_WEB_LINK_CODE_BYTES,
    MAX_WEB_URL_BYTES, MAX_WEB_URLS, MIN_WEB_LINK_CODE_BYTES, WEB_CA_PATH, WEB_PAIRING_CODE_DIGITS,
    WEB_PAIRING_LIFETIME_MS, WebAccessAction, WebAccessMessage, WebAccessRefusal, WebAccessRequest,
    WebCertProblem, WebDeviceEntry, WebDeviceRole, WebGatewayStatus, WebPairing, WebTrust,
    valid_web_device_id, valid_web_device_name, valid_web_fingerprint,
};
