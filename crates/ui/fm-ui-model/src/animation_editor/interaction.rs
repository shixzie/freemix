//! Pointer interaction, selection, and one-step editing actions.
//!
//! A drag is a session: [`AnimDocument::begin_drag`] records the origin
//! state, every [`AnimDocument::update_drag`] recomputes the result from that
//! origin (so the draft shows the live outcome), [`AnimDocument::commit_drag`]
//! records the whole drag as one undo step, and
//! [`AnimDocument::cancel_drag`] restores the origin. An update the clip
//! refuses (a collision, say) keeps the last valid state and reports the
//! typed error, which [`DragSession::rejected`] keeps for display.
//!
//! Modifiers: `shift` disables snapping during drags (and extends a click
//! selection along a track), `alt` extends a retime handle without scaling,
//! and `ctrl` toggles click/box selection.

use core::num::NonZeroU32;
use core::time::Duration;
use std::sync::Arc;

use fm_animation::{
    AnimationPreset, Clip, ClipPosition, ClipState, Easing, KeyRef, KeySelection, Keyframe,
    MAX_PHASE_FRAMES, MotionError, Phase, PhaseMotion, PresetGeometry, Property, Snap, SnapTargets,
    TrackKey, Value, stretch_frame,
};

use super::curve::{CurveHandle, CurveSnap};
use super::document::{AnimDocument, AnimEditorError, Snapshot};

const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// A preset the Simple-mode picker offers: a built-in motion or one the
/// operator saved to the project's preset library (Protocol 2.34).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PresetSource {
    BuiltIn(AnimationPreset),
    User {
        /// The library preset id.
        preset: NonZeroU32,
        name: String,
        motion: Arc<PhaseMotion>,
    },
}

/// Keyboard modifiers held during a pointer action.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Modifiers {
    /// Disables snapping; shift-click extends along a track.
    pub shift: bool,
    /// Retime handles extend without scaling.
    pub alt: bool,
    /// Toggles keys in and out of the selection.
    pub ctrl: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        shift: false,
        alt: false,
        ctrl: false,
    };
    pub const SHIFT: Self = Self {
        shift: true,
        alt: false,
        ctrl: false,
    };
    pub const ALT: Self = Self {
        shift: false,
        alt: true,
        ctrl: false,
    };
    pub const CTRL: Self = Self {
        shift: false,
        alt: false,
        ctrl: true,
    };
}

/// A pointer position on the timeline: a frame of the shown phase and a row
/// index into [`AnimDocument::rows`]. Only box selection reads the row.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Pointer {
    pub frame: u32,
    pub row: usize,
}

impl Pointer {
    #[must_use]
    pub const fn new(frame: u32, row: usize) -> Self {
        Self { frame, row }
    }

    /// A pointer at `frame` on row `0`.
    #[must_use]
    pub const fn at(frame: u32) -> Self {
        Self { frame, row: 0 }
    }
}

/// A retime handle.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RetimeHandle {
    /// Left edge of the selection's bounding box; scales about the right
    /// edge.
    SelectionStart,
    /// Right edge of the selection's bounding box; scales about the left
    /// edge.
    SelectionEnd,
    /// End of the shown phase; stretches every key of the phase.
    PhaseEnd,
}

/// What the pointer pressed on to start a drag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DragStart<T> {
    /// A key; drags the selection (selecting the key first if needed).
    Key(KeyRef<T>),
    /// Empty track space; drags a selection box.
    SelectionBox,
    /// The playhead or the time ruler.
    Playhead,
    Retime(RetimeHandle),
}

/// How a box combines with the selection it started from.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BoxMode {
    Replace,
    Add,
    Toggle,
}

/// The kind and live state of a drag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DragKind<T> {
    Keys {
        hit: KeyRef<T>,
    },
    Box {
        anchor: Pointer,
        current: Pointer,
        mode: BoxMode,
        rows: Vec<TrackKey<T>>,
    },
    Playhead,
    Retime {
        handle: RetimeHandle,
        /// Selection (or phase) bounds at the start of the drag.
        first: u32,
        last: u32,
    },
    /// A bezier handle of a custom easing curve
    /// ([`AnimDocument::begin_curve_drag`]); timeline pointer updates leave
    /// it alone.
    Curve {
        /// The keys whose easing the drag sets.
        keys: KeySelection<T>,
        handle: CurveHandle,
        /// The curve before the drag.
        start: fm_animation::CubicBezier,
        /// The curve shown now.
        current: fm_animation::CubicBezier,
        /// The axes the last update snapped on.
        snap: CurveSnap,
    },
}

/// A drag in progress.
#[derive(Clone, Debug)]
pub struct DragSession<T> {
    kind: DragKind<T>,
    origin: Snapshot<T>,
    origin_playhead: u32,
    grab_frame: u32,
    snaps: SnapTargets,
    snap: Option<Snap>,
    rejected: Option<AnimEditorError<T>>,
}

impl<T> DragSession<T> {
    pub(crate) const fn new(kind: DragKind<T>, origin: Snapshot<T>, origin_playhead: u32) -> Self {
        Self {
            kind,
            origin,
            origin_playhead,
            grab_frame: 0,
            snaps: SnapTargets::new(),
            snap: None,
            rejected: None,
        }
    }

    pub(crate) const fn origin(&self) -> &Snapshot<T> {
        &self.origin
    }

    pub(crate) fn set_rejected(&mut self, rejected: Option<AnimEditorError<T>>) {
        self.rejected = rejected;
    }

    pub(crate) const fn set_curve(&mut self, curve: fm_animation::CubicBezier, snapped: CurveSnap) {
        if let DragKind::Curve { current, snap, .. } = &mut self.kind {
            *current = curve;
            *snap = snapped;
        }
    }

    #[must_use]
    pub const fn kind(&self) -> &DragKind<T> {
        &self.kind
    }

    /// Whether this drags a curve handle rather than something on the
    /// timeline.
    #[must_use]
    pub const fn is_curve(&self) -> bool {
        matches!(self.kind, DragKind::Curve { .. })
    }

    /// The landmark the last update snapped to, for a guide line.
    #[must_use]
    pub const fn snap(&self) -> Option<Snap> {
        self.snap
    }

    /// Why the last update was refused; `None` after a valid update.
    #[must_use]
    pub const fn rejected(&self) -> Option<&AnimEditorError<T>> {
        self.rejected.as_ref()
    }

    /// For a box drag, the inclusive frame and row ranges it covers.
    #[must_use]
    pub fn box_range(&self) -> Option<((u32, u32), (usize, usize))> {
        match &self.kind {
            DragKind::Box {
                anchor, current, ..
            } => Some((
                (
                    anchor.frame.min(current.frame),
                    anchor.frame.max(current.frame),
                ),
                (anchor.row.min(current.row), anchor.row.max(current.row)),
            )),
            _ => None,
        }
    }
}

/// The result of committing a drag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DragOutcome<T> {
    /// Whether the drag changed the draft and was recorded as one undo step.
    pub edited: bool,
    /// The refusal of the last update, if the drag ended on an invalid
    /// position (the last valid state was committed).
    pub rejected: Option<AnimEditorError<T>>,
}

/// The clip state that shows `frame` of `phase`.
#[must_use]
pub fn phase_state(phase: Phase, frame: u32) -> ClipState {
    match phase {
        Phase::In => ClipState::In(ClipPosition::frame(frame)),
        Phase::Out => ClipState::Out(ClipPosition::frame(frame)),
    }
}

impl<T: Copy + Ord> AnimDocument<T> {
    // ----- drags -----------------------------------------------------------

    /// Starts a drag at `pointer`.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::DragActive`] while another drag runs;
    /// [`AnimEditorError::UnknownKey`] for a missing hit key;
    /// [`AnimEditorError::EmptySelection`] for a selection handle with
    /// nothing selected.
    pub fn begin_drag(
        &mut self,
        start: DragStart<T>,
        pointer: Pointer,
        modifiers: Modifiers,
    ) -> Result<(), AnimEditorError<T>> {
        self.ensure_idle()?;
        self.transport.playing = false;
        let origin_playhead = self.playhead;
        let phase_frames = self.phase_frames();
        let timebase = self.draft.timebase();
        let (kind, snaps) = match start {
            DragStart::Key(hit) => {
                if self.draft.key(hit.track, hit.frame).is_none() {
                    return Err(AnimEditorError::UnknownKey {
                        track: hit.track,
                        frame: hit.frame,
                    });
                }
                if !self.selection.contains(hit.track, hit.frame) {
                    if !(modifiers.ctrl || modifiers.shift) {
                        self.selection = KeySelection::new();
                    }
                    self.selection.insert(hit.track, hit.frame);
                }
                self.anchor = Some(hit);
                let snaps = self
                    .landmarks(true)
                    .with_whole_seconds(timebase, phase_frames);
                (DragKind::Keys { hit }, snaps)
            }
            DragStart::SelectionBox => {
                let mode = if modifiers.ctrl {
                    BoxMode::Toggle
                } else if modifiers.shift {
                    BoxMode::Add
                } else {
                    BoxMode::Replace
                };
                let kind = DragKind::Box {
                    anchor: pointer,
                    current: pointer,
                    mode,
                    rows: self.rows(),
                };
                (kind, SnapTargets::new())
            }
            DragStart::Playhead => {
                let snaps = self
                    .phase_key_frames(None)
                    .fold(SnapTargets::new(), |snaps, frame| snaps.with_keys([frame]))
                    .with_phase_bounds(phase_frames);
                (DragKind::Playhead, snaps)
            }
            DragStart::Retime(handle) => {
                let (first, last) = match handle {
                    RetimeHandle::PhaseEnd => (0, phase_frames),
                    RetimeHandle::SelectionStart | RetimeHandle::SelectionEnd => {
                        match (self.selection.first_frame(), self.selection.last_frame()) {
                            (Some(first), Some(last)) => (first, last),
                            _ => return Err(AnimEditorError::EmptySelection),
                        }
                    }
                };
                let snaps = if handle == RetimeHandle::PhaseEnd {
                    let reach = phase_frames
                        .saturating_mul(2)
                        .max(timebase.frame_at_second(10).try_into().unwrap_or(u32::MAX))
                        .min(MAX_PHASE_FRAMES);
                    SnapTargets::new()
                        .with_playhead(self.playhead)
                        .with_whole_seconds(timebase, reach)
                } else {
                    self.landmarks(true)
                        .with_whole_seconds(timebase, phase_frames)
                };
                (
                    DragKind::Retime {
                        handle,
                        first,
                        last,
                    },
                    snaps,
                )
            }
        };
        self.drag = Some(DragSession {
            kind,
            origin: self.snapshot(),
            origin_playhead,
            grab_frame: pointer.frame,
            snaps,
            snap: None,
            rejected: None,
        });
        if matches!(start, DragStart::Playhead | DragStart::SelectionBox) {
            // Pressing the ruler moves the playhead; pressing empty space
            // applies an empty box at once.
            let _ = self.update_drag(pointer, modifiers);
        }
        Ok(())
    }

    /// Moves the drag to `pointer`, recomputing from the drag origin.
    /// Returns the landmark snapped to, if any.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::NoDrag`] without a drag; otherwise the clip's
    /// refusal (for example [`AnimEditorError::RetimeCollision`] with its
    /// `min_len`), in which case the last valid state is kept.
    pub fn update_drag(
        &mut self,
        pointer: Pointer,
        modifiers: Modifiers,
    ) -> Result<Option<Snap>, AnimEditorError<T>> {
        let mut drag = self.drag.take().ok_or(AnimEditorError::NoDrag)?;
        let result = self.apply_drag(&mut drag, pointer, modifiers);
        match &result {
            Ok(snap) => {
                drag.snap = *snap;
                drag.rejected = None;
            }
            Err(error) => drag.rejected = Some(error.clone()),
        }
        self.drag = Some(drag);
        result
    }

    /// Ends the drag, recording any draft change as exactly one undo step.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::NoDrag`] without a drag.
    pub fn commit_drag(&mut self) -> Result<DragOutcome<T>, AnimEditorError<T>> {
        let drag = self.drag.take().ok_or(AnimEditorError::NoDrag)?;
        let edited = self.draft != drag.origin.clip;
        if edited {
            self.record_edit(drag.origin);
        }
        Ok(DragOutcome {
            edited,
            rejected: drag.rejected,
        })
    }

    /// Ends the drag, restoring the draft, selection, and playhead from
    /// before it.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::NoDrag`] without a drag.
    pub fn cancel_drag(&mut self) -> Result<(), AnimEditorError<T>> {
        let drag = self.drag.take().ok_or(AnimEditorError::NoDrag)?;
        if self.draft != drag.origin.clip {
            self.draft = drag.origin.clip;
            self.bump();
        }
        self.selection = drag.origin.selection;
        self.playhead = drag.origin_playhead;
        Ok(())
    }

    fn apply_drag(
        &mut self,
        drag: &mut DragSession<T>,
        pointer: Pointer,
        modifiers: Modifiers,
    ) -> Result<Option<Snap>, AnimEditorError<T>> {
        let threshold = self.view.snap_threshold_frames();
        let snap_to = |frame: u32| -> (u32, Option<Snap>) {
            if modifiers.shift {
                (frame, None)
            } else {
                let snap = drag.snaps.snap(frame, threshold);
                (snap.map_or(frame, |snap| snap.frame), snap)
            }
        };
        let origin = &drag.origin;
        match &mut drag.kind {
            DragKind::Keys { hit } => {
                let moved = i64::from(pointer.frame) - i64::from(drag.grab_frame);
                let candidate = clamp_frame(i64::from(hit.frame) + moved);
                let (frame, snap) = snap_to(candidate);
                let delta = i64::from(frame) - i64::from(hit.frame);
                let mut clip = origin.clip.clone();
                let selection = clip.move_keys(&origin.selection, delta)?;
                self.show_drag_result(clip, selection);
                Ok(snap)
            }
            DragKind::Box {
                anchor,
                current,
                mode,
                rows,
            } => {
                *current = pointer;
                let (low_frame, high_frame) = (
                    anchor.frame.min(current.frame),
                    anchor.frame.max(current.frame),
                );
                let (low_row, high_row) =
                    (anchor.row.min(current.row), anchor.row.max(current.row));
                let mut selection = match mode {
                    BoxMode::Replace => KeySelection::new(),
                    BoxMode::Add | BoxMode::Toggle => origin.selection.clone(),
                };
                for track_key in rows.iter().skip(low_row).take(high_row - low_row + 1) {
                    let Some(track) = self.draft.track(*track_key) else {
                        continue;
                    };
                    for key in &track.keys {
                        if (low_frame..=high_frame).contains(&key.frame)
                            && (*mode != BoxMode::Toggle
                                || !selection.remove(*track_key, key.frame))
                        {
                            selection.insert(*track_key, key.frame);
                        }
                    }
                }
                self.selection = selection;
                Ok(None)
            }
            DragKind::Playhead => {
                let (frame, snap) = snap_to(pointer.frame);
                self.playhead = frame.min(self.phase_frames());
                Ok(snap)
            }
            DragKind::Retime {
                handle,
                first,
                last,
            } => {
                let (first, last) = (*first, *last);
                let (frame, snap) = snap_to(pointer.frame);
                let mut clip = origin.clip.clone();
                let selection = retime(
                    &mut clip,
                    &origin.selection,
                    self.phase,
                    *handle,
                    (first, last),
                    frame,
                    modifiers.alt,
                )?;
                self.show_drag_result(clip, selection);
                Ok(snap)
            }
            // Moved by `update_curve_drag` in curve coordinates.
            DragKind::Curve { .. } => Ok(None),
        }
    }

    fn show_drag_result(&mut self, clip: Clip<T>, selection: KeySelection<T>) {
        if clip != self.draft {
            self.draft = clip;
            self.bump();
        }
        self.selection = selection;
        self.playhead = self.playhead.min(self.phase_frames());
    }

    /// Playhead, phase bounds, and key frames of the shown phase, excluding
    /// selected keys when `exclude_selected`.
    fn landmarks(&self, exclude_selected: bool) -> SnapTargets {
        let selection = exclude_selected.then_some(&self.selection);
        SnapTargets::new()
            .with_playhead(self.playhead)
            .with_keys(self.phase_key_frames(selection))
            .with_phase_bounds(self.phase_frames())
    }

    fn phase_key_frames<'a>(
        &'a self,
        exclude: Option<&'a KeySelection<T>>,
    ) -> impl Iterator<Item = u32> + 'a {
        self.draft
            .tracks()
            .iter()
            .filter(move |track| track.key.phase == self.phase)
            .flat_map(move |track| {
                track
                    .keys
                    .iter()
                    .filter(move |key| {
                        exclude.is_none_or(|selection| !selection.contains(track.key, key.frame))
                    })
                    .map(|key| key.frame)
            })
    }

    // ----- clicks ----------------------------------------------------------

    /// Click on a key: plain selects only it, `ctrl` toggles it, `shift`
    /// adds every key on its track between the last clicked key and it.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::UnknownKey`] for a missing key;
    /// [`AnimEditorError::DragActive`] during a drag.
    pub fn click_key(
        &mut self,
        hit: KeyRef<T>,
        modifiers: Modifiers,
    ) -> Result<(), AnimEditorError<T>> {
        self.ensure_idle()?;
        let Some(track) = self.draft.track(hit.track) else {
            return Err(AnimEditorError::UnknownKey {
                track: hit.track,
                frame: hit.frame,
            });
        };
        if track.key_index(hit.frame).is_none() {
            return Err(AnimEditorError::UnknownKey {
                track: hit.track,
                frame: hit.frame,
            });
        }
        if modifiers.ctrl {
            if !self.selection.remove(hit.track, hit.frame) {
                self.selection.insert(hit.track, hit.frame);
            }
            self.anchor = Some(hit);
        } else if modifiers.shift {
            if let Some(anchor) = self.anchor.filter(|anchor| anchor.track == hit.track) {
                let (low, high) = (anchor.frame.min(hit.frame), anchor.frame.max(hit.frame));
                for key in &track.keys {
                    if (low..=high).contains(&key.frame) {
                        self.selection.insert(hit.track, key.frame);
                    }
                }
            } else {
                self.selection.insert(hit.track, hit.frame);
                self.anchor = Some(hit);
            }
        } else {
            self.selection = KeySelection::new();
            self.selection.insert(hit.track, hit.frame);
            self.anchor = Some(hit);
        }
        Ok(())
    }

    /// Click on empty track space: clears the selection unless `ctrl` or
    /// `shift` is held.
    pub fn click_empty(&mut self, modifiers: Modifiers) {
        if self.drag.is_none() && !(modifiers.ctrl || modifiers.shift) {
            self.clear_selection();
        }
    }

    // ----- one-step edits --------------------------------------------------

    /// The value `target`'s `property` shows at the playhead: the sampled
    /// animation, else the property's identity (the static colour for
    /// [`Property::Color`]).
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::UnknownTarget`] without outline metadata.
    pub fn value_at_playhead(
        &self,
        target: T,
        property: Property,
    ) -> Result<Value, AnimEditorError<T>> {
        let outline = self
            .target(target)
            .ok_or(AnimEditorError::UnknownTarget(target))?;
        Ok(self
            .draft
            .sample(target, property, phase_state(self.phase, self.playhead))
            .or_else(|| property.identity())
            .unwrap_or(Value::Color(outline.color)))
    }

    /// Keys `target`'s `property` at the playhead with its current sampled
    /// value (an existing key there is kept as is). One undo step; selects
    /// the key.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::UnknownTarget`], a limit, or
    /// [`AnimEditorError::DragActive`].
    pub fn key_at_playhead(
        &mut self,
        target: T,
        property: Property,
    ) -> Result<KeyRef<T>, AnimEditorError<T>> {
        let track = TrackKey::new(target, self.phase, property);
        if self.draft.key(track, self.playhead).is_some() {
            self.ensure_idle()?;
            let key = KeyRef {
                track,
                frame: self.playhead,
            };
            self.selection = core::iter::once(key).collect();
            self.anchor = Some(key);
            return Ok(key);
        }
        let value = self.value_at_playhead(target, property)?;
        self.set_value_at_playhead(target, property, value)
    }

    /// Inserts or updates `target`'s `property` key at the playhead with
    /// `value`. A new key takes the easing of the key before it. One undo
    /// step; selects the key.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::UnknownTarget`],
    /// [`AnimEditorError::InvalidValue`], a limit, or
    /// [`AnimEditorError::DragActive`].
    pub fn set_value_at_playhead(
        &mut self,
        target: T,
        property: Property,
        value: Value,
    ) -> Result<KeyRef<T>, AnimEditorError<T>> {
        if self.target(target).is_none() {
            return Err(AnimEditorError::UnknownTarget(target));
        }
        let track = TrackKey::new(target, self.phase, property);
        let frame = self.playhead;
        self.edit(|clip, _| {
            let easing = clip.track(track).map_or(Easing::Linear, |existing| {
                existing
                    .keys
                    .iter()
                    .rev()
                    .find(|key| key.frame <= frame)
                    .map_or(Easing::Linear, |key| key.easing)
            });
            clip.set_key(track, Keyframe::new(frame, value, easing))?;
            Ok(core::iter::once(KeyRef { track, frame }).collect())
        })?;
        Ok(KeyRef { track, frame })
    }

    /// Deletes the selected keys. One undo step.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::EmptySelection`] or
    /// [`AnimEditorError::DragActive`].
    pub fn delete_selection(&mut self) -> Result<(), AnimEditorError<T>> {
        self.require_selection()?;
        self.edit(|clip, document| {
            clip.remove_keys(&document.selection)?;
            Ok(KeySelection::new())
        })
    }

    /// Copies the selected keys; returns how many were copied.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::EmptySelection`].
    pub fn copy_selection(&mut self) -> Result<usize, AnimEditorError<T>> {
        self.require_selection()?;
        let clipboard = self.draft.copy_keys(&self.selection)?;
        let count = clipboard.keys.len();
        self.clipboard = Some(clipboard);
        Ok(count)
    }

    /// Pastes the clipboard into the shown phase with its earliest key at
    /// the playhead, replacing keys on the same frames. One undo step;
    /// selects the pasted keys.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::EmptyClipboard`],
    /// [`AnimEditorError::KeyOutsidePhase`] when it would run past the phase
    /// end, a limit, or [`AnimEditorError::DragActive`].
    pub fn paste_at_playhead(&mut self) -> Result<(), AnimEditorError<T>> {
        let clipboard = self
            .clipboard
            .clone()
            .filter(|clipboard| !clipboard.keys.is_empty())
            .ok_or(AnimEditorError::EmptyClipboard)?;
        let (phase, at) = (self.phase, self.playhead);
        self.edit(|clip, _| Ok(clip.paste(&clipboard, phase, at)?))
    }

    /// Sets the easing of every selected key. One undo step.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::EmptySelection`] or
    /// [`AnimEditorError::DragActive`].
    pub fn set_easing(&mut self, easing: Easing) -> Result<(), AnimEditorError<T>> {
        self.require_selection()?;
        self.edit(|clip, document| {
            clip.set_easing(&document.selection, easing)?;
            Ok(document.selection.clone())
        })
    }

    /// Replaces `target`'s tracks for `preset`'s properties in `phase` with
    /// the preset over `duration` frames, first lengthening the phase to
    /// `duration` if it is shorter. One undo step; shows `phase` and selects
    /// the generated keys.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::UnknownTarget`], a limit, or
    /// [`AnimEditorError::DragActive`].
    pub fn apply_preset(
        &mut self,
        target: T,
        phase: Phase,
        preset: AnimationPreset,
        duration: u32,
    ) -> Result<(), AnimEditorError<T>> {
        let outline = self
            .target(target)
            .ok_or(AnimEditorError::UnknownTarget(target))?;
        let geometry = PresetGeometry {
            x: outline.bounds.x,
            y: outline.bounds.y,
            width: outline.bounds.width,
            height: outline.bounds.height,
            canvas_width: self.canvas.0,
            canvas_height: self.canvas.1,
        };
        self.ensure_idle()?;
        let shown = self.phase;
        self.phase = phase;
        let result = self.edit(|clip, _| {
            if clip.phase_frames(phase) < duration {
                clip.set_phase_frames_keeping_keys(phase, duration)?;
            }
            clip.apply_preset(target, phase, preset, duration, geometry)?;
            Ok(preset
                .keys(phase, duration, geometry)
                .into_iter()
                .flat_map(|(property, keys)| {
                    keys.into_iter().map(move |key| KeyRef {
                        track: TrackKey::new(target, phase, property),
                        frame: key.frame,
                    })
                })
                .collect())
        });
        if result.is_err() {
            self.phase = shown;
        } else if shown != phase {
            self.transport.playing = false;
        }
        result
    }

    /// Puts a saved user preset's `motion` on `target` over `duration`
    /// frames of `phase` (retimed proportionally, crop keys scaled from the
    /// preset's box to the target's outline), first lengthening the phase
    /// to `duration` if it is shorter. Only `properties` the target
    /// animates are put; a motion animating none of them changes nothing.
    /// One undo step; shows `phase` and selects the generated keys.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::UnknownTarget`], a retime collision, a limit, or
    /// [`AnimEditorError::DragActive`].
    pub fn apply_motion(
        &mut self,
        target: T,
        phase: Phase,
        motion: &PhaseMotion,
        duration: u32,
        properties: &[Property],
    ) -> Result<(), AnimEditorError<T>> {
        let outline = self
            .target(target)
            .ok_or(AnimEditorError::UnknownTarget(target))?;
        let (width, height) = outline.crop_box();
        self.ensure_idle()?;
        let shown = self.phase;
        self.phase = phase;
        let result = self.edit(|clip, _| {
            clip.apply_motion(target, phase, motion, duration, width, height, |property| {
                properties.contains(&property)
            })?;
            Ok(motion
                .properties()
                .filter(|property| properties.contains(property))
                .flat_map(|property| {
                    let track = TrackKey::new(target, phase, property);
                    clip.track(track)
                        .map(|track| track.keys.clone())
                        .unwrap_or_default()
                        .into_iter()
                        .map(move |key| KeyRef {
                            track,
                            frame: key.frame,
                        })
                })
                .collect())
        });
        if result.is_err() {
            self.phase = shown;
        } else if shown != phase {
            self.transport.playing = false;
        }
        result
    }

    /// Applies a built-in or user preset ([`Self::apply_preset`],
    /// [`Self::apply_motion`]).
    ///
    /// # Errors
    ///
    /// As the preset kind's method.
    pub fn apply_preset_source(
        &mut self,
        target: T,
        phase: Phase,
        preset: &PresetSource,
        duration: u32,
        properties: &[Property],
    ) -> Result<(), AnimEditorError<T>> {
        match preset {
            PresetSource::BuiltIn(preset) => self.apply_preset(target, phase, *preset, duration),
            PresetSource::User { motion, .. } => {
                self.apply_motion(target, phase, motion, duration, properties)
            }
        }
    }

    /// `target`'s tracks in `phase` of the draft as a motion measured
    /// against its outline, for saving as a user preset.
    ///
    /// # Errors
    ///
    /// [`MotionError::Empty`] when the target has no outline or nothing to
    /// save in `phase`, or a motion bound.
    pub fn capture_motion(&self, target: T, phase: Phase) -> Result<PhaseMotion, MotionError> {
        let (width, height) = self.target(target).ok_or(MotionError::Empty)?.crop_box();
        PhaseMotion::capture(&self.draft, target, phase, width, height)
    }

    /// Sets `phase`'s length to `frames`, stretching its keys
    /// proportionally. One undo step.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::RetimeCollision`] with the shortest length that
    /// fits, [`AnimEditorError::LimitExceeded`], or
    /// [`AnimEditorError::DragActive`].
    pub fn set_phase_duration(
        &mut self,
        phase: Phase,
        frames: u32,
    ) -> Result<(), AnimEditorError<T>> {
        let old = self.draft.phase_frames(phase);
        self.edit(|clip, document| {
            clip.stretch_phase(phase, frames)?;
            Ok(stretched(&document.selection, phase, old, frames))
        })
    }

    /// Sets `phase`'s length to `frames` without moving keys. One undo step.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::KeyOutsidePhase`] when keys lie past `frames`,
    /// [`AnimEditorError::LimitExceeded`], or
    /// [`AnimEditorError::DragActive`].
    pub fn extend_phase(&mut self, phase: Phase, frames: u32) -> Result<(), AnimEditorError<T>> {
        self.edit(|clip, document| {
            clip.set_phase_frames_keeping_keys(phase, frames)?;
            Ok(document.selection.clone())
        })
    }

    /// Moves the selection by `delta` frames (typically `±1` or `±10`),
    /// clamped to the phase. One undo step.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::EmptySelection`],
    /// [`AnimEditorError::KeyCollision`], or
    /// [`AnimEditorError::DragActive`].
    pub fn nudge_selection(&mut self, delta: i64) -> Result<(), AnimEditorError<T>> {
        self.require_selection()?;
        self.edit(|clip, document| Ok(clip.move_keys(&document.selection, delta)?))
    }

    fn require_selection(&self) -> Result<(), AnimEditorError<T>> {
        if self.selection.is_empty() {
            Err(AnimEditorError::EmptySelection)
        } else {
            Ok(())
        }
    }

    // ----- transport -------------------------------------------------------

    /// Starts playback from the playhead (from `0` when at the phase end).
    pub fn play(&mut self) {
        if self.drag.is_some() {
            return;
        }
        if self.playhead >= self.phase_frames() {
            self.playhead = 0;
        }
        self.transport.playing = true;
        self.transport.start_frame = self.playhead;
        self.transport.elapsed = Duration::ZERO;
    }

    pub const fn pause(&mut self) {
        self.transport.playing = false;
    }

    pub fn toggle_play(&mut self) {
        if self.transport.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Whether playback wraps to frame `0` at the phase end instead of
    /// stopping.
    pub const fn set_looping(&mut self, looping: bool) {
        self.transport.looping = looping;
    }

    /// Advances playback by `elapsed` clock time at the clip's timebase.
    /// Returns whether playback is still running.
    pub fn tick(&mut self, elapsed: Duration) -> bool {
        if !self.transport.playing {
            return false;
        }
        self.transport.elapsed = self.transport.elapsed.saturating_add(elapsed);
        let rate = self.draft.timebase().rate();
        let frames = self.transport.elapsed.as_nanos() * u128::from(rate.numerator())
            / (u128::from(rate.denominator()) * NANOS_PER_SECOND);
        let length = u128::from(self.phase_frames());
        let position = u128::from(self.transport.start_frame) + frames;
        if position <= length {
            self.playhead = u32::try_from(position).unwrap_or(u32::MAX);
        } else if self.transport.looping {
            self.playhead = u32::try_from(position % (length + 1)).unwrap_or(0);
        } else {
            self.playhead = self.phase_frames();
            self.transport.playing = false;
        }
        self.transport.playing
    }

    /// Steps the playhead by `delta` frames, clamped to the phase; pauses.
    pub fn step(&mut self, delta: i64) {
        let frame = (i64::from(self.playhead) + delta).clamp(0, i64::from(self.phase_frames()));
        self.set_playhead(clamp_frame(frame));
    }
}

/// Applies a retime handle at `frame` to a copy of the drag origin.
fn retime<T: Copy + Ord>(
    clip: &mut Clip<T>,
    selection: &KeySelection<T>,
    phase: Phase,
    handle: RetimeHandle,
    (first, last): (u32, u32),
    frame: u32,
    alt: bool,
) -> Result<KeySelection<T>, AnimEditorError<T>> {
    Ok(match handle {
        RetimeHandle::SelectionEnd if alt => extend_edge(clip, selection, last, frame)?,
        RetimeHandle::SelectionStart if alt => extend_edge(clip, selection, first, frame)?,
        RetimeHandle::SelectionEnd => {
            clip.retime_selection(selection, first, last - first, frame.saturating_sub(first))?
        }
        RetimeHandle::SelectionStart => {
            clip.retime_selection(selection, last, last - first, last.saturating_sub(frame))?
        }
        RetimeHandle::PhaseEnd if alt => {
            let floor = last_key_frame(clip, phase).unwrap_or(0);
            clip.set_phase_frames_keeping_keys(phase, frame.max(floor))?;
            selection.clone()
        }
        RetimeHandle::PhaseEnd => {
            clip.stretch_phase(phase, frame)?;
            stretched(selection, phase, last, frame)
        }
    })
}

/// `selection` after stretching `phase` from `old` to `new` frames.
fn stretched<T: Copy + Ord>(
    selection: &KeySelection<T>,
    phase: Phase,
    old: u32,
    new: u32,
) -> KeySelection<T> {
    selection
        .iter()
        .map(|key| {
            if key.track.phase == phase {
                KeyRef {
                    track: key.track,
                    frame: stretch_frame(key.frame, old, new),
                }
            } else {
                key
            }
        })
        .collect()
}

fn clamp_frame(frame: i64) -> u32 {
    u32::try_from(frame.max(0)).unwrap_or(u32::MAX)
}

fn last_key_frame<T: Copy + Ord>(clip: &Clip<T>, phase: Phase) -> Option<u32> {
    clip.tracks()
        .iter()
        .filter(|track| track.key.phase == phase)
        .filter_map(|track| track.keys.last().map(|key| key.frame))
        .max()
}

/// Moves only the selected keys on frame `edge` to `to`, leaving the rest of
/// the selection in place: the box grows or shrinks without scaling.
fn extend_edge<T: Copy + Ord>(
    clip: &mut Clip<T>,
    selection: &KeySelection<T>,
    edge: u32,
    to: u32,
) -> Result<KeySelection<T>, AnimEditorError<T>> {
    let edge_keys: KeySelection<T> = selection.iter().filter(|key| key.frame == edge).collect();
    let moved = clip.move_keys(&edge_keys, i64::from(to) - i64::from(edge))?;
    Ok(selection
        .iter()
        .filter(|key| key.frame != edge)
        .chain(moved.iter())
        .collect())
}
