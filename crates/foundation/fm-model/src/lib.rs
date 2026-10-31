mod animation_preset;
mod audio_mixer;
mod aux_bus;
mod cue;
mod cycles;
mod display_output;
mod grade;
mod input_lifecycle;
mod layer_key;
mod media_list;
mod media_playback;
mod playlist;
mod project;
mod resolve;
mod scene_animation;
mod scene_layer_edit;
mod script;
mod shortcut;
mod stream;
mod title;
mod transition_favorite;
mod validation;

pub use animation_preset::{
    AnimationPresetError, AnimationPresetId, AnimationPresetLibrary,
    MAX_ANIMATION_PRESET_NAME_BYTES, MAX_PROJECT_ANIMATION_PRESETS, UserAnimationPreset,
};
pub use audio_mixer::{
    AUDIO_GROUP_COUNT, AudioGroup, AudioGroupNumber, AudioGroups, AudioLabelError,
    AudioMonitorDeviceError, AudioMonitorOutput, AudioSoloMode, MAX_AUDIO_LABEL_BYTES,
    MAX_AUDIO_MONITOR_DEVICE_BYTES, validate_audio_group_name, validate_audio_monitor_device,
    validate_audio_strip_label,
};
pub use aux_bus::{
    AUX_BUS_COUNT, AUX_OMT_BASE_PORT, AudioBusMask, AudioSends, AudioSendsError, AuxBus,
    AuxBusNumber, AuxBuses, BusSendState, SendTap, aux_omt_port,
};
pub use cue::{
    CUE_OVERLAY_CHANNELS, Cue, CueAction, CueArmed, CueFault, CueList, CueListError,
    CueMediaOperation, CueNumberUpdate, CuePending, CueRunState, MAX_CUE_ACTIONS, MAX_CUE_ARM_MS,
    MAX_CUE_DELAY_MS, MAX_CUE_LIST_ACTIONS, MAX_CUE_LIST_SCRIPT_STEPS, MAX_CUE_LIST_TEXT_BYTES,
    MAX_CUE_LISTS, MAX_CUE_NAME_BYTES, MAX_CUE_TEXT_BYTES, MAX_CUE_TITLE_NUMBER,
    MAX_CUE_TRANSITION_FRAMES, MAX_CUE_WAIT_FRAMES, MAX_CUES, ResolvedCueAction,
    cue_action_is_valid, cue_chain_actions, cue_name_is_valid, delay_frames, title_text_number,
    validate_cue_list, validate_cue_list_definition, validate_cue_lists,
};
pub use display_output::{
    MAX_DISPLAY_OUTPUTS, SetDisplayOutputError, check_display_output, display_selector_is_valid,
};
pub use fm_animation;
pub use fm_types::{
    CUE_GAIN_MAX_MILLIDB, CUE_GAIN_MIN_MILLIDB, CueGain, CueMute, FireValue, InputTarget,
    TargetBindings,
};
pub use grade::{
    GradeEditError, GradeId, GradeSource, LutId, MAX_GRADE_NAME_BYTES, MAX_LUT_FILE_BYTES,
    MAX_LUT_NAME_BYTES, MAX_PROJECT_GRADES, MAX_PROJECT_LAYER_GRADES, MAX_PROJECT_LUTS,
    ProjectGrade, ProjectGrading, ProjectLut, SceneLayerRef, lut_asset_key, lut_asset_uri,
    lut_id_from_uri, next_free_id,
};
pub use input_lifecycle::{
    AUDIO_FILE_EXTENSIONS, BatchImportItem, BatchImportPlan, BatchImportRefusal,
    BatchImportRequest, GeneratorVideo, InputCategory, InputCategoryId, InputColorLabel,
    InputLifecycle, InputLifecycleError, InputSourceError, InputSourceSpec, MAX_BATCH_IMPORT_ITEMS,
    MAX_INPUT_CATEGORIES, MAX_INPUT_CATEGORY_NAME_BYTES, MAX_INPUT_SOURCE_BYTES,
    MEDIA_ASSET_URI_PREFIX, MEDIA_FILE_EXTENSIONS, is_audio_file_name, is_document_file_name,
    is_importable_file_name, is_media_file_name, media_input_name, plan_batch_import,
    unique_input_name, validate_input_categories, validate_input_category_name,
    validate_media_asset_uri,
};
pub use layer_key::{
    ChromaLayerKey, FillLayerKey, KeyChannel, KeyDespill, KeyRefinement, LayerKey, LayerKeyError,
    LumaLayerKey, MAX_KEY_CHOKE, MAX_KEY_EDGE_SOFTNESS, MAX_KEY_LIGHT_WRAP_WIDTH,
};
pub use media_list::{
    DEFAULT_MEDIA_LIST_TRANSITION_FRAMES, M3uEntry, MAX_M3U_BYTES, MAX_MEDIA_LIST_ITEM_ID,
    MAX_MEDIA_LIST_ITEMS, MAX_MEDIA_LIST_TEXT_BYTES, MAX_MEDIA_LIST_TITLE_BYTES,
    MAX_MEDIA_LIST_TRANSITION_FRAMES, MAX_MEDIA_LIST_WIRE_TEXT_BYTES, MAX_MEDIA_LISTS, MediaList,
    MediaListError, MediaListItem, MediaListOptions, MediaListTransition, MediaListTransitionKind,
    is_m3u_file_name, is_media_list_file_name, media_list_play_order, media_list_title_is_valid,
    media_list_wire_text_bytes, parse_m3u, validate_media_list_items, validate_media_list_options,
};
pub use media_playback::{
    DEFAULT_MEDIA_LIVE_EDGE_OFFSET_FRAMES, MAX_MEDIA_END_TRIGGER_LEAD_FRAMES, MAX_MEDIA_FRAME,
    MAX_MEDIA_IDLE_COMPLETION_REFRESHES, MAX_MEDIA_LIVE_EDGE_OFFSET_FRAMES,
    MAX_MEDIA_SPEED_BASIS_POINTS, MEDIA_SPEED_NORMAL_BASIS_POINTS,
    MIN_MEDIA_LIVE_EDGE_OFFSET_FRAMES, MIN_MEDIA_SPEED_BASIS_POINTS, MediaAudioSpeedPolicy,
    MediaEndAction, MediaGrowing, MediaPlaybackSettings, MediaPlaybackSettingsError,
    SetCaptureSettingsError, SetMediaPlaybackError,
};
pub use playlist::{
    MAX_PLAYLIST_ENTRIES, MAX_PLAYLIST_ENTRY_FRAMES, MAX_PLAYLIST_TRANSITION_FRAMES, Playlist,
    PlaylistEntry, PlaylistEntryError, PlaylistLoop, PlaylistRunState, PlaylistTransition,
    PlaylistTransitionKind, validate_playlist_entries,
};
pub use project::AddInputError;
pub use project::AudioBusSendError;
pub use project::{
    AddAudioBusError, AddOutputError, AddSceneInputError, AddSceneLayerError, AddStreamTargetError,
    AudioBus, BusSend, CURRENT_SCHEMA_VERSION, CropRect, DocumentInput, DuplicateInputError,
    DuplicateSceneInputError, Input, InputAudioStrip, InputAudioStripState,
    InputBalanceBasisPoints, InputDelaySamples, InputGainMilliDb, InputKind, InputPanBasisPoints,
    InputReference, Layer, LayerAnchor, LayerEffect, LayerEffectError, LayerGeometry,
    MAX_CRAWL_DOCUMENT_BYTES, MAX_CRAWLS, MAX_PREVIEW_DOCUMENT_BYTES, MAX_PREVIEW_STATES,
    MAX_PTZ_PRESETS_DOCUMENT_BYTES, MAX_SCOREBOARD_DOCUMENT_BYTES, MAX_SCOREBOARDS,
    MAX_SOURCE_CROP_SIDE, MainMix, MixFeed, Output, OutputFormat, PreviewDocumentError, Project,
    ProjectCrawlError, ProjectSettings, PtzPresetsDocumentError, RECORDER_SLOT_COUNT, RecorderSlot,
    RecorderSlotError, RectMask, RelinkMediaInputError, RemoveAudioBusError, RemoveInputError,
    RemoveOutputError, RemoveSceneError, RemoveStreamTargetError, RenameAudioBusError,
    RenameOutputError, RenameProjectError, RenameSceneError, ReplaceInputError, RestartPolicy,
    Rgba8, Rotation, Scene, SceneInputAudioSourceError, SceneLayerError, SchemaVersion,
    ScoreboardDocumentError, SetAudioMixerError, SetInputLockError, SetOutputRouteError,
    SetOutputStartupError, SetSceneAnimationError, SetSceneBackgroundError, SetSceneLayersError,
    SetStingerError, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, SourceRef,
    StartupPolicy, StingerAudioPolicy, StingerConfig, StingerMissingMediaFallback,
    StingerSlotNumber, UpdateStreamTargetError,
};
pub use resolve::{
    AudioRoute, FrameContext, LayerSource, MixFeedHistory, MixSelection, RenderSource,
    ResolveError, ResolvedInput, ResolvedLayer, ResolvedScene, SourceResolver,
};
pub use scene_animation::{
    LayerId, LayerPose, SCENE_ANIMATION_PROPERTIES, SceneAnimation, SceneAnimationError, SceneClip,
    apply_pose, default_scene_timebase, footprint, pose_layer, resolve_anchor, scene_animates,
};
pub use scene_layer_edit::{SceneLayerChange, SceneLayerEditError, check_scene_layer};
pub use script::{
    MAX_QUEUED_SCRIPT_RUNS, MAX_SCRIPT_NAME_BYTES, MAX_SCRIPT_RUNS, MAX_SCRIPT_SOURCE_BYTES,
    MAX_SCRIPT_WAIT_SECONDS, MAX_SCRIPT_WAITS, MAX_SCRIPTS, Script, ScriptError, ScriptFault,
    ScriptRun, ScriptRunState, script_limits, script_name_is_valid, script_source_is_valid,
    script_wait_frames, script_waits, validate_script_definition, validate_script_run,
    validate_scripts,
};
pub use shortcut::{
    MAX_SHORTCUT_ACTIONS, MAX_SHORTCUT_KEY_BYTES, MAX_SHORTCUT_NAME_BYTES, MAX_SHORTCUT_OSC_BYTES,
    MAX_SHORTCUT_TEXT_BYTES, MAX_SHORTCUTS, SHORTCUT_MOUSE_BUTTONS, SHORTCUT_OSC_PREFIX,
    SHORTCUT_SCOPES, SHORTCUT_TOOL_WINDOWS, SHORTCUT_WINDOW_SUFFIX, Shortcut, ShortcutAction,
    ShortcutCueOperation, ShortcutError, ShortcutKey, shortcut_name_is_valid,
    shortcut_osc_is_valid, shortcut_scope_is_valid, shortcut_scope_panel, shortcut_scopes_overlap,
    validate_shortcut_action, validate_shortcut_definition, validate_shortcuts,
};
pub use stream::{
    DEFAULT_STREAM_SRT_LATENCY_MS, DEFAULT_VIDEO_BITRATE_KBPS, MAX_STREAM_ENDPOINT_BYTES,
    MAX_STREAM_KEY_BYTES, MAX_STREAM_LADDER_HEIGHT, MAX_STREAM_LADDER_RUNGS,
    MAX_STREAM_LADDER_WIDTH, MAX_STREAM_RECONNECT_ATTEMPTS, MAX_STREAM_RECONNECT_DELAY_MS,
    MAX_STREAM_SRT_LATENCY_MS, MAX_STREAM_SRT_PASSPHRASE_BYTES, MAX_STREAM_TARGET_NAME_BYTES,
    MAX_STREAM_TARGETS, MAX_VIDEO_BITRATE_KBPS, MIN_STREAM_KEY_BYTES, MIN_STREAM_LADDER_DIMENSION,
    MIN_STREAM_LADDER_RUNGS, MIN_STREAM_RECONNECT_DELAY_MS, MIN_STREAM_SRT_LATENCY_MS,
    MIN_STREAM_SRT_PASSPHRASE_BYTES, MIN_VIDEO_BITRATE_KBPS, REDACTED_STREAM_KEY, SrtPassphrase,
    SrtPassphraseError, StreamEndpoint, StreamEndpointError, StreamKey, StreamKeyError,
    StreamLadder, StreamLadderError, StreamLadderRung, StreamProtocol, StreamReconnectError,
    StreamReconnectPolicy, StreamTarget, StreamTargetError, StreamTargetId,
    StreamVideoBitrateError,
};
pub use title::{
    MAX_TITLE_CONTENT_BYTES, MAX_TITLE_FIELDS, MAX_TITLE_TEXT_CHARS, SetTitleFieldsError,
    TitleFieldEntry, TitleFieldId, TitleFieldValue, TitleFieldsError, TitleInput, TitleShownOn,
    TitleTimeline, validate_title_fields,
};
pub use transition_favorite::{
    DEFAULT_TRANSITION_FAVORITE_DURATION_FRAMES, MAX_TRANSITION_FAVORITE_DURATION_FRAMES,
    TRANSITION_FAVORITE_COUNT, TransitionEffect, TransitionFavorite,
    TransitionFavoriteDurationError, TransitionFavorites,
};
pub use validation::{EntityRef, ValidationError, ValidationErrorKind};
