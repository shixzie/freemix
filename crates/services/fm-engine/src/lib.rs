//! Deterministic composition root for the simulated `FreeMix` engine.

mod animation_preset;
mod engine;
mod error;
mod grade;
mod grade_edit;
mod history;
mod media;
mod playlist;
mod scene_animation;
mod state;
mod title_animation;

pub use animation_preset::EngineAnimationPresetEdit;
pub use engine::{
    Engine, EngineAcceptance, EngineCommand, EngineCommandOutcome, EngineEvent,
    EngineFadeToBlackState, EngineManualTransitionKind, EngineManualTransitionPosition,
    EngineManualTransitionState, EnginePrepareOutcome, EngineRestoreState, EngineSnapshot,
    EngineStreamStatus, EngineTitleAir, FrameResult, MAX_INPUT_AUDIO_BALANCE_BASIS_POINTS,
    MAX_INPUT_AUDIO_DELAY_SAMPLES, MAX_INPUT_AUDIO_GAIN_MILLIDB, MAX_RETAINED_RECEIPTS,
    MIN_INPUT_AUDIO_BALANCE_BASIS_POINTS, MIN_INPUT_AUDIO_GAIN_MILLIDB, PreparedEngineExecution,
    TitleAirRequirement, TitleCue, TitleCueAction,
};
pub use error::{
    EngineError, EngineMediaError, InputCatalogueError, ShowError, SnapshotError, TitleRejection,
    TitleRejectionKind,
};
pub use fm_playback;
pub use grade::{CompiledProjectGrades, GradeCompileError, GradeTarget};
pub use grade_edit::{EngineGradeEdit, EngineLut};
pub use history::{
    EditHistoryEntry, EditLockScope, EditRejection, EditRejectionKind, EngineCommandKind,
    MAX_EDIT_HISTORY,
};
pub use media::{
    EngineMediaCommand, EngineMediaFault, EngineMediaFrame, EngineMediaOperation, EngineMediaSeek,
    EngineMediaSource, EngineMediaStatus, MAX_MEDIA_TRACK_LANGUAGE_BYTES,
    MAX_MEDIA_TRACK_TITLE_BYTES, MAX_MEDIA_TRACKS, SYNTHETIC_GROWING_START_SECONDS,
    SYNTHETIC_MEDIA_SECONDS,
};
pub use playlist::{
    EnginePlaylistOperation, EnginePlaylistStart, MAX_PLAYLIST_TRANSITION_FRAMES, PlaylistCue,
    PlaylistRestoreError, playlist_due_frame, playlist_from_model, playlist_to_model,
};
pub use scene_animation::{
    EngineScene, EngineSceneAir, EngineSceneAnimationEdit, SceneAnimationChange, SceneClipBound,
    scene_inputs_sharing,
};
pub use state::{
    EngineInputAudioStripState, EngineInputSource, EngineTitle, MAX_SHOW_INPUTS, ShowState,
};
pub use title_animation::{
    EngineTitleAnimationEdit, EngineTitleTemplate, TitleAnimationChange, TitleClipBound,
    title_template_inputs,
};

// Persisted title inputs (`fm_model`) and live title field sets (`fm_titles`)
// must agree on every bound, or a project could hold values the engine
// refuses to restore, or the engine could accept values the project cannot
// save.
const _: () = {
    assert!(fm_model::MAX_TITLE_FIELDS == fm_titles::MAX_TITLE_FIELDS);
    assert!(fm_model::MAX_TITLE_TEXT_CHARS == fm_titles::MAX_TITLE_TEXT_CHARS);
    assert!(fm_model::MAX_TITLE_CONTENT_BYTES == fm_titles::MAX_TITLE_CONTENT_BYTES);
};
