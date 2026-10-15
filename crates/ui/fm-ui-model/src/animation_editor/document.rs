//! The editor document: a local draft [`Clip`], its selection, view, and
//! bounded snapshot undo history.

use core::{fmt, time::Duration};
use std::collections::VecDeque;

use fm_animation::{
    Clip, ClipError, KeyClipboard, KeyRef, KeySelection, MAX_CLIP_KEYS, MAX_CLIP_TRACKS,
    MAX_PHASE_FRAMES, MAX_TRACK_KEYS, Phase, Rgba, TrackKey,
};

use super::interaction::DragSession;

/// Most undo (and redo) steps kept; the oldest step is dropped beyond this.
pub const UNDO_LIMIT: usize = 100;
/// Default distance, in pixels, within which a drag snaps to a landmark.
pub const DEFAULT_SNAP_THRESHOLD_PX: f32 = 6.0;
/// Most zoomed-in timeline: one frame spans 64 pixels.
pub const MIN_FRAMES_PER_PIXEL: f32 = 1.0 / 64.0;
/// Most zoomed-out timeline: one pixel spans 64 frames.
pub const MAX_FRAMES_PER_PIXEL: f32 = 64.0;
/// Default zoom: four pixels per frame.
pub const DEFAULT_FRAMES_PER_PIXEL: f32 = 0.25;

/// Simple mode shows one preset bar per target; advanced mode shows every
/// property track with individual keys.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum EditorMode {
    #[default]
    Simple,
    Advanced,
}

/// What kind of element an animation target is, for outline icons.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TargetKind {
    Text,
    Image,
    Shape,
    Video,
    Group,
    Other,
}

/// A target's static placement in canvas pixels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TargetBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl TargetBounds {
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Caller-supplied outline metadata for one animation target: its label,
/// kind, and the static design values animation layers onto.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetOutline<T> {
    pub target: T,
    pub name: String,
    pub kind: TargetKind,
    pub bounds: TargetBounds,
    /// Static rotation in millidegrees.
    pub rotation_mdeg: i64,
    /// Static opacity, `0..=255`.
    pub opacity: u8,
    /// Static straight RGBA tint/fill.
    pub color: Rgba,
    /// `width` x `height` of the target's own frame, which crop keys are
    /// pixels of, when it differs from `bounds` (a quarter-turned scene
    /// layer, whose bounds are its footprint); `None` means `bounds`.
    pub frame_size: Option<(u32, u32)>,
}

impl<T> TargetOutline<T> {
    /// An unrotated, opaque, white target.
    #[must_use]
    pub fn new(target: T, name: impl Into<String>, kind: TargetKind, bounds: TargetBounds) -> Self {
        Self {
            target,
            name: name.into(),
            kind,
            bounds,
            rotation_mdeg: 0,
            opacity: u8::MAX,
            color: Rgba::new(255, 255, 255, 255),
            frame_size: None,
        }
    }

    /// The box a saved motion's crop keys are measured against: the
    /// target's own frame, each side at least one pixel.
    #[must_use]
    pub fn crop_box(&self) -> (u32, u32) {
        let (width, height) = self
            .frame_size
            .unwrap_or((self.bounds.width, self.bounds.height));
        (width.max(1), height.max(1))
    }
}

/// Timeline zoom and scroll. Frames map to pixels relative to the left edge
/// of the track area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimelineView {
    frames_per_pixel: f32,
    scroll_frame: f32,
    snap_threshold_px: f32,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self {
            frames_per_pixel: DEFAULT_FRAMES_PER_PIXEL,
            scroll_frame: 0.0,
            snap_threshold_px: DEFAULT_SNAP_THRESHOLD_PX,
        }
    }
}

impl TimelineView {
    #[must_use]
    pub const fn frames_per_pixel(&self) -> f32 {
        self.frames_per_pixel
    }

    /// The frame at the left edge of the track area.
    #[must_use]
    pub const fn scroll_frame(&self) -> f32 {
        self.scroll_frame
    }

    #[must_use]
    pub const fn snap_threshold_px(&self) -> f32 {
        self.snap_threshold_px
    }

    /// Sets the zoom, clamped to
    /// [`MIN_FRAMES_PER_PIXEL`]`..=`[`MAX_FRAMES_PER_PIXEL`].
    pub fn set_frames_per_pixel(&mut self, frames_per_pixel: f32) {
        if frames_per_pixel.is_finite() {
            self.frames_per_pixel =
                frames_per_pixel.clamp(MIN_FRAMES_PER_PIXEL, MAX_FRAMES_PER_PIXEL);
        }
    }

    pub fn set_snap_threshold_px(&mut self, pixels: f32) {
        if pixels.is_finite() {
            self.snap_threshold_px = pixels.max(0.0);
        }
    }

    /// Scrolls so `frame` is at the left edge (never before frame `0`).
    pub fn scroll_to(&mut self, frame: f32) {
        if frame.is_finite() {
            self.scroll_frame = frame.max(0.0);
        }
    }

    pub fn scroll_by_px(&mut self, pixels: f32) {
        self.scroll_to(self.scroll_frame + pixels * self.frames_per_pixel);
    }

    /// Multiplies the zoom by `factor` (`< 1` zooms in) keeping the frame
    /// under `anchor_x` in place.
    pub fn zoom_about(&mut self, factor: f32, anchor_x: f32) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let anchored = self.scroll_frame + anchor_x * self.frames_per_pixel;
        self.set_frames_per_pixel(self.frames_per_pixel * factor);
        self.scroll_to(anchored - anchor_x * self.frames_per_pixel);
    }

    /// The x offset of `frame`'s left edge.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // Frames stay below 2^24 (MAX_PHASE_FRAMES).
    pub fn x_of_frame(&self, frame: u32) -> f32 {
        (frame as f32 - self.scroll_frame) / self.frames_per_pixel
    }

    /// The nearest whole frame at x offset `x`, never negative.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    pub fn frame_at_x(&self, x: f32) -> u32 {
        let frame = (self.scroll_frame + x * self.frames_per_pixel).round();
        if frame.is_finite() && frame > 0.0 {
            frame.min(u32::MAX as f32) as u32
        } else {
            0
        }
    }

    /// Whole frames at least partly visible in a track area `width` pixels
    /// wide, inclusive.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn visible_range(&self, width: f32) -> (u32, u32) {
        let first = self.scroll_frame.floor().max(0.0) as u32;
        let last = (self.scroll_frame + width.max(0.0) * self.frames_per_pixel)
            .ceil()
            .max(0.0) as u32;
        (first, last.max(first))
    }

    /// The snap threshold in frames at the current zoom; at least one frame.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    pub fn snap_threshold_frames(&self) -> u32 {
        let frames = (self.snap_threshold_px * self.frames_per_pixel).ceil();
        (frames.clamp(1.0, MAX_PHASE_FRAMES as f32)) as u32
    }
}

/// A non-key limit an edit would exceed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AnimLimit {
    /// A phase longer than [`MAX_PHASE_FRAMES`].
    PhaseFrames { max: u32 },
    /// More than [`MAX_CLIP_TRACKS`] tracks.
    Tracks { max: usize },
    /// More than [`MAX_TRACK_KEYS`] keys on one track.
    TrackKeys { max: usize },
    /// More than [`MAX_CLIP_KEYS`] keys in the clip.
    ClipKeys { max: usize },
}

/// Why an editor action was refused. A refused action leaves the document
/// unchanged. [`Self::msg_key`] is a stable id for localized messages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnimEditorError<T> {
    /// Retiming would put two keys on `track` on one frame; `min_len` is the
    /// shortest span/phase length that would not collide.
    RetimeCollision {
        track: TrackKey<T>,
        min_len: u32,
    },
    /// A moved or pasted key landed on another key.
    KeyCollision {
        track: TrackKey<T>,
        frame: u32,
    },
    /// A clip limit would be exceeded.
    LimitExceeded(AnimLimit),
    /// A key would lie after the end of its phase.
    KeyOutsidePhase {
        track: TrackKey<T>,
        frame: u32,
    },
    /// A value is the wrong kind or out of range for the property.
    InvalidValue {
        track: TrackKey<T>,
        frame: u32,
    },
    /// A referenced key does not exist in the draft.
    UnknownKey {
        track: TrackKey<T>,
        frame: u32,
    },
    /// The target has no outline metadata.
    UnknownTarget(T),
    /// Retiming from a selection that spans zero frames.
    ZeroSpan,
    /// The action needs a non-empty key selection.
    EmptySelection,
    /// Paste with nothing copied.
    EmptyClipboard,
    /// A drag update/commit/cancel with no drag in progress.
    NoDrag,
    /// An edit, undo, or new drag while a drag is in progress.
    DragActive,
    NothingToUndo,
    NothingToRedo,
    /// A curve edit on the last key of a track, which starts no segment.
    NoSegment {
        track: TrackKey<T>,
        frame: u32,
    },
    /// The draft violated a structural invariant (never expected from editor
    /// actions).
    InvalidClip,
}

impl<T> AnimEditorError<T> {
    /// Stable message id, suitable as a localization key.
    #[must_use]
    pub const fn msg_key(&self) -> &'static str {
        match self {
            Self::RetimeCollision { .. } => "anim-editor-retime-collision",
            Self::KeyCollision { .. } => "anim-editor-key-collision",
            Self::LimitExceeded(_) => "anim-editor-limit-exceeded",
            Self::KeyOutsidePhase { .. } => "anim-editor-key-outside-phase",
            Self::InvalidValue { .. } => "anim-editor-invalid-value",
            Self::UnknownKey { .. } => "anim-editor-unknown-key",
            Self::UnknownTarget(_) => "anim-editor-unknown-target",
            Self::ZeroSpan => "anim-editor-zero-span",
            Self::EmptySelection => "anim-editor-empty-selection",
            Self::EmptyClipboard => "anim-editor-empty-clipboard",
            Self::NoDrag => "anim-editor-no-drag",
            Self::DragActive => "anim-editor-drag-active",
            Self::NothingToUndo => "anim-editor-nothing-to-undo",
            Self::NothingToRedo => "anim-editor-nothing-to-redo",
            Self::InvalidClip => "anim-editor-invalid-clip",
            Self::NoSegment { .. } => "anim-editor-no-segment",
        }
    }
}

impl<T> From<ClipError<T>> for AnimEditorError<T> {
    fn from(error: ClipError<T>) -> Self {
        match error {
            ClipError::PhaseTooLong { .. } => Self::LimitExceeded(AnimLimit::PhaseFrames {
                max: MAX_PHASE_FRAMES,
            }),
            ClipError::TooManyTracks { .. } => Self::LimitExceeded(AnimLimit::Tracks {
                max: MAX_CLIP_TRACKS,
            }),
            ClipError::TooManyTrackKeys { .. } => Self::LimitExceeded(AnimLimit::TrackKeys {
                max: MAX_TRACK_KEYS,
            }),
            ClipError::TooManyKeys { .. } => {
                Self::LimitExceeded(AnimLimit::ClipKeys { max: MAX_CLIP_KEYS })
            }
            ClipError::KeyOutsidePhase { track, frame } => Self::KeyOutsidePhase { track, frame },
            ClipError::InvalidValue { track, frame } => Self::InvalidValue { track, frame },
            ClipError::RetimeCollision { track, min_len } => {
                Self::RetimeCollision { track, min_len }
            }
            ClipError::KeyCollision { track, frame } => Self::KeyCollision { track, frame },
            ClipError::ZeroSpan => Self::ZeroSpan,
            ClipError::UnknownKey { track, frame } => Self::UnknownKey { track, frame },
            ClipError::DuplicateTrack(_)
            | ClipError::EmptyTrack(_)
            | ClipError::NonIncreasingKeys { .. } => Self::InvalidClip,
        }
    }
}

impl<T: fmt::Debug> fmt::Display for AnimEditorError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RetimeCollision { track, min_len } => write!(
                formatter,
                "retiming collides keys on {track:?}; shortest length is {min_len}"
            ),
            Self::LimitExceeded(limit) => write!(formatter, "limit exceeded: {limit:?}"),
            Self::KeyCollision { track, frame }
            | Self::KeyOutsidePhase { track, frame }
            | Self::InvalidValue { track, frame }
            | Self::UnknownKey { track, frame }
            | Self::NoSegment { track, frame } => {
                write!(formatter, "{}: {track:?} at frame {frame}", self.msg_key())
            }
            Self::UnknownTarget(target) => write!(formatter, "unknown target {target:?}"),
            _ => formatter.write_str(self.msg_key()),
        }
    }
}

impl<T: fmt::Debug> std::error::Error for AnimEditorError<T> {}

/// Playback state; the caller drives it with clock ticks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Transport {
    pub(crate) playing: bool,
    pub(crate) looping: bool,
    /// Playhead frame when playback (re)started.
    pub(crate) start_frame: u32,
    /// Clock time accumulated since playback (re)started.
    pub(crate) elapsed: Duration,
}

impl Transport {
    #[must_use]
    pub const fn is_playing(&self) -> bool {
        self.playing
    }

    #[must_use]
    pub const fn is_looping(&self) -> bool {
        self.looping
    }
}

/// One undo step: the draft and the selection that went with it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Snapshot<T> {
    pub(crate) clip: Clip<T>,
    pub(crate) selection: KeySelection<T>,
}

/// The keyframe animation editor's headless state for one clip.
///
/// `T` identifies a target (for example a title element id). All edits act
/// on a local draft; each completed action (including a whole drag) is one
/// undo step. [`Self::generation`] changes whenever the draft changes so
/// previews and a future protocol layer can detect staleness.
#[derive(Clone, Debug)]
pub struct AnimDocument<T> {
    pub(crate) draft: Clip<T>,
    pub(crate) base_revision: u64,
    pub(crate) dirty: bool,
    pub(crate) generation: u64,
    pub(crate) selection: KeySelection<T>,
    /// Shift-click range anchor.
    pub(crate) anchor: Option<KeyRef<T>>,
    pub(crate) playhead: u32,
    pub(crate) phase: Phase,
    pub(crate) view: TimelineView,
    pub(crate) mode: EditorMode,
    pub(crate) targets: Vec<TargetOutline<T>>,
    pub(crate) canvas: (u32, u32),
    pub(crate) clipboard: Option<KeyClipboard<T>>,
    pub(crate) undo: VecDeque<Snapshot<T>>,
    pub(crate) redo: Vec<Snapshot<T>>,
    pub(crate) drag: Option<DragSession<T>>,
    pub(crate) transport: Transport,
}

impl<T: Copy + Ord> AnimDocument<T> {
    /// A document editing `clip`, which is at authority revision
    /// `base_revision`, on a `canvas_width` x `canvas_height` canvas.
    #[must_use]
    pub fn new(clip: Clip<T>, base_revision: u64, canvas_width: u32, canvas_height: u32) -> Self {
        Self {
            draft: clip,
            base_revision,
            dirty: false,
            generation: 0,
            selection: KeySelection::new(),
            anchor: None,
            playhead: 0,
            phase: Phase::In,
            view: TimelineView::default(),
            mode: EditorMode::default(),
            targets: Vec::new(),
            canvas: (canvas_width, canvas_height),
            clipboard: None,
            undo: VecDeque::new(),
            redo: Vec::new(),
            drag: None,
            transport: Transport::default(),
        }
    }

    /// The draft clip.
    #[must_use]
    pub const fn clip(&self) -> &Clip<T> {
        &self.draft
    }

    /// The authority revision the draft is based on.
    #[must_use]
    pub const fn base_revision(&self) -> u64 {
        self.base_revision
    }

    /// Whether the draft has edits not yet saved to the authority.
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Increments on every draft change (edits, drag updates, undo, redo,
    /// rebase).
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn selection(&self) -> &KeySelection<T> {
        &self.selection
    }

    /// Playhead frame within [`Self::phase`].
    #[must_use]
    pub const fn playhead(&self) -> u32 {
        self.playhead
    }

    /// The phase shown on the timeline.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    #[must_use]
    pub const fn view(&self) -> &TimelineView {
        &self.view
    }

    pub const fn view_mut(&mut self) -> &mut TimelineView {
        &mut self.view
    }

    #[must_use]
    pub const fn mode(&self) -> EditorMode {
        self.mode
    }

    pub const fn set_mode(&mut self, mode: EditorMode) {
        self.mode = mode;
    }

    /// Canvas size in pixels.
    #[must_use]
    pub const fn canvas(&self) -> (u32, u32) {
        self.canvas
    }

    pub const fn set_canvas(&mut self, width: u32, height: u32) {
        self.canvas = (width, height);
    }

    #[must_use]
    pub const fn transport(&self) -> &Transport {
        &self.transport
    }

    #[must_use]
    pub const fn clipboard(&self) -> Option<&KeyClipboard<T>> {
        self.clipboard.as_ref()
    }

    /// The drag in progress, if any.
    #[must_use]
    pub const fn drag(&self) -> Option<&DragSession<T>> {
        self.drag.as_ref()
    }

    /// Target outline metadata in caller (outline) order.
    #[must_use]
    pub fn targets(&self) -> &[TargetOutline<T>] {
        &self.targets
    }

    #[must_use]
    pub fn target(&self, target: T) -> Option<&TargetOutline<T>> {
        self.targets.iter().find(|outline| outline.target == target)
    }

    /// Replaces the outline metadata. Later duplicates of a target are
    /// dropped.
    pub fn set_targets(&mut self, targets: impl IntoIterator<Item = TargetOutline<T>>) {
        self.targets.clear();
        for outline in targets {
            if self.target(outline.target).is_none() {
                self.targets.push(outline);
            }
        }
    }

    /// The length of the shown phase in frames.
    #[must_use]
    pub const fn phase_frames(&self) -> u32 {
        self.draft.phase_frames(self.phase)
    }

    /// Track rows of the shown phase: outline targets first (in outline
    /// order), then any other targets, each by property. Box selection rows
    /// index into this list.
    #[must_use]
    pub fn rows(&self) -> Vec<TrackKey<T>> {
        let mut rows: Vec<TrackKey<T>> = self
            .draft
            .tracks()
            .iter()
            .map(|track| track.key)
            .filter(|key| key.phase == self.phase)
            .collect();
        rows.sort_by_key(|key| {
            let order = self
                .targets
                .iter()
                .position(|outline| outline.target == key.target)
                .unwrap_or(usize::MAX);
            (order, key.target, key.property)
        });
        rows
    }

    /// Simple-mode summary: per target with keys in the shown phase, its
    /// first and last key frames, in row order.
    #[must_use]
    pub fn target_spans(&self) -> Vec<(T, u32, u32)> {
        let mut spans: Vec<(T, u32, u32)> = Vec::new();
        for row in self.rows() {
            let Some(track) = self.draft.track(row) else {
                continue;
            };
            let (Some(first), Some(last)) = (track.keys.first(), track.keys.last()) else {
                continue;
            };
            match spans.iter_mut().find(|span| span.0 == row.target) {
                Some(span) => {
                    span.1 = span.1.min(first.frame);
                    span.2 = span.2.max(last.frame);
                }
                None => spans.push((row.target, first.frame, last.frame)),
            }
        }
        spans
    }

    /// Moves the playhead, clamped to the shown phase. Stops playback.
    pub fn set_playhead(&mut self, frame: u32) {
        self.transport.playing = false;
        self.playhead = frame.min(self.phase_frames());
    }

    /// Shows `phase`: clears the selection and clamps the playhead.
    pub fn set_phase(&mut self, phase: Phase) {
        if self.phase != phase {
            self.phase = phase;
            self.selection = KeySelection::new();
            self.anchor = None;
            self.transport.playing = false;
            self.playhead = self.playhead.min(self.phase_frames());
        }
    }

    /// Replaces the selection with keys that exist in the draft.
    pub fn set_selection(&mut self, selection: &KeySelection<T>) {
        self.selection = selection
            .iter()
            .filter(|key| self.draft.key(key.track, key.frame).is_some())
            .collect();
        self.anchor = self.selection.iter().next();
    }

    pub fn clear_selection(&mut self) {
        self.selection = KeySelection::new();
        self.anchor = None;
    }

    /// Selects every key in the shown phase.
    pub fn select_all(&mut self) {
        self.selection = self
            .draft
            .tracks()
            .iter()
            .filter(|track| track.key.phase == self.phase)
            .flat_map(|track| {
                track.keys.iter().map(move |key| KeyRef {
                    track: track.key,
                    frame: key.frame,
                })
            })
            .collect();
        self.anchor = self.selection.iter().next();
    }

    /// Replaces the draft with the authority's `clip` at `revision`,
    /// discarding local edits, history, and any drag. The selection keeps
    /// keys that still exist.
    pub fn reset_from_authority(&mut self, clip: Clip<T>, revision: u64) {
        self.draft = clip;
        self.base_revision = revision;
        self.dirty = false;
        self.undo.clear();
        self.redo.clear();
        self.drag = None;
        self.bump();
        let selection = core::mem::take(&mut self.selection);
        self.set_selection(&selection);
        self.playhead = self.playhead.min(self.phase_frames());
    }

    /// Records that the current draft was saved as authority `revision`.
    /// History is kept.
    pub const fn mark_saved(&mut self, revision: u64) {
        self.base_revision = revision;
        self.dirty = false;
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    #[must_use]
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// Restores the draft and selection before the last edit.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::DragActive`] during a drag;
    /// [`AnimEditorError::NothingToUndo`] with no history.
    pub fn undo(&mut self) -> Result<(), AnimEditorError<T>> {
        self.ensure_idle()?;
        let previous = self.undo.pop_back().ok_or(AnimEditorError::NothingToUndo)?;
        let current = self.replace_state(previous);
        self.redo.push(current);
        truncate_front(&mut self.redo);
        Ok(())
    }

    /// Reapplies the last undone edit.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::DragActive`] during a drag;
    /// [`AnimEditorError::NothingToRedo`] with nothing undone.
    pub fn redo(&mut self) -> Result<(), AnimEditorError<T>> {
        self.ensure_idle()?;
        let next = self.redo.pop().ok_or(AnimEditorError::NothingToRedo)?;
        let current = self.replace_state(next);
        self.push_undo(current);
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> Snapshot<T> {
        Snapshot {
            clip: self.draft.clone(),
            selection: self.selection.clone(),
        }
    }

    /// Swaps in `state`, returning the replaced state.
    fn replace_state(&mut self, state: Snapshot<T>) -> Snapshot<T> {
        let current = Snapshot {
            clip: core::mem::replace(&mut self.draft, state.clip),
            selection: core::mem::replace(&mut self.selection, state.selection),
        };
        self.anchor = self.selection.iter().next();
        self.dirty = true;
        self.bump();
        self.playhead = self.playhead.min(self.phase_frames());
        current
    }

    pub(crate) fn push_undo(&mut self, before: Snapshot<T>) {
        self.undo.push_back(before);
        while self.undo.len() > UNDO_LIMIT {
            self.undo.pop_front();
        }
    }

    /// Records a completed edit whose prior state was `before`.
    pub(crate) fn record_edit(&mut self, before: Snapshot<T>) {
        self.push_undo(before);
        self.redo.clear();
        self.dirty = true;
    }

    pub(crate) const fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    pub(crate) fn ensure_idle(&self) -> Result<(), AnimEditorError<T>> {
        if self.drag.is_some() {
            Err(AnimEditorError::DragActive)
        } else {
            Ok(())
        }
    }

    /// Runs one atomic edit as one undo step. `edit` works on a copy of the
    /// draft and returns the selection to show afterwards; a refused edit
    /// changes nothing.
    pub(crate) fn edit(
        &mut self,
        edit: impl FnOnce(&mut Clip<T>, &Self) -> Result<KeySelection<T>, AnimEditorError<T>>,
    ) -> Result<(), AnimEditorError<T>> {
        self.ensure_idle()?;
        let mut next = self.draft.clone();
        let selection = edit(&mut next, self)?;
        if next == self.draft {
            self.selection = selection;
            return Ok(());
        }
        let before = self.snapshot();
        self.draft = next;
        self.selection = selection;
        self.anchor = self.selection.iter().next();
        self.playhead = self.playhead.min(self.phase_frames());
        self.bump();
        self.record_edit(before);
        Ok(())
    }
}

fn truncate_front<S>(stack: &mut Vec<S>) {
    if stack.len() > UNDO_LIMIT {
        let excess = stack.len() - UNDO_LIMIT;
        stack.drain(..excess);
    }
}
