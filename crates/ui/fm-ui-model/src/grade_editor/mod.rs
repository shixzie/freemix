//! Headless core of the colour grading editor (SW-019).
//!
//! [`GradeDocument`] edits a local draft [`fm_grade::Grade`]. It owns
//! bounded snapshot undo/redo ([`GRADE_UNDO_LIMIT`]), gestures that make a
//! whole drag one undo step, presets, per-section resets, the bypass toggle,
//! two A/B compare slots, and a grade clipboard. Nothing here renders or
//! talks to a server: egui widgets draw from and feed into this state, and a
//! future protocol layer can sync the draft using
//! [`GradeDocument::base_revision`], [`GradeDocument::is_dirty`],
//! [`GradeDocument::generation`], [`GradeDocument::reset_from_authority`],
//! and [`GradeDocument::mark_saved`].
//!
//! * [`document`]: state, scalar controls ([`GradeParam`]), sections, undo,
//!   and [`GradeEditorError`].
//! * [`wheel`]: puck position ↔ wheel values and relative drags with a
//!   fine-adjust modifier.
//! * [`curves`]: adding, moving, and deleting curve points within the
//!   grade's bounds, and sampling curves for drawing.
//! * [`preview`]: local graded previews through
//!   [`fm_grade::CompiledGrade`] with caching, before/after compositing, and
//!   the debounced latest-wins [`RecomputeScheduler`].
//! * [`binding`]: binds a document to one target's replicated grade and
//!   turns edits into debounced grade commands.
//! * [`scopes`]: waveform, parade, vectorscope, and histogram grids.

pub mod binding;
pub mod curves;
pub mod document;
pub mod preview;
pub mod scopes;
pub mod wheel;

pub use binding::{
    DRAG_SEND_INTERVAL, GradeBinding, GradeCommand, GradeNameProblem, GradeSyncStatus, GradeTarget,
    SyncOutcome, library_name_problem, lut_fits_protocol, lut_id, lut_name_problem, lut_uri,
    next_library_id, next_lut_id, target_grade, target_source,
};
pub use curves::{CurveTarget, HUE_ANCHORS, MIN_HUE_GAP, MIN_LUMA_GAP, MIN_RGB_GAP};
pub use document::{
    AbSlot, GRADE_UNDO_LIMIT, GradeDocument, GradeEditorError, GradeParam, GradeSection,
    WHEEL_KINDS, wheel_mut,
};
pub use preview::{
    BARS_75, CompareMode, DEFAULT_PREVIEW_CACHE, DEFAULT_RECOMPUTE_DEBOUNCE,
    DEFAULT_RECOMPUTE_MAX_WAIT, GradePreview, RecomputeScheduler, RgbaImage, bars,
    compose_side_by_side, compose_wipe, neutral_white_balance, sample_still,
};
pub use scopes::{
    Histogram, ScopeConfig, ScopeGrid, ScopeKind, Scopes, bucket, chroma, compute_scopes, luma,
    vectorscope_cell,
};
pub use wheel::{
    FINE_FACTOR, ValueDrag, WheelDrag, direction_from_offset, puck_offset, wheel_color,
};

#[cfg(test)]
mod tests;
