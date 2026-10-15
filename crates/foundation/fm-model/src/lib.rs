mod cycles;
mod grade;
mod playlist;
mod project;
mod resolve;
mod stream;
mod title;
mod validation;

pub use grade::{
    GradeEditError, GradeId, GradeSource, LutId, MAX_GRADE_NAME_BYTES, MAX_LUT_FILE_BYTES,
    MAX_LUT_NAME_BYTES, MAX_PROJECT_GRADES, MAX_PROJECT_LUTS, ProjectGrade, ProjectGrading,
    ProjectLut, lut_asset_key, lut_asset_uri, lut_id_from_uri, next_free_id,
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
    AudioBus, BusSend, CURRENT_SCHEMA_VERSION, CropRect, DuplicateSceneInputError, Input,
    InputAudioStrip, InputAudioStripState, InputBalanceBasisPoints, InputDelaySamples,
    InputGainMilliDb, InputKind, InputReference, Layer, LayerEffect, LayerEffectError,
    LayerGeometry, MainMix, MixFeed, Output, OutputFormat, Project, ProjectSettings,
    RECORDER_SLOT_COUNT, RecorderSlot, RecorderSlotError, RectMask, RelinkMediaInputError,
    RemoveAudioBusError, RemoveInputError, RemoveOutputError, RemoveSceneError,
    RemoveStreamTargetError, RenameAudioBusError, RenameOutputError, RenameProjectError,
    RenameSceneError, ReplaceInputError, RestartPolicy, Rgba8, Rotation, Scene,
    SceneInputAudioSourceError, SceneLayerError, SchemaVersion, SetInputLockError,
    SetOutputRouteError, SetOutputStartupError, SetSceneBackgroundError, SetStingerError,
    SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, SourceRef, StartupPolicy,
    StingerAudioPolicy, StingerConfig, StingerMissingMediaFallback, StingerSlotNumber,
    UpdateStreamTargetError,
};
pub use resolve::{
    AudioRoute, FrameContext, LayerSource, MixFeedHistory, MixSelection, RenderSource,
    ResolveError, ResolvedInput, ResolvedLayer, ResolvedScene, SourceResolver,
};
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
pub use validation::{EntityRef, ValidationError, ValidationErrorKind};
