//! Deterministic composition root for the simulated `FreeMix` engine.

mod engine;
mod error;
mod history;
mod playlist;
mod state;

pub use engine::{
    Engine, EngineAcceptance, EngineCommand, EngineCommandOutcome, EngineEvent,
    EngineFadeToBlackState, EngineManualTransitionKind, EngineManualTransitionPosition,
    EngineManualTransitionState, EnginePrepareOutcome, EngineRestoreState, EngineSnapshot,
    EngineStreamStatus, FrameResult, MAX_INPUT_AUDIO_BALANCE_BASIS_POINTS,
    MAX_INPUT_AUDIO_DELAY_SAMPLES, MAX_INPUT_AUDIO_GAIN_MILLIDB, MAX_RETAINED_RECEIPTS,
    MIN_INPUT_AUDIO_BALANCE_BASIS_POINTS, MIN_INPUT_AUDIO_GAIN_MILLIDB, PreparedEngineExecution,
    TitleAirRequirement,
};
pub use error::{
    EngineError, InputCatalogueError, ShowError, SnapshotError, TitleRejection, TitleRejectionKind,
};
pub use history::{
    EditHistoryEntry, EditLockScope, EditRejection, EditRejectionKind, EngineCommandKind,
    MAX_EDIT_HISTORY,
};
pub use playlist::{
    EnginePlaylistOperation, EnginePlaylistStart, MAX_PLAYLIST_TRANSITION_FRAMES, PlaylistCue,
    PlaylistRestoreError, playlist_due_frame, playlist_from_model, playlist_to_model,
};
pub use state::{
    EngineInputAudioStripState, EngineInputSource, EngineTitle, MAX_SHOW_INPUTS, ShowState,
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
