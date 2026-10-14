//! Deterministic composition root for the simulated `FreeMix` engine.

mod engine;
mod error;
mod history;
mod state;

pub use engine::{
    Engine, EngineAcceptance, EngineCommand, EngineCommandOutcome, EngineEvent,
    EngineFadeToBlackState, EngineManualTransitionKind, EngineManualTransitionPosition,
    EngineManualTransitionState, EnginePrepareOutcome, EngineRestoreState, EngineSnapshot,
    EngineStreamStatus, FrameResult, MAX_INPUT_AUDIO_BALANCE_BASIS_POINTS,
    MAX_INPUT_AUDIO_DELAY_SAMPLES, MAX_INPUT_AUDIO_GAIN_MILLIDB, MAX_RETAINED_RECEIPTS,
    MIN_INPUT_AUDIO_BALANCE_BASIS_POINTS, MIN_INPUT_AUDIO_GAIN_MILLIDB, PreparedEngineExecution,
};
pub use error::{EngineError, InputCatalogueError, ShowError, SnapshotError};
pub use history::{
    EditHistoryEntry, EditLockScope, EditRejection, EditRejectionKind, EngineCommandKind,
    MAX_EDIT_HISTORY,
};
pub use state::{EngineInputAudioStripState, EngineInputSource, MAX_SHOW_INPUTS, ShowState};
