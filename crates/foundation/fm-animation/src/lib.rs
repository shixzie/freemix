//! Pure, exact keyframe animation: easing curves, timebases, tracks, clips,
//! editing, snapping, and built-in presets.
//!
//! Everything is integer arithmetic: easing in Q16 fixed point, cross-rate
//! positions as exact `u128` fractions, and frame retiming rounded once, half
//! up. Results are bit-identical on every host.
//!
//! * [`easing`]: the [`Easing`] curve library and [`EasingPreset`] ids.
//! * [`Timebase`] and [`ClipPosition`]: a clip authored at 25p samples
//!   exactly on a 50p project.
//! * [`Property`] and [`Value`]: what animates and how it layers onto static
//!   values (offset, factor, or override).
//! * [`Keyframe`], [`Track`], and [`Clip`]: keyed by target, [`Phase`], and
//!   property, with bounded size ([`MAX_CLIP_TRACKS`], [`MAX_TRACK_KEYS`],
//!   [`MAX_CLIP_KEYS`], [`MAX_PHASE_FRAMES`]).
//! * [`AnimationEdit`]: atomic stretch, retime, move, easing, and copy/paste.
//! * [`SnapTargets`]: nearest landmark within a threshold.
//! * [`AnimationPreset`]: Fade, Slide, Pop, Wipe, and Drop.
//! * [`sin_cos_q30`]: the fixed-point rotation every renderer shares.

mod clip;
pub mod easing;
mod edit;
mod preset;
mod property;
mod snap;
mod time;
mod track;
mod trig;
mod value;

pub use clip::{
    Clip, ClipError, ClipState, MAX_CLIP_KEYS, MAX_CLIP_TRACKS, MAX_PHASE_FRAMES, MAX_TRACK_KEYS,
};
pub use easing::{
    CubicBezier, EASE_ONE, Easing, EasingError, EasingPreset, MAX_BEZIER_Y, MIN_BEZIER_Y,
    ease_between, progress_q16,
};
pub use edit::{
    AnimationEdit, ClipboardKey, KeyClipboard, KeyRef, KeySelection, retime_frame, round_half_up,
    stretch_frame,
};
pub use preset::{AnimationPreset, PresetGeometry};
pub use property::{Layering, MAX_SCALE_BP, OPACITY_IDENTITY, Property, SCALE_IDENTITY_BP};
pub use snap::{Snap, SnapKind, SnapTargets};
pub use time::{ClipPosition, Timebase};
pub use track::{Keyframe, Phase, Track, TrackKey, sample, sample_frame};
pub use trig::{FULL_TURN_MILLIDEGREES, TRIG_ONE, sin_cos_q30};
pub use value::{Animatable, Rgba, Value};
