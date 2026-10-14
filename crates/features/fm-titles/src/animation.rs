//! Title animation: one [`fm_animation::Clip`] per template, keyed by
//! element.
//!
//! A [`TitleTemplate`] carries a [`TitleAnimation`]: an In and an Out phase
//! in whole frames of the clip's own [`Timebase`] (30/1 by default), plus a
//! revision that advances on every applied edit. Tracks animate one
//! [`Property`] of one element in one [`Phase`]; an element or property with
//! no track keeps its static value. The renderer samples the clip at an
//! [`AnimSample`] (see [`crate::ReferenceRenderer::render_sampled`]):
//!
//! * `PositionX`/`PositionY` add pixels to the element box.
//! * `ScaleX`/`ScaleY` (basis points) scale the laid-out box about its centre;
//!   text is re-laid out inside the scaled box at a font size scaled by the
//!   smaller factor and capped at [`crate::MAX_FONT_SIZE_PX`].
//! * `Rotation` (millidegrees, clockwise on screen) turns the drawn element
//!   about the centre of its scaled box.
//! * `Opacity` multiplies into the static opacity.
//! * `CropLeft`/`Top`/`Right`/`Bottom` hide that many pixels of the scaled
//!   box from each edge.
//! * `Color` overrides the fill and text color.

use crate::{ElementId, TitleTemplate};
use core::fmt;
use fm_animation::{
    AnimationPreset, Clip, ClipError, ClipState, Phase, PresetGeometry, Property, Rgba, Timebase,
    Value,
};
use fm_types::FrameRate;

/// Where in its life a title is sampled: static, In at a clip position,
/// Hold (on air), or Out at a clip position.
pub type AnimSample = ClipState;

/// The clip type every title animation uses.
pub type TitleClip = Clip<ElementId>;

/// The timebase a new title animation is authored in: 30 frames per second.
#[must_use]
pub fn default_title_timebase() -> Timebase {
    // 30/1 has a nonzero numerator and denominator.
    Timebase::new(FrameRate::new(30, 1).unwrap_or_else(|_| unreachable!("30/1 is a valid rate")))
}

/// A template's keyframe animation and its edit revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleAnimation {
    pub clip: TitleClip,
    /// Advances once per applied edit; an optimistic-concurrency counter.
    pub revision: u64,
}

impl Default for TitleAnimation {
    /// An empty clip of zero-length phases at [`default_title_timebase`].
    fn default() -> Self {
        Self::empty(default_title_timebase())
    }
}

impl TitleAnimation {
    /// An empty clip of zero-length phases at `timebase`, revision zero.
    #[must_use]
    pub fn empty(timebase: Timebase) -> Self {
        Self {
            // Zero-length phases are always inside MAX_PHASE_FRAMES.
            clip: Clip::new(timebase, 0, 0)
                .unwrap_or_else(|_| unreachable!("an empty clip is valid")),
            revision: 0,
        }
    }

    #[must_use]
    pub const fn new(clip: TitleClip, revision: u64) -> Self {
        Self { clip, revision }
    }

    /// Whether no element is animated.
    #[must_use]
    pub fn is_static(&self) -> bool {
        self.clip.tracks().is_empty()
    }
}

/// Why a template animation edit was refused. A refused edit changes
/// nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnimationEditError {
    /// The template has no element with this id.
    UnknownElement(ElementId),
    /// The clip refused the edit.
    Clip(ClipError<ElementId>),
    /// The revision counter is exhausted.
    RevisionExhausted,
}

impl fmt::Display for AnimationEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownElement(element) => write!(formatter, "unknown title element {element}"),
            Self::Clip(error) => write!(formatter, "title animation edit refused: {error}"),
            Self::RevisionExhausted => formatter.write_str("title animation revision is exhausted"),
        }
    }
}

impl std::error::Error for AnimationEditError {}

impl From<ClipError<ElementId>> for AnimationEditError {
    fn from(error: ClipError<ElementId>) -> Self {
        Self::Clip(error)
    }
}

impl TitleTemplate {
    /// Applies a built-in preset to one element over `frames` frames of
    /// `phase`, growing the phase to `frames` when it is shorter (keys
    /// already in the phase stay where they are). Off-canvas distances and
    /// the wipe extent come from the element's static bounds and the canvas.
    /// Atomic; returns the new animation revision.
    ///
    /// # Errors
    ///
    /// [`AnimationEditError`]; nothing changes on error.
    pub fn apply_animation_preset(
        &mut self,
        element: ElementId,
        phase: Phase,
        preset: AnimationPreset,
        frames: u32,
    ) -> Result<u64, AnimationEditError> {
        let bounds = self
            .elements
            .iter()
            .find(|candidate| candidate.id == element)
            .map(|candidate| candidate.bounds)
            .ok_or(AnimationEditError::UnknownElement(element))?;
        let geometry = PresetGeometry {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
            canvas_width: self.width,
            canvas_height: self.height,
        };
        self.edit_animation(|clip| {
            let length = clip.phase_frames(phase).max(frames);
            clip.set_phase_frames_keeping_keys(phase, length)?;
            clip.apply_preset(element, phase, preset, frames, geometry)
        })
    }

    /// Scales every key of `phase` to a new phase length (see
    /// [`fm_animation::Clip::stretch_phase`]). Atomic; returns the new
    /// animation revision.
    ///
    /// # Errors
    ///
    /// [`AnimationEditError`]; nothing changes on error.
    pub fn stretch_animation(
        &mut self,
        phase: Phase,
        frames: u32,
    ) -> Result<u64, AnimationEditError> {
        self.edit_animation(|clip| clip.stretch_phase(phase, frames))
    }

    /// Runs `edit` on a copy of the clip and commits it with a new revision
    /// only when it succeeds.
    ///
    /// # Errors
    ///
    /// [`AnimationEditError`]; nothing changes on error.
    pub fn edit_animation(
        &mut self,
        edit: impl FnOnce(&mut TitleClip) -> Result<(), ClipError<ElementId>>,
    ) -> Result<u64, AnimationEditError> {
        let revision = self
            .animation
            .revision
            .checked_add(1)
            .ok_or(AnimationEditError::RevisionExhausted)?;
        let mut clip = self.animation.clip.clone();
        edit(&mut clip)?;
        clip.validate()?;
        if let Some(track) = clip
            .tracks()
            .iter()
            .find(|track| !self.elements.iter().any(|e| e.id == track.key.target))
        {
            return Err(AnimationEditError::UnknownElement(track.key.target));
        }
        self.animation = TitleAnimation::new(clip, revision);
        Ok(revision)
    }
}

/// One element's sampled animation, every property defaulting to identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ElementAnimation {
    pub dx: i64,
    pub dy: i64,
    /// Basis points.
    pub scale_x: i64,
    pub scale_y: i64,
    /// Millidegrees, clockwise.
    pub rotation: i64,
    /// `0..=255`, multiplied into the static opacity.
    pub opacity: u8,
    /// Left, top, right, bottom, in pixels, each at least zero.
    pub crop: [i64; 4],
    pub color: Option<Rgba>,
}

impl ElementAnimation {
    pub(crate) const IDENTITY: Self = Self {
        dx: 0,
        dy: 0,
        scale_x: fm_animation::SCALE_IDENTITY_BP,
        scale_y: fm_animation::SCALE_IDENTITY_BP,
        rotation: 0,
        opacity: u8::MAX,
        crop: [0; 4],
        color: None,
    };

    /// Samples every property of `element`; [`ClipState::Static`] and
    /// untracked properties are identity.
    pub(crate) fn sample(clip: &TitleClip, element: ElementId, state: ClipState) -> Self {
        let mut sampled = Self::IDENTITY;
        if matches!(state, ClipState::Static) || clip.tracks().is_empty() {
            return sampled;
        }
        for (property, value) in clip.evaluate(element, state) {
            let scalar = match value {
                Value::Scalar(scalar) => scalar,
                Value::Color(color) => {
                    if property == Property::Color {
                        sampled.color = Some(color);
                    }
                    continue;
                }
            };
            match property {
                Property::PositionX => sampled.dx = scalar,
                Property::PositionY => sampled.dy = scalar,
                Property::ScaleX => sampled.scale_x = scalar,
                Property::ScaleY => sampled.scale_y = scalar,
                Property::Rotation => sampled.rotation = scalar,
                Property::Opacity => {
                    sampled.opacity = u8::try_from(scalar.clamp(0, 255)).unwrap_or(u8::MAX);
                }
                Property::CropLeft => sampled.crop[0] = scalar.max(0),
                Property::CropTop => sampled.crop[1] = scalar.max(0),
                Property::CropRight => sampled.crop[2] = scalar.max(0),
                Property::CropBottom => sampled.crop[3] = scalar.max(0),
                Property::Color => {}
            }
        }
        sampled
    }
}
