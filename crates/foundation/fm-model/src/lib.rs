mod animation_preset;
mod audio_mixer;
mod cycles;
mod display_output;
mod grade;
mod input_lifecycle;
mod layer_key;
mod media_playback;
mod playlist;
mod project;
mod resolve;
mod scene_animation;
mod scene_layer_edit;
mod stream;
mod title;
mod transition_favorite;
mod validation;

pub use animation_preset::{
    AnimationPresetError, AnimationPresetId, AnimationPresetLibrary,
    MAX_ANIMATION_PRESET_NAME_BYTES, MAX_PROJECT_ANIMATION_PRESETS, UserAnimationPreset,
};
pub use audio_mixer::{
    AUDIO_GROUP_COUNT, AudioGroup, AudioGroupNumber, AudioGroups, AudioLabelError, AudioSoloMode,
    MAX_AUDIO_LABEL_BYTES, validate_audio_group_name, validate_audio_strip_label,
};
pub use display_output::{
    MAX_DISPLAY_OUTPUTS, SetDisplayOutputError, check_display_output, display_selector_is_valid,
};
pub use fm_animation;
pub use grade::{
    GradeEditError, GradeId, GradeSource, LutId, MAX_GRADE_NAME_BYTES, MAX_LUT_FILE_BYTES,
    MAX_LUT_NAME_BYTES, MAX_PROJECT_GRADES, MAX_PROJECT_LAYER_GRADES, MAX_PROJECT_LUTS,
    ProjectGrade, ProjectGrading, ProjectLut, SceneLayerRef, lut_asset_key, lut_asset_uri,
    lut_id_from_uri, next_free_id,
};
pub use input_lifecycle::{
    BatchImportItem, BatchImportPlan, BatchImportRefusal, BatchImportRequest, GeneratorVideo,
    InputCategory, InputCategoryId, InputColorLabel, InputLifecycle, InputLifecycleError,
    InputSourceError, InputSourceSpec, MAX_BATCH_IMPORT_ITEMS, MAX_INPUT_CATEGORIES,
    MAX_INPUT_CATEGORY_NAME_BYTES, MAX_INPUT_SOURCE_BYTES, MEDIA_ASSET_URI_PREFIX,
    MEDIA_FILE_EXTENSIONS, is_media_file_name, media_input_name, plan_batch_import,
    unique_input_name, validate_input_categories, validate_input_category_name,
    validate_media_asset_uri,
};
pub use layer_key::{ChromaLayerKey, LayerKey, LayerKeyError, LumaLayerKey};
pub use media_playback::{
    DEFAULT_MEDIA_LIVE_EDGE_OFFSET_FRAMES, MAX_MEDIA_END_TRIGGER_LEAD_FRAMES, MAX_MEDIA_FRAME,
    MAX_MEDIA_IDLE_COMPLETION_REFRESHES, MAX_MEDIA_LIVE_EDGE_OFFSET_FRAMES,
    MAX_MEDIA_SPEED_BASIS_POINTS, MEDIA_SPEED_NORMAL_BASIS_POINTS,
    MIN_MEDIA_LIVE_EDGE_OFFSET_FRAMES, MIN_MEDIA_SPEED_BASIS_POINTS, MediaAudioSpeedPolicy,
    MediaEndAction, MediaGrowing, MediaPlaybackSettings, MediaPlaybackSettingsError,
    SetMediaPlaybackError,
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
    AudioBus, BusSend, CURRENT_SCHEMA_VERSION, CropRect, DuplicateInputError,
    DuplicateSceneInputError, Input, InputAudioStrip, InputAudioStripState,
    InputBalanceBasisPoints, InputDelaySamples, InputGainMilliDb, InputKind, InputPanBasisPoints,
    InputReference, Layer, LayerAnchor, LayerEffect, LayerEffectError, LayerGeometry, MainMix,
    MixFeed, Output, OutputFormat, Project, ProjectSettings, RECORDER_SLOT_COUNT, RecorderSlot,
    RecorderSlotError, RectMask, RelinkMediaInputError, RemoveAudioBusError, RemoveInputError,
    RemoveOutputError, RemoveSceneError, RemoveStreamTargetError, RenameAudioBusError,
    RenameOutputError, RenameProjectError, RenameSceneError, ReplaceInputError, RestartPolicy,
    Rgba8, Rotation, Scene, SceneInputAudioSourceError, SceneLayerError, SchemaVersion,
    SetAudioMixerError, SetInputLockError, SetOutputRouteError, SetOutputStartupError,
    SetSceneAnimationError, SetSceneBackgroundError, SetSceneLayersError, SetStingerError,
    SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, SourceRef, StartupPolicy,
    StingerAudioPolicy, StingerConfig, StingerMissingMediaFallback, StingerSlotNumber,
    UpdateStreamTargetError,
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
pub use stream::{
    DEFAULT_VIDEO_BITRATE_KBPS, MAX_STREAM_ENDPOINT_BYTES, MAX_STREAM_KEY_BYTES,
    MAX_STREAM_LADDER_HEIGHT, MAX_STREAM_LADDER_RUNGS, MAX_STREAM_LADDER_WIDTH,
    MAX_STREAM_TARGET_NAME_BYTES, MAX_VIDEO_BITRATE_KBPS, MIN_STREAM_KEY_BYTES,
    MIN_STREAM_LADDER_DIMENSION, MIN_STREAM_LADDER_RUNGS, MIN_VIDEO_BITRATE_KBPS,
    REDACTED_STREAM_KEY, StreamEndpoint, StreamEndpointError, StreamKey, StreamKeyError,
    StreamLadder, StreamLadderError, StreamLadderRung, StreamProtocol, StreamTarget,
    StreamTargetError, StreamTargetId, StreamVideoBitrateError,
};
pub use title::{
    MAX_TITLE_CONTENT_BYTES, MAX_TITLE_FIELDS, MAX_TITLE_TEXT_CHARS, SetTitleFieldsError,
    TitleFieldEntry, TitleFieldId, TitleFieldValue, TitleFieldsError, TitleInput,
    validate_title_fields,
};
pub use transition_favorite::{
    DEFAULT_TRANSITION_FAVORITE_DURATION_FRAMES, MAX_TRANSITION_FAVORITE_DURATION_FRAMES,
    TRANSITION_FAVORITE_COUNT, TransitionEffect, TransitionFavorite,
    TransitionFavoriteDurationError, TransitionFavorites,
};
pub use validation::{EntityRef, ValidationError, ValidationErrorKind};
