//! The editor document: a local draft [`Grade`], bounded snapshot undo, A/B
//! compare slots, and a grade clipboard.

use core::fmt;
use std::collections::VecDeque;

use fm_grade::{
    BUILTIN_PRESETS, Contrast, CurveChannel, Grade, GradeError, GradeLut, GradePreset,
    HueCurveKind, MAX_CONTRAST_AMOUNT, MAX_EXPOSURE_MILLISTOPS, MAX_LUT_STRENGTH_PERMILLE,
    MAX_PIVOT_BP, MAX_SATURATION_PERMILLE, MAX_WHEEL_MASTER, MAX_WHITE_BALANCE, Wheel, WheelKind,
};

use super::curves::CurveTarget;

/// Most undo (and redo) steps kept; the oldest step is dropped beyond this.
pub const GRADE_UNDO_LIMIT: usize = 100;

/// Every wheel, in display order.
pub const WHEEL_KINDS: [WheelKind; 4] = [
    WheelKind::Lift,
    WheelKind::Gamma,
    WheelKind::Gain,
    WheelKind::Offset,
];

/// A part of a grade that resets as a unit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GradeSection {
    /// Temperature and tint.
    WhiteBalance,
    Exposure,
    /// Contrast amount and pivot.
    Contrast,
    Saturation,
    /// One wheel's colour and master.
    Wheel(WheelKind),
    /// All four wheels.
    Wheels,
    /// One RGB curve.
    Curve(CurveChannel),
    /// All four RGB curves.
    Curves,
    /// One hue or luma curve.
    HueCurve(HueCurveKind),
    /// All three hue and luma curves.
    HueCurves,
    /// The LUT reference.
    Lut,
    /// The whole grade (the bypass state is kept).
    All,
}

impl GradeSection {
    /// The section's value in the identity grade, written into `grade`.
    pub fn reset(self, grade: &mut Grade) {
        let identity = Grade::IDENTITY;
        match self {
            Self::WhiteBalance => grade.white_balance = identity.white_balance,
            Self::Exposure => grade.exposure_millistops = identity.exposure_millistops,
            Self::Contrast => grade.contrast = identity.contrast,
            Self::Saturation => grade.saturation_permille = identity.saturation_permille,
            Self::Wheel(kind) => *wheel_mut(grade, kind) = Wheel::NEUTRAL,
            Self::Wheels => grade.wheels = identity.wheels,
            Self::Curve(channel) => CurveTarget::Rgb(channel).reset(grade),
            Self::Curves => grade.curves = identity.curves,
            Self::HueCurve(kind) => CurveTarget::Hue(kind).reset(grade),
            Self::HueCurves => grade.hue_curves = identity.hue_curves,
            Self::Lut => grade.lut = None,
            Self::All => {
                let enabled = grade.enabled;
                *grade = identity;
                grade.enabled = enabled;
            }
        }
    }

    /// Whether the section already holds its identity value in `grade`.
    #[must_use]
    pub fn is_default(self, grade: &Grade) -> bool {
        let mut reset = grade.clone();
        self.reset(&mut reset);
        reset == *grade
    }
}

/// A scalar control of the grade, for sliders and typed entry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GradeParam {
    /// White-balance temperature, `-1000..=1000` (blue to amber).
    Temperature,
    /// White-balance tint, `-1000..=1000` (green to magenta).
    Tint,
    /// Exposure in thousandths of a stop, `-4000..=4000`.
    Exposure,
    /// Contrast amount, `-1000..=1000`.
    Contrast,
    /// Contrast pivot in basis points, `0..=10000`.
    Pivot,
    /// Saturation per mille, `0..=3000`; `1000` is neutral.
    Saturation,
    /// A wheel's master (luminance) offset, `-1000..=1000`.
    WheelMaster(WheelKind),
    /// LUT mix strength per mille; only meaningful with a LUT.
    LutStrength,
}

impl GradeParam {
    /// The Simple-mode sliders, in display order.
    pub const SIMPLE: [Self; 5] = [
        Self::Exposure,
        Self::Contrast,
        Self::Saturation,
        Self::Temperature,
        Self::Tint,
    ];

    /// Inclusive range of stored values.
    #[must_use]
    pub const fn range(self) -> (i32, i32) {
        match self {
            Self::Temperature | Self::Tint => symmetric(MAX_WHITE_BALANCE),
            Self::Exposure => symmetric(MAX_EXPOSURE_MILLISTOPS),
            Self::Contrast => symmetric(MAX_CONTRAST_AMOUNT),
            Self::Pivot => (0, MAX_PIVOT_BP as i32),
            Self::Saturation => (0, MAX_SATURATION_PERMILLE as i32),
            Self::WheelMaster(_) => symmetric(MAX_WHEEL_MASTER),
            Self::LutStrength => (0, MAX_LUT_STRENGTH_PERMILLE as i32),
        }
    }

    /// The value a double-click resets to.
    #[must_use]
    pub const fn default_value(self) -> i32 {
        match self {
            Self::Pivot => Contrast::NEUTRAL.pivot_bp as i32,
            Self::Saturation => 1000,
            Self::LutStrength => MAX_LUT_STRENGTH_PERMILLE as i32,
            _ => 0,
        }
    }

    /// The stored value in `grade` (`0` strength without a LUT).
    #[must_use]
    pub fn get(self, grade: &Grade) -> i32 {
        match self {
            Self::Temperature => i32::from(grade.white_balance.temperature),
            Self::Tint => i32::from(grade.white_balance.tint),
            Self::Exposure => i32::from(grade.exposure_millistops),
            Self::Contrast => i32::from(grade.contrast.amount),
            Self::Pivot => i32::from(grade.contrast.pivot_bp),
            Self::Saturation => i32::from(grade.saturation_permille),
            Self::WheelMaster(kind) => i32::from(grade.wheels.get(kind).master),
            Self::LutStrength => grade
                .lut
                .as_ref()
                .map_or(0, |lut| i32::from(lut.strength_permille)),
        }
    }

    /// Writes `value`, clamped to [`Self::range`], into `grade`. Strength
    /// without a LUT is ignored.
    pub fn set(self, grade: &mut Grade, value: i32) {
        let (low, high) = self.range();
        let value = value.clamp(low, high);
        // In range of both i16 and u16 after the clamp.
        let signed = i16::try_from(value).unwrap_or_default();
        let unsigned = u16::try_from(value).unwrap_or_default();
        match self {
            Self::Temperature => grade.white_balance.temperature = signed,
            Self::Tint => grade.white_balance.tint = signed,
            Self::Exposure => grade.exposure_millistops = signed,
            Self::Contrast => grade.contrast.amount = signed,
            Self::Pivot => grade.contrast.pivot_bp = unsigned,
            Self::Saturation => grade.saturation_permille = unsigned,
            Self::WheelMaster(kind) => wheel_mut(grade, kind).master = signed,
            Self::LutStrength => {
                if let Some(lut) = &mut grade.lut {
                    lut.strength_permille = unsigned;
                }
            }
        }
    }

    /// The section a reset of this control belongs to.
    #[must_use]
    pub const fn section(self) -> GradeSection {
        match self {
            Self::Temperature | Self::Tint => GradeSection::WhiteBalance,
            Self::Exposure => GradeSection::Exposure,
            Self::Contrast | Self::Pivot => GradeSection::Contrast,
            Self::Saturation => GradeSection::Saturation,
            Self::WheelMaster(kind) => GradeSection::Wheel(kind),
            Self::LutStrength => GradeSection::Lut,
        }
    }
}

const fn symmetric(bound: i16) -> (i32, i32) {
    (-(bound as i32), bound as i32)
}

/// The wheel of `kind` in `grade`, mutably.
pub fn wheel_mut(grade: &mut Grade, kind: WheelKind) -> &mut Wheel {
    match kind {
        WheelKind::Lift => &mut grade.wheels.lift,
        WheelKind::Gamma => &mut grade.wheels.gamma,
        WheelKind::Gain => &mut grade.wheels.gain,
        WheelKind::Offset => &mut grade.wheels.offset,
    }
}

/// One of the two compare slots.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum AbSlot {
    #[default]
    A,
    B,
}

impl AbSlot {
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }
}

/// Why an editor action was refused. A refused action leaves the document
/// unchanged. [`Self::msg_key`] is a stable id for localized messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GradeEditorError {
    /// The edit would break a grade bound.
    Invalid(GradeError),
    NothingToUndo,
    NothingToRedo,
    /// Paste with nothing copied.
    EmptyClipboard,
    /// Recall of a compare slot that holds nothing.
    EmptySlot(AbSlot),
    /// An edit, undo, or new gesture while a gesture is in progress.
    GestureActive,
    /// A gesture update/commit/cancel with no gesture in progress.
    NoGesture,
    /// The curve already has its maximum number of points.
    CurveFull {
        max: usize,
    },
    /// A point already sits at (or too close to) that position.
    PointTaken,
    /// The first and last points of an RGB curve cannot be deleted.
    EndpointLocked,
    /// A referenced curve point does not exist.
    UnknownPoint,
}

impl GradeEditorError {
    /// Stable message id, suitable as a localization key.
    #[must_use]
    pub const fn msg_key(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "grade-editor-invalid",
            Self::NothingToUndo => "grade-editor-nothing-to-undo",
            Self::NothingToRedo => "grade-editor-nothing-to-redo",
            Self::EmptyClipboard => "grade-editor-empty-clipboard",
            Self::EmptySlot(_) => "grade-editor-empty-slot",
            Self::GestureActive => "grade-editor-gesture-active",
            Self::NoGesture => "grade-editor-no-gesture",
            Self::CurveFull { .. } => "grade-editor-curve-full",
            Self::PointTaken => "grade-editor-point-taken",
            Self::EndpointLocked => "grade-editor-endpoint-locked",
            Self::UnknownPoint => "grade-editor-unknown-point",
        }
    }
}

impl From<GradeError> for GradeEditorError {
    fn from(error: GradeError) -> Self {
        Self::Invalid(error)
    }
}

impl fmt::Display for GradeEditorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => write!(formatter, "{}: {error}", self.msg_key()),
            Self::EmptySlot(slot) => write!(formatter, "{}: {slot:?}", self.msg_key()),
            Self::CurveFull { max } => write!(formatter, "{}: at most {max}", self.msg_key()),
            _ => formatter.write_str(self.msg_key()),
        }
    }
}

impl std::error::Error for GradeEditorError {}

/// The colour grading editor's headless state for one grade.
///
/// All edits act on a local draft; each completed action (including a whole
/// gesture such as a wheel or slider drag) is one undo step.
/// [`Self::generation`] changes whenever the draft changes so previews and a
/// future protocol layer can detect staleness. [`Self::is_dirty`] compares
/// the draft with the last saved grade, so undoing back to it is clean.
#[derive(Clone, Debug)]
pub struct GradeDocument {
    draft: Grade,
    saved: Grade,
    base_revision: u64,
    generation: u64,
    undo: VecDeque<Grade>,
    redo: Vec<Grade>,
    gesture: Option<Grade>,
    clipboard: Option<Grade>,
    slots: [Option<Grade>; 2],
    active_slot: AbSlot,
}

impl Default for GradeDocument {
    fn default() -> Self {
        Self::new(Grade::IDENTITY, 0)
    }
}

impl GradeDocument {
    /// A document editing `grade`, which is at authority revision
    /// `base_revision`.
    #[must_use]
    pub fn new(grade: Grade, base_revision: u64) -> Self {
        Self {
            saved: grade.clone(),
            draft: grade,
            base_revision,
            generation: 0,
            undo: VecDeque::new(),
            redo: Vec::new(),
            gesture: None,
            clipboard: None,
            slots: [None, None],
            active_slot: AbSlot::A,
        }
    }

    /// The draft grade.
    #[must_use]
    pub const fn grade(&self) -> &Grade {
        &self.draft
    }

    /// The authority revision the draft is based on.
    #[must_use]
    pub const fn base_revision(&self) -> u64 {
        self.base_revision
    }

    /// Whether the draft differs from the last saved (or authority) grade.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.draft != self.saved
    }

    /// Increments on every draft change.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        self.gesture.is_none() && !self.undo.is_empty()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.gesture.is_none() && !self.redo.is_empty()
    }

    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    #[must_use]
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// Whether a gesture (a drag) is in progress.
    #[must_use]
    pub const fn gesture_active(&self) -> bool {
        self.gesture.is_some()
    }

    /// Whether the grade is bypassed (disabled), so output is the source.
    #[must_use]
    pub const fn is_bypassed(&self) -> bool {
        !self.draft.enabled
    }

    /// The copied grade, if any.
    #[must_use]
    pub const fn clipboard(&self) -> Option<&Grade> {
        self.clipboard.as_ref()
    }

    /// The grade stored in a compare slot.
    #[must_use]
    pub const fn slot(&self, slot: AbSlot) -> Option<&Grade> {
        self.slots[slot.index()].as_ref()
    }

    /// The slot the draft is compared as: [`Self::toggle_ab`] stores the
    /// draft here before switching to the other slot.
    #[must_use]
    pub const fn active_slot(&self) -> AbSlot {
        self.active_slot
    }

    /// The built-in preset the draft matches (ignoring the LUT and bypass).
    #[must_use]
    pub fn matching_preset(&self) -> Option<&'static GradePreset> {
        BUILTIN_PRESETS.iter().find(|preset| {
            let mut draft = self.draft.clone();
            draft.enabled = true;
            draft.lut.clone_from(&preset.grade.lut);
            draft == preset.grade
        })
    }

    /// Applies `edit` to a copy of the draft and adopts it as one undo step
    /// when it validates and differs. Returns whether the draft changed.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::GestureActive`] during a gesture, the error
    /// `edit` returns, or [`GradeEditorError::Invalid`] for a result that
    /// breaks a bound. The draft is unchanged on error.
    pub fn edit<R>(
        &mut self,
        edit: impl FnOnce(&mut Grade) -> Result<R, GradeEditorError>,
    ) -> Result<R, GradeEditorError> {
        self.ensure_idle()?;
        let mut next = self.draft.clone();
        let result = edit(&mut next)?;
        next.validate()?;
        if next != self.draft {
            let before = core::mem::replace(&mut self.draft, next);
            self.push_undo(before);
            self.redo.clear();
            self.bump();
        }
        Ok(result)
    }

    /// Starts a gesture: updates change the draft live and the whole
    /// gesture becomes one undo step on [`Self::commit_gesture`].
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::GestureActive`] when one is already running.
    pub fn begin_gesture(&mut self) -> Result<(), GradeEditorError> {
        self.ensure_idle()?;
        self.gesture = Some(self.draft.clone());
        Ok(())
    }

    /// Applies `edit` to the draft during a gesture.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::NoGesture`] without a gesture, the error `edit`
    /// returns, or [`GradeEditorError::Invalid`]; the draft is unchanged on
    /// error.
    pub fn update_gesture<R>(
        &mut self,
        edit: impl FnOnce(&mut Grade) -> Result<R, GradeEditorError>,
    ) -> Result<R, GradeEditorError> {
        if self.gesture.is_none() {
            return Err(GradeEditorError::NoGesture);
        }
        let mut next = self.draft.clone();
        let result = edit(&mut next)?;
        next.validate()?;
        if next != self.draft {
            self.draft = next;
            self.bump();
        }
        Ok(result)
    }

    /// Ends the gesture, recording one undo step if the draft changed.
    /// Returns whether it changed.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::NoGesture`] without a gesture.
    pub fn commit_gesture(&mut self) -> Result<bool, GradeEditorError> {
        let before = self.gesture.take().ok_or(GradeEditorError::NoGesture)?;
        let changed = before != self.draft;
        if changed {
            self.push_undo(before);
            self.redo.clear();
        }
        Ok(changed)
    }

    /// Ends the gesture and restores the draft from before it.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::NoGesture`] without a gesture.
    pub fn cancel_gesture(&mut self) -> Result<(), GradeEditorError> {
        let before = self.gesture.take().ok_or(GradeEditorError::NoGesture)?;
        if before != self.draft {
            self.draft = before;
            self.bump();
        }
        Ok(())
    }

    /// Sets one scalar control (clamped to its range); one undo step.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::GestureActive`] during a gesture.
    pub fn set_param(&mut self, param: GradeParam, value: i32) -> Result<(), GradeEditorError> {
        self.edit(|grade| {
            param.set(grade, value);
            Ok(())
        })
    }

    /// Resets one scalar control to its default; one undo step.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::GestureActive`] during a gesture.
    pub fn reset_param(&mut self, param: GradeParam) -> Result<(), GradeEditorError> {
        self.set_param(param, param.default_value())
    }

    /// Resets a section to its identity value; one undo step.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::GestureActive`] during a gesture.
    pub fn reset_section(&mut self, section: GradeSection) -> Result<(), GradeEditorError> {
        self.edit(|grade| {
            section.reset(grade);
            Ok(())
        })
    }

    /// Replaces one wheel; one undo step.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::Invalid`] outside the wheel radius, or
    /// [`GradeEditorError::GestureActive`].
    pub fn set_wheel(&mut self, kind: WheelKind, wheel: Wheel) -> Result<(), GradeEditorError> {
        self.edit(|grade| {
            *wheel_mut(grade, kind) = wheel;
            Ok(())
        })
    }

    /// Sets or clears the LUT reference; one undo step.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::Invalid`] for a bad URI, or
    /// [`GradeEditorError::GestureActive`].
    pub fn set_lut(&mut self, lut: Option<GradeLut>) -> Result<(), GradeEditorError> {
        self.edit(|grade| {
            grade.lut = lut;
            Ok(())
        })
    }

    /// Bypasses (disables) or re-enables the grade; one undo step.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::GestureActive`] during a gesture.
    pub fn set_bypass(&mut self, bypass: bool) -> Result<(), GradeEditorError> {
        self.edit(|grade| {
            grade.enabled = !bypass;
            Ok(())
        })
    }

    /// Flips [`Self::is_bypassed`]; one undo step.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::GestureActive`] during a gesture.
    pub fn toggle_bypass(&mut self) -> Result<(), GradeEditorError> {
        self.set_bypass(!self.is_bypassed())
    }

    /// Replaces every setting with `preset`'s, keeping the LUT reference and
    /// the bypass state; one undo step.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::GestureActive`] during a gesture.
    pub fn apply_preset(&mut self, preset: &GradePreset) -> Result<(), GradeEditorError> {
        self.edit(|grade| {
            let lut = grade.lut.take();
            let enabled = grade.enabled;
            *grade = preset.grade.clone();
            grade.lut = lut;
            grade.enabled = enabled;
            Ok(())
        })
    }

    /// Adds a point to a curve at normalized `at` (see
    /// [`CurveTarget::add_point`]); one undo step. Returns its index.
    ///
    /// # Errors
    ///
    /// The curve's refusal, or [`GradeEditorError::GestureActive`].
    pub fn add_curve_point(
        &mut self,
        target: CurveTarget,
        at: [f32; 2],
    ) -> Result<usize, GradeEditorError> {
        self.edit(|grade| target.add_point(grade, at))
    }

    /// Deletes a curve point; one undo step.
    ///
    /// # Errors
    ///
    /// The curve's refusal, or [`GradeEditorError::GestureActive`].
    pub fn delete_curve_point(
        &mut self,
        target: CurveTarget,
        index: usize,
    ) -> Result<(), GradeEditorError> {
        self.edit(|grade| target.delete_point(grade, index))
    }

    /// Copies the draft to the grade clipboard.
    pub fn copy(&mut self) {
        self.clipboard = Some(self.draft.clone());
    }

    /// Replaces the draft with the copied grade, keeping the bypass state;
    /// one undo step.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::EmptyClipboard`] with nothing copied, or
    /// [`GradeEditorError::GestureActive`].
    pub fn paste(&mut self) -> Result<(), GradeEditorError> {
        let copied = self
            .clipboard
            .clone()
            .ok_or(GradeEditorError::EmptyClipboard)?;
        self.replace_keeping_bypass(copied)
    }

    /// Puts a grade on the clipboard (for example one copied elsewhere).
    pub fn set_clipboard(&mut self, grade: Option<Grade>) {
        self.clipboard = grade;
    }

    /// Stores the draft in `slot`.
    pub fn store_slot(&mut self, slot: AbSlot) {
        self.slots[slot.index()] = Some(self.draft.clone());
    }

    /// Replaces the draft with `slot`'s grade, keeping the bypass state, and
    /// makes it the active slot; one undo step.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::EmptySlot`] or
    /// [`GradeEditorError::GestureActive`].
    pub fn recall_slot(&mut self, slot: AbSlot) -> Result<(), GradeEditorError> {
        let stored = self.slots[slot.index()]
            .clone()
            .ok_or(GradeEditorError::EmptySlot(slot))?;
        self.replace_keeping_bypass(stored)?;
        self.active_slot = slot;
        Ok(())
    }

    /// A/B compare: stores the draft in the active slot and switches to the
    /// other one, loading its grade (one undo step). An empty other slot
    /// starts as a copy of the draft, so the first press sets up the
    /// comparison without changing the picture.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::GestureActive`] during a gesture.
    pub fn toggle_ab(&mut self) -> Result<(), GradeEditorError> {
        self.ensure_idle()?;
        self.store_slot(self.active_slot);
        let other = self.active_slot.other();
        if self.slots[other.index()].is_none() {
            self.store_slot(other);
        }
        self.recall_slot(other)
    }

    /// Forgets both compare slots.
    pub fn clear_slots(&mut self) {
        self.slots = [None, None];
        self.active_slot = AbSlot::A;
    }

    /// Restores the draft from before the last edit.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::GestureActive`] during a gesture;
    /// [`GradeEditorError::NothingToUndo`] with no history.
    pub fn undo(&mut self) -> Result<(), GradeEditorError> {
        self.ensure_idle()?;
        let previous = self
            .undo
            .pop_back()
            .ok_or(GradeEditorError::NothingToUndo)?;
        let current = core::mem::replace(&mut self.draft, previous);
        self.redo.push(current);
        if self.redo.len() > GRADE_UNDO_LIMIT {
            self.redo.remove(0);
        }
        self.bump();
        Ok(())
    }

    /// Reapplies the last undone edit.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::GestureActive`] during a gesture;
    /// [`GradeEditorError::NothingToRedo`] with nothing undone.
    pub fn redo(&mut self) -> Result<(), GradeEditorError> {
        self.ensure_idle()?;
        let next = self.redo.pop().ok_or(GradeEditorError::NothingToRedo)?;
        let current = core::mem::replace(&mut self.draft, next);
        self.push_undo(current);
        self.bump();
        Ok(())
    }

    /// Replaces the draft with the authority's `grade` at `revision`,
    /// dropping history and any gesture.
    pub fn reset_from_authority(&mut self, grade: Grade, revision: u64) {
        self.saved = grade.clone();
        self.draft = grade;
        self.base_revision = revision;
        self.undo.clear();
        self.redo.clear();
        self.gesture = None;
        self.bump();
    }

    /// Records that the current draft was saved as authority `revision`.
    /// History is kept.
    pub fn mark_saved(&mut self, revision: u64) {
        self.saved = self.draft.clone();
        self.base_revision = revision;
    }

    fn replace_keeping_bypass(&mut self, grade: Grade) -> Result<(), GradeEditorError> {
        self.edit(|draft| {
            let enabled = draft.enabled;
            *draft = grade;
            draft.enabled = enabled;
            Ok(())
        })
    }

    fn push_undo(&mut self, before: Grade) {
        self.undo.push_back(before);
        while self.undo.len() > GRADE_UNDO_LIMIT {
            self.undo.pop_front();
        }
    }

    const fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    const fn ensure_idle(&self) -> Result<(), GradeEditorError> {
        if self.gesture.is_some() {
            Err(GradeEditorError::GestureActive)
        } else {
            Ok(())
        }
    }
}
