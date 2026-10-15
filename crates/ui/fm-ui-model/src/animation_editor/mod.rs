//! Headless core of the keyframe animation editor (GX-011).
//!
//! [`AnimDocument`] edits a local draft [`fm_animation::Clip`] keyed by a
//! caller-chosen target id `T` (for example a title element id). It owns the
//! selection, playhead, shown phase, timeline view, simple/advanced mode,
//! caller-supplied target outline metadata, a clipboard, and snapshot
//! undo/redo bounded at [`UNDO_LIMIT`]. Nothing here renders or talks to a
//! server: egui widgets draw from and feed into this state, and a future
//! protocol layer can sync the draft using [`AnimDocument::base_revision`],
//! [`AnimDocument::is_dirty`], [`AnimDocument::generation`],
//! [`AnimDocument::reset_from_authority`], and [`AnimDocument::mark_saved`].
//!
//! * [`document`]: state, view mapping, undo, and [`AnimEditorError`].
//! * [`interaction`]: drag sessions, click/box selection, retime handles,
//!   and one-step edits (key at playhead, delete, copy/paste, easing,
//!   presets, phase duration, nudge) plus playback.
//! * [`curve`]: custom cubic-bezier easing: seeds, handle drag math,
//!   snapping, and one-step curve edits for the curve editor.
//! * [`preview`]: sampled values, the wireframe proxy and onion skins, and
//!   the latest-wins [`PreviewScheduler`] for exact previews.

pub mod curve;
pub mod document;
pub mod interaction;
pub mod preview;

pub use curve::{
    CURVE_SNAP_X, CURVE_SNAP_Y, CurveControl, CurveHandle, CurvePreset, CurveSnap, CurveTarget,
    DEFAULT_CURVE_SNAP, MAX_CURVE_Y, MIN_CURVE_Y, Segment, curve_seed, default_curve,
};
pub use document::{
    AnimDocument, AnimEditorError, AnimLimit, DEFAULT_FRAMES_PER_PIXEL, DEFAULT_SNAP_THRESHOLD_PX,
    EditorMode, MAX_FRAMES_PER_PIXEL, MIN_FRAMES_PER_PIXEL, TargetBounds, TargetKind,
    TargetOutline, TimelineView, Transport, UNDO_LIMIT,
};
pub use interaction::{
    BoxMode, DragKind, DragOutcome, DragSession, DragStart, Modifiers, Pointer, RetimeHandle,
    phase_state,
};
pub use preview::{
    AnimSample, DEFAULT_PREVIEW_DEBOUNCE, ExactPreviewRequest, MAX_ONION_SKINS, OnionSkin,
    PreviewCompletion, PreviewKey, PreviewRequestId, PreviewScheduler, ProxyQuad, proxy_quad,
};

#[cfg(test)]
mod tests;
