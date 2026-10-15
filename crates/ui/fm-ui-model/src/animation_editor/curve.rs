//! Custom cubic-bezier easing (GX-011): the curve editor's headless core.
//!
//! A key's easing shapes the segment from that key to the next one on its
//! track. [`Easing::CubicBezier`] is drawn in a unit square from `(0, 0)`
//! (the key) to `(1, 1)` (the next key) with two handles: [`CurveHandle::Out`]
//! at `(x1, y1)` leaves the key, [`CurveHandle::In`] at `(x2, y2)` arrives at
//! the next key. In After Effects terms a handle's `x` is its *influence*
//! (how far along the segment it reaches) and its slope is the *speed* at
//! the key.
//!
//! The editor works in unit coordinates (`f64`) and stores Q16 controls:
//! [`constrain_point`] clamps `x` to `0..=1` (so time never runs backwards)
//! and `y` to `-1..=2` (the overshoot [`fm_animation`] permits), then rounds
//! to Q16. [`snap_point`] pulls each axis onto common values (`0`, quarters,
//! thirds, `1`, and the overshoot marks) unless shift is held.
//!
//! Edits target a [`CurveTarget`]: the key selection (the inspector's curve
//! editor) or one segment (a handle on the value graph). A handle drag is a
//! [`DragKind::Curve`] session on the shared drag machinery, so the draft
//! shows the live curve, [`AnimDocument::commit_drag`] records the whole
//! drag as one undo step, and [`AnimDocument::cancel_drag`] restores it.
//! Numeric entry ([`AnimDocument::set_curve_control`]) and presets
//! ([`AnimDocument::apply_curve_preset`]) are one undo step each.

use fm_animation::{
    CubicBezier, EASE_ONE, Easing, EasingPreset, KeyRef, KeySelection, MAX_BEZIER_Y, MIN_BEZIER_Y,
    TrackKey, Value,
};

use super::document::{AnimDocument, AnimEditorError};
use super::interaction::{DragKind, DragSession, Modifiers};

/// Snap targets for a handle's `x` (influence): the ends, quarters, thirds,
/// and the middle.
pub const CURVE_SNAP_X: [f64; 7] = [0.0, 0.25, 1.0 / 3.0, 0.5, 2.0 / 3.0, 0.75, 1.0];
/// Snap targets for a handle's `y`: those of `x` plus the usual
/// anticipation and overshoot marks.
pub const CURVE_SNAP_Y: [f64; 9] = [-0.5, 0.0, 0.25, 1.0 / 3.0, 0.5, 2.0 / 3.0, 0.75, 1.0, 1.5];
/// Default snap distance in unit coordinates.
pub const DEFAULT_CURVE_SNAP: f64 = 0.03;
/// Lowest control `y` in unit coordinates (`-1`).
pub const MIN_CURVE_Y: f64 = -1.0;
/// Highest control `y` in unit coordinates (`2`).
pub const MAX_CURVE_Y: f64 = 2.0;

const Q16: f64 = 65_536.0;

/// One of a cubic bezier's two handles.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CurveHandle {
    /// `(x1, y1)`: leaves the segment's first key.
    Out,
    /// `(x2, y2)`: arrives at the segment's second key.
    In,
}

impl CurveHandle {
    pub const ALL: [Self; 2] = [Self::Out, Self::In];

    /// The end of the unit square the handle hangs from: `(0, 0)` for
    /// [`Self::Out`], `(1, 1)` for [`Self::In`].
    #[must_use]
    pub const fn anchor(self) -> [f64; 2] {
        match self {
            Self::Out => [0.0, 0.0],
            Self::In => [1.0, 1.0],
        }
    }
}

/// One of the four numbers of a cubic bezier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CurveControl {
    X1,
    Y1,
    X2,
    Y2,
}

impl CurveControl {
    pub const ALL: [Self; 4] = [Self::X1, Self::Y1, Self::X2, Self::Y2];

    /// The handle this number belongs to.
    #[must_use]
    pub const fn handle(self) -> CurveHandle {
        match self {
            Self::X1 | Self::Y1 => CurveHandle::Out,
            Self::X2 | Self::Y2 => CurveHandle::In,
        }
    }

    /// Whether this is a horizontal (time) number.
    #[must_use]
    pub const fn is_x(self) -> bool {
        matches!(self, Self::X1 | Self::X2)
    }

    /// The permitted range in unit coordinates.
    #[must_use]
    pub const fn range(self) -> (f64, f64) {
        if self.is_x() {
            (0.0, 1.0)
        } else {
            (MIN_CURVE_Y, MAX_CURVE_Y)
        }
    }

    /// This number of `curve` in unit coordinates.
    #[must_use]
    pub fn value(self, curve: CubicBezier) -> f64 {
        let [x1, y1, x2, y2] = unit_controls(curve);
        match self {
            Self::X1 => x1,
            Self::Y1 => y1,
            Self::X2 => x2,
            Self::Y2 => y2,
        }
    }
}

/// A bezier to start a custom curve from, approximating a familiar shape.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CurvePreset {
    /// Starts slowly, arrives at full speed (CSS `ease-in`).
    EaseIn,
    /// Leaves at full speed, settles gently (CSS `ease-out`).
    EaseOut,
    /// Slow at both keys (CSS `ease-in-out`).
    EaseInOut,
    /// Overshoots the next key's value, then settles back.
    Back,
    /// Pulls back below the starting value before moving.
    Anticipate,
}

impl CurvePreset {
    /// Every preset, in toolbar order.
    pub const ALL: [Self; 5] = [
        Self::EaseIn,
        Self::EaseOut,
        Self::EaseInOut,
        Self::Back,
        Self::Anticipate,
    ];

    /// A stable id for diagnostics and tests.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::EaseIn => "ease-in",
            Self::EaseOut => "ease-out",
            Self::EaseInOut => "ease-in-out",
            Self::Back => "back",
            Self::Anticipate => "anticipate",
        }
    }

    /// The preset's controls in unit coordinates `[x1, y1, x2, y2]`.
    #[must_use]
    pub const fn controls(self) -> [f64; 4] {
        match self {
            Self::EaseIn => [0.42, 0.0, 1.0, 1.0],
            Self::EaseOut => [0.0, 0.0, 0.58, 1.0],
            Self::EaseInOut => [0.42, 0.0, 0.58, 1.0],
            Self::Back => [0.34, 1.56, 0.64, 1.0],
            Self::Anticipate => [0.36, -0.56, 0.66, 1.0],
        }
    }

    /// The preset as a curve.
    #[must_use]
    pub fn curve(self) -> CubicBezier {
        curve_from_unit(self.controls())
    }
}

/// The axes a handle snapped on, as Q16 values.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct CurveSnap {
    pub x: Option<u32>,
    pub y: Option<i32>,
}

impl CurveSnap {
    pub const NONE: Self = Self { x: None, y: None };

    /// The snapped `x` in unit coordinates, for a guide line.
    #[must_use]
    pub fn unit_x(self) -> Option<f64> {
        self.x.map(|x| f64::from(x) / Q16)
    }

    /// The snapped `y` in unit coordinates, for a guide line.
    #[must_use]
    pub fn unit_y(self) -> Option<f64> {
        self.y.map(|y| f64::from(y) / Q16)
    }
}

/// What a curve edit changes: the easing of every selected key, or of one
/// segment's first key.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CurveTarget<T> {
    Selection,
    Segment(KeyRef<T>),
}

/// The curve a custom edit starts from when a key's easing has no bezier
/// form (a hold) or keys disagree: CSS `ease`.
#[must_use]
pub fn default_curve() -> CubicBezier {
    curve_from_unit([0.25, 0.1, 0.25, 1.0])
}

/// The bezier an edit of `easing` starts from: the curve itself, a named
/// easing's approximation ([`EasingPreset::to_bezier`]), or
/// [`default_curve`] for a hold.
#[must_use]
pub fn curve_seed(easing: Easing) -> CubicBezier {
    match easing {
        Easing::CubicBezier(curve) => curve,
        other => EasingPreset::from_easing(other)
            .and_then(EasingPreset::to_bezier)
            .unwrap_or_else(default_curve),
    }
}

/// `curve`'s controls in unit coordinates `[x1, y1, x2, y2]`.
#[must_use]
pub fn unit_controls(curve: CubicBezier) -> [f64; 4] {
    let (x1, y1, x2, y2) = curve.control_points();
    [
        f64::from(x1) / Q16,
        f64::from(y1) / Q16,
        f64::from(x2) / Q16,
        f64::from(y2) / Q16,
    ]
}

/// The position of `handle` in unit coordinates.
#[must_use]
pub fn handle_point(curve: CubicBezier, handle: CurveHandle) -> [f64; 2] {
    let [x1, y1, x2, y2] = unit_controls(curve);
    match handle {
        CurveHandle::Out => [x1, y1],
        CurveHandle::In => [x2, y2],
    }
}

/// A unit-coordinate point clamped into the permitted control range and
/// rounded to Q16. Non-finite coordinates read as `0`.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped first.
pub fn constrain_point([x, y]: [f64; 2]) -> (u32, i32) {
    let finite = |value: f64| if value.is_finite() { value } else { 0.0 };
    let x = (finite(x).clamp(0.0, 1.0) * Q16).round() as u32;
    let y = (finite(y).clamp(MIN_CURVE_Y, MAX_CURVE_Y) * Q16).round() as i32;
    (x.min(EASE_ONE), y.clamp(MIN_BEZIER_Y, MAX_BEZIER_Y))
}

/// Pulls each axis of `point` onto the nearest of [`CURVE_SNAP_X`] /
/// [`CURVE_SNAP_Y`] within `threshold` (unit coordinates).
#[must_use]
pub fn snap_point([x, y]: [f64; 2], threshold: f64) -> ([f64; 2], CurveSnap) {
    let nearest = |value: f64, targets: &[f64]| {
        targets
            .iter()
            .copied()
            .filter(|target| (target - value).abs() <= threshold)
            .min_by(|a, b| (a - value).abs().total_cmp(&(b - value).abs()))
    };
    let snapped_x = nearest(x, &CURVE_SNAP_X);
    let snapped_y = nearest(y, &CURVE_SNAP_Y);
    let point = [snapped_x.unwrap_or(x), snapped_y.unwrap_or(y)];
    let (qx, qy) = constrain_point(point);
    (
        point,
        CurveSnap {
            x: snapped_x.map(|_| qx),
            y: snapped_y.map(|_| qy),
        },
    )
}

/// `curve` with `handle` moved to `point` (unit coordinates): snapped
/// within `snap` unless it is `None`, then constrained.
#[must_use]
pub fn with_handle(
    curve: CubicBezier,
    handle: CurveHandle,
    point: [f64; 2],
    snap: Option<f64>,
) -> (CubicBezier, CurveSnap) {
    let (point, snapped) = match snap {
        Some(threshold) => snap_point(point, threshold),
        None => (point, CurveSnap::NONE),
    };
    let (x, y) = constrain_point(point);
    let (x1, y1, x2, y2) = curve.control_points();
    let (x1, y1, x2, y2) = match handle {
        CurveHandle::Out => (x, y, x2, y2),
        CurveHandle::In => (x1, y1, x, y),
    };
    (CubicBezier::new(x1, y1, x2, y2).unwrap_or(curve), snapped)
}

/// `curve` with one number set to `value` (unit coordinates, clamped).
#[must_use]
pub fn with_control(curve: CubicBezier, control: CurveControl, value: f64) -> CubicBezier {
    let mut point = handle_point(curve, control.handle());
    point[usize::from(!control.is_x())] = value;
    with_handle(curve, control.handle(), point, None).0
}

/// A curve from unit controls `[x1, y1, x2, y2]`, constrained.
#[must_use]
pub fn curve_from_unit([x1, y1, x2, y2]: [f64; 4]) -> CubicBezier {
    let (x1, y1) = constrain_point([x1, y1]);
    let (x2, y2) = constrain_point([x2, y2]);
    // constrain_point keeps every control in range.
    CubicBezier::new(x1, y1, x2, y2).unwrap_or(STRAIGHT)
}

/// The straight diagonal, a fallback that is always valid.
const STRAIGHT: CubicBezier = match CubicBezier::new(0, 0, EASE_ONE, 1 << 16) {
    Ok(curve) => curve,
    Err(_) => panic!("the diagonal is a valid curve"),
};

/// The eased value of `curve` at `progress` (`0..=1`), in unit
/// coordinates. Exact to Q16, as the engine samples it.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped first.
pub fn sample_curve(curve: CubicBezier, progress: f64) -> f64 {
    let progress = if progress.is_finite() { progress } else { 0.0 };
    let q = (progress.clamp(0.0, 1.0) * Q16).round() as u32;
    f64::from(Easing::CubicBezier(curve).apply(q)) / Q16
}

/// A point of the bezier itself (not the eased function) at parameter `t`,
/// for drawing the curve with its handles.
#[must_use]
pub fn bezier_point(curve: CubicBezier, t: f64) -> [f64; 2] {
    let [x1, y1, x2, y2] = unit_controls(curve);
    let u = 1.0 - t;
    let b1 = 3.0 * u * u * t;
    let b2 = 3.0 * u * t * t;
    let b3 = t * t * t;
    [b1 * x1 + b2 * x2 + b3, b1 * y1 + b2 * y2 + b3]
}

/// The segment from a key to the next key on its track.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Segment<T> {
    /// The first key, whose easing shapes the segment.
    pub key: KeyRef<T>,
    pub end_frame: u32,
    pub start_value: Value,
    pub end_value: Value,
    pub easing: Easing,
}

impl<T> Segment<T> {
    /// The segment's length in frames (at least `1`).
    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.end_frame - self.key.frame
    }

    /// `handle`'s influence: how far along the segment it reaches, `0..=1`.
    #[must_use]
    pub fn influence(curve: CubicBezier, handle: CurveHandle) -> f64 {
        let [x, _] = handle_point(curve, handle);
        match handle {
            CurveHandle::Out => x,
            CurveHandle::In => 1.0 - x,
        }
    }

    /// The speed at `handle`'s key in stored value units per frame (for a
    /// scalar segment), from the handle's slope; `None` for colours or a
    /// handle with no influence (an instant jump).
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // Display only.
    pub fn speed_per_frame(&self, curve: CubicBezier, handle: CurveHandle) -> Option<f64> {
        let (Value::Scalar(from), Value::Scalar(to)) = (self.start_value, self.end_value) else {
            return None;
        };
        let [x, y] = handle_point(curve, handle);
        let [anchor_x, anchor_y] = handle.anchor();
        let run = (x - anchor_x).abs();
        if run < f64::EPSILON {
            return None;
        }
        let slope = (y - anchor_y) / (x - anchor_x);
        Some(slope * (to - from) as f64 / f64::from(self.frames()))
    }
}

impl<T: Copy + Ord> AnimDocument<T> {
    /// The segment starting at `key`, `None` when the key is missing or is
    /// the last on its track.
    #[must_use]
    pub fn segment(&self, key: KeyRef<T>) -> Option<Segment<T>> {
        let track = self.draft.track(key.track)?;
        let index = track.key_index(key.frame)?;
        let (start, end) = (track.keys.get(index)?, track.keys.get(index + 1)?);
        Some(Segment {
            key,
            end_frame: end.frame,
            start_value: start.value,
            end_value: end.value,
            easing: start.easing,
        })
    }

    /// The segment of `track` that `frame` falls in (from a key up to, not
    /// including, the next key).
    #[must_use]
    pub fn segment_at(&self, track: TrackKey<T>, frame: u32) -> Option<Segment<T>> {
        let keys = &self.draft.track(track)?.keys;
        let start = keys
            .windows(2)
            .find(|pair| pair[0].frame <= frame && frame < pair[1].frame)?[0]
            .frame;
        self.segment(KeyRef {
            track,
            frame: start,
        })
    }

    /// The keys `target` edits.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::EmptySelection`] for an empty selection;
    /// [`AnimEditorError::UnknownKey`] for a missing segment key;
    /// [`AnimEditorError::NoSegment`] for the last key of a track.
    pub fn curve_keys(
        &self,
        target: CurveTarget<T>,
    ) -> Result<KeySelection<T>, AnimEditorError<T>> {
        match target {
            CurveTarget::Selection => {
                if self.selection.is_empty() {
                    Err(AnimEditorError::EmptySelection)
                } else {
                    Ok(self.selection.clone())
                }
            }
            CurveTarget::Segment(key) => {
                if self.draft.key(key.track, key.frame).is_none() {
                    return Err(AnimEditorError::UnknownKey {
                        track: key.track,
                        frame: key.frame,
                    });
                }
                if self.segment(key).is_none() {
                    return Err(AnimEditorError::NoSegment {
                        track: key.track,
                        frame: key.frame,
                    });
                }
                Ok(core::iter::once(key).collect())
            }
        }
    }

    /// The curve `target` shows: its keys' shared easing as a bezier
    /// ([`curve_seed`]), or [`default_curve`] when they differ.
    ///
    /// # Errors
    ///
    /// As [`Self::curve_keys`].
    pub fn curve(&self, target: CurveTarget<T>) -> Result<CubicBezier, AnimEditorError<T>> {
        let keys = self.curve_keys(target)?;
        let mut easings = keys
            .iter()
            .filter_map(|key| self.draft.key(key.track, key.frame))
            .map(|key| key.easing);
        let first = easings.next();
        Ok(match first {
            Some(easing) if easings.all(|other| other == easing) => curve_seed(easing),
            _ => default_curve(),
        })
    }

    /// Sets `target`'s easing to `curve`. One undo step.
    ///
    /// # Errors
    ///
    /// As [`Self::curve_keys`], or [`AnimEditorError::DragActive`].
    pub fn set_curve(
        &mut self,
        target: CurveTarget<T>,
        curve: CubicBezier,
    ) -> Result<(), AnimEditorError<T>> {
        self.ensure_idle()?;
        let keys = self.curve_keys(target)?;
        self.edit(|clip, document| {
            clip.set_easing(&keys, Easing::CubicBezier(curve))?;
            Ok(document.selection.clone())
        })
    }

    /// Sets one number of `target`'s curve to `value` (unit coordinates,
    /// clamped to the permitted range). One undo step.
    ///
    /// # Errors
    ///
    /// As [`Self::set_curve`].
    pub fn set_curve_control(
        &mut self,
        target: CurveTarget<T>,
        control: CurveControl,
        value: f64,
    ) -> Result<(), AnimEditorError<T>> {
        let curve = with_control(self.curve(target)?, control, value);
        self.set_curve(target, curve)
    }

    /// Starts `target` from `preset`. One undo step.
    ///
    /// # Errors
    ///
    /// As [`Self::set_curve`].
    pub fn apply_curve_preset(
        &mut self,
        target: CurveTarget<T>,
        preset: CurvePreset,
    ) -> Result<(), AnimEditorError<T>> {
        self.set_curve(target, preset.curve())
    }

    /// Starts dragging `handle` of `target`'s curve. Returns the curve the
    /// drag starts from.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::DragActive`] or as [`Self::curve_keys`].
    pub fn begin_curve_drag(
        &mut self,
        target: CurveTarget<T>,
        handle: CurveHandle,
    ) -> Result<CubicBezier, AnimEditorError<T>> {
        self.ensure_idle()?;
        let keys = self.curve_keys(target)?;
        let start = self.curve(target)?;
        self.transport.playing = false;
        self.drag = Some(DragSession::new(
            DragKind::Curve {
                keys,
                handle,
                start,
                current: start,
                snap: CurveSnap::NONE,
            },
            self.snapshot(),
            self.playhead,
        ));
        Ok(start)
    }

    /// Moves the dragged handle to `point` (unit coordinates), snapping
    /// within `snap_threshold` unless shift is held, and shows the result
    /// live. Returns the axes snapped on.
    ///
    /// # Errors
    ///
    /// [`AnimEditorError::NoDrag`] without a curve drag; otherwise the
    /// clip's refusal, in which case the last valid curve is kept.
    pub fn update_curve_drag(
        &mut self,
        point: [f64; 2],
        modifiers: Modifiers,
        snap_threshold: f64,
    ) -> Result<CurveSnap, AnimEditorError<T>> {
        let Some(DragKind::Curve {
            keys,
            handle,
            start,
            ..
        }) = self.drag.as_ref().map(DragSession::kind).cloned()
        else {
            return Err(AnimEditorError::NoDrag);
        };
        let snap = (!modifiers.shift).then_some(snap_threshold);
        let (curve, snapped) = with_handle(start, handle, point, snap);
        let mut clip = self
            .drag
            .as_ref()
            .map(|drag| drag.origin().clip.clone())
            .ok_or(AnimEditorError::NoDrag)?;
        let result = clip.set_easing(&keys, Easing::CubicBezier(curve));
        if let Err(error) = result {
            let error = AnimEditorError::from(error);
            if let Some(drag) = self.drag.as_mut() {
                drag.set_rejected(Some(error.clone()));
            }
            return Err(error);
        }
        if clip != self.draft {
            self.draft = clip;
            self.bump();
        }
        if let Some(drag) = self.drag.as_mut() {
            drag.set_curve(curve, snapped);
            drag.set_rejected(None);
        }
        Ok(snapped)
    }

    /// The curve of the curve drag in progress and the handle dragged.
    #[must_use]
    pub fn curve_drag(&self) -> Option<(CurveHandle, CubicBezier, CurveSnap)> {
        match self.drag.as_ref().map(DragSession::kind) {
            Some(DragKind::Curve {
                handle,
                current,
                snap,
                ..
            }) => Some((*handle, *current, *snap)),
            _ => None,
        }
    }
}
