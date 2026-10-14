//! Built-in In/Out animation presets.
//!
//! A preset generates keyframes relative to the target's static values: an
//! In preset ends at the identity (offset `0`, scale `1.0`, opacity `255`,
//! crop `0`) so the hold state is exactly the static design, and an Out
//! preset starts there. Keys sit at frame `0` and frame `duration`; a zero
//! duration is a cut to the end state.

use crate::{
    clip::{Clip, ClipError},
    easing::Easing,
    property::{OPACITY_IDENTITY, Property, SCALE_IDENTITY_BP},
    track::{Keyframe, Phase, Track, TrackKey},
    value::Value,
};

/// A built-in entrance/exit animation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AnimationPreset {
    /// Opacity from transparent.
    Fade,
    /// Position from off the left canvas edge.
    SlideFromLeft,
    /// Position from off the right canvas edge.
    SlideFromRight,
    /// Position from off the top canvas edge.
    SlideFromTop,
    /// Position from off the bottom canvas edge.
    SlideFromBottom,
    /// Scale from zero with a Back overshoot.
    Pop,
    /// Animated right crop revealing left to right.
    Wipe,
    /// Falls in from above and bounces.
    Drop,
}

/// The target's static placement and the canvas, used to compute off-canvas
/// distances and crop extents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresetGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub canvas_width: u32,
    pub canvas_height: u32,
}

impl AnimationPreset {
    pub const ALL: [Self; 8] = [
        Self::Fade,
        Self::SlideFromLeft,
        Self::SlideFromRight,
        Self::SlideFromTop,
        Self::SlideFromBottom,
        Self::Pop,
        Self::Wipe,
        Self::Drop,
    ];

    /// Stable id for UI and wire use.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Fade => "fade",
            Self::SlideFromLeft => "slide-from-left",
            Self::SlideFromRight => "slide-from-right",
            Self::SlideFromTop => "slide-from-top",
            Self::SlideFromBottom => "slide-from-bottom",
            Self::Pop => "pop",
            Self::Wipe => "wipe",
            Self::Drop => "drop",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.id() == id)
    }

    /// The tracks this preset animates for `phase` over `duration` frames,
    /// as `(property, keys)`. In presets run from the effect value to the
    /// identity; Out presets run from the identity to the effect value
    /// (slides and drops exit the way they came in, except Drop, which falls
    /// out of the bottom).
    #[must_use]
    pub fn keys(
        self,
        phase: Phase,
        duration: u32,
        geometry: PresetGeometry,
    ) -> Vec<(Property, Vec<Keyframe>)> {
        let x = i64::from(geometry.x);
        let y = i64::from(geometry.y);
        let width = i64::from(geometry.width);
        let height = i64::from(geometry.height);
        let off_left = -(x + width);
        let off_right = i64::from(geometry.canvas_width) - x;
        let off_top = -(y + height);
        let off_bottom = i64::from(geometry.canvas_height) - y;
        let (property_values, in_easing, out_easing): (Vec<(Property, i64, i64)>, Easing, Easing) =
            match self {
                Self::Fade => (
                    vec![(Property::Opacity, 0, OPACITY_IDENTITY)],
                    Easing::Linear,
                    Easing::Linear,
                ),
                Self::SlideFromLeft => (
                    vec![(Property::PositionX, off_left, 0)],
                    Easing::CubicOut,
                    Easing::CubicIn,
                ),
                Self::SlideFromRight => (
                    vec![(Property::PositionX, off_right, 0)],
                    Easing::CubicOut,
                    Easing::CubicIn,
                ),
                Self::SlideFromTop => (
                    vec![(Property::PositionY, off_top, 0)],
                    Easing::CubicOut,
                    Easing::CubicIn,
                ),
                Self::SlideFromBottom => (
                    vec![(Property::PositionY, off_bottom, 0)],
                    Easing::CubicOut,
                    Easing::CubicIn,
                ),
                Self::Pop => (
                    vec![
                        (Property::ScaleX, 0, SCALE_IDENTITY_BP),
                        (Property::ScaleY, 0, SCALE_IDENTITY_BP),
                    ],
                    Easing::BackOut,
                    Easing::BackIn,
                ),
                Self::Wipe => (
                    vec![(Property::CropRight, width, 0)],
                    Easing::CubicInOut,
                    Easing::CubicInOut,
                ),
                Self::Drop => (
                    vec![(Property::PositionY, off_top, 0)],
                    Easing::BounceOut,
                    Easing::QuadIn,
                ),
            };
        property_values
            .into_iter()
            .map(|(property, effect, identity)| {
                let (from, to, easing) = match phase {
                    Phase::In => (effect, identity, in_easing),
                    Phase::Out => {
                        let exit = if matches!(self, Self::Drop) {
                            off_bottom
                        } else {
                            effect
                        };
                        (identity, exit, out_easing)
                    }
                };
                let clamp = |value: i64| {
                    property
                        .range()
                        .map_or(value, |(low, high)| value.clamp(low, high))
                };
                let end = Keyframe::new(duration, Value::Scalar(clamp(to)), Easing::Linear);
                let keys = if duration == 0 {
                    // A cut: only the end state.
                    vec![end]
                } else {
                    vec![Keyframe::new(0, Value::Scalar(clamp(from)), easing), end]
                };
                (property, keys)
            })
            .collect()
    }
}

impl<T: Copy + Ord> Clip<T> {
    /// Replaces `target`'s tracks for the preset's properties in `phase` with
    /// the preset over `duration` frames, atomically.
    ///
    /// # Errors
    ///
    /// [`ClipError::KeyOutsidePhase`] when `duration` exceeds the phase, or a
    /// limit violation.
    pub fn apply_preset(
        &mut self,
        target: T,
        phase: Phase,
        preset: AnimationPreset,
        duration: u32,
        geometry: PresetGeometry,
    ) -> Result<(), ClipError<T>> {
        self.try_edit(|clip| {
            for (property, keys) in preset.keys(phase, duration, geometry) {
                clip.put_track(Track::new(TrackKey::new(target, phase, property), keys));
            }
            Ok(())
        })
    }
}
