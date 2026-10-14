mod cycles;
mod project;
mod resolve;
mod stream;
mod validation;

pub use project::AddInputError;
pub use project::AudioBusSendError;
pub use project::{
    AddAudioBusError, AddOutputError, AddSceneInputError, AddSceneLayerError, AddStreamTargetError,
    AudioBus, BusSend, CURRENT_SCHEMA_VERSION, CropRect, DuplicateSceneInputError, Input,
    InputAudioStrip, InputAudioStripState, InputBalanceBasisPoints, InputDelaySamples,
    InputGainMilliDb, InputKind, Layer, LayerEffect, LayerEffectError, LayerGeometry, MainMix,
    MixFeed, Output, OutputFormat, Project, ProjectSettings, RECORDER_SLOT_COUNT, RecorderSlot,
    RecorderSlotError, RectMask, RelinkMediaInputError, RemoveAudioBusError, RemoveInputError,
    RemoveOutputError, RemoveSceneError, RemoveStreamTargetError, RenameAudioBusError,
    RenameOutputError, RenameProjectError, RenameSceneError, ReplaceInputError, RestartPolicy,
    Rgba8, Rotation, Scene, SceneInputAudioSourceError, SceneLayerError, SchemaVersion,
    SetInputLockError, SetOutputRouteError, SetOutputStartupError, SetSceneBackgroundError,
    SetStingerError, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, SourceRef,
    StartupPolicy, StingerAudioPolicy, StingerConfig, StingerMissingMediaFallback,
    StingerSlotNumber, UpdateStreamTargetError,
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
pub use validation::{EntityRef, ValidationError, ValidationErrorKind};
