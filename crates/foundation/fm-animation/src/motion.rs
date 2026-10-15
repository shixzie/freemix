//! Saved motions: one phase of one target's animation, kept apart from any
//! clip so it can be applied to another target (user animation presets).
//!
//! Every animated property but [`Property::Color`] already layers onto the
//! target's static values (offsets for position, rotation, and crop; factors
//! for scale and opacity), so a phase's tracks are relative to the target's
//! resting pose and play on any target. Two things are made relative on
//! top of that:
//!
//! * Time: a motion is applied over any number of frames. Every key is
//!   retimed exactly like [`Clip::stretch_phase`]
//!   (`round_half_up(frame * frames / motion_frames)`), refusing a length
//!   that would make two keys of one track collide.
//! * Crop extents: crop keys are pixels of the captured target's box, so a
//!   motion remembers that box (`width` x `height`) and scales `crop_left`
//!   and `crop_right` keys by the new box width, `crop_top` and
//!   `crop_bottom` by its height (rounded once, half up). A wipe captured on
//!   a 400 px wide element wipes the whole of an 800 px wide one.
//!
//! Position offsets stay pixels (a 200 px slide is a 200 px slide on any
//! target); colour keys override the static colour and are kept as they
//! are.

use crate::{
    clip::{Clip, ClipError, MAX_PHASE_FRAMES},
    edit::round_half_up,
    property::Property,
    time::Timebase,
    track::{Keyframe, Phase, Track, TrackKey},
    value::Value,
};
use core::fmt;

/// Most keyframes one motion may hold across its tracks. With one track per
/// property this keeps every motion's wire encoding well inside a protocol
/// field.
pub const MAX_MOTION_KEYS: usize = 1_024;

/// The single target every track of a [`MotionClip`] animates.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MotionTarget;

/// The clip a [`PhaseMotion`] keeps: tracks of [`MotionTarget`] in
/// [`Phase::In`] only, an In phase as long as the motion, and an empty Out
/// phase.
pub type MotionClip = Clip<MotionTarget>;

/// Why a motion was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MotionError {
    /// The target has no track in the phase: there is nothing to save.
    Empty,
    /// The clip has an Out phase or a track outside its In phase.
    NotOnePhase,
    /// More than [`MAX_MOTION_KEYS`] keyframes.
    TooManyKeys { count: usize },
    /// The reference box has a zero side.
    ZeroReference,
    /// The clip violates an `fm_animation` bound.
    Clip(ClipError<MotionTarget>),
}

impl fmt::Display for MotionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("the phase animates nothing to save"),
            Self::NotOnePhase => formatter.write_str("a motion holds exactly one phase"),
            Self::TooManyKeys { count } => {
                write!(formatter, "{count} keys exceed {MAX_MOTION_KEYS}")
            }
            Self::ZeroReference => {
                formatter.write_str("a motion's reference box must not be empty")
            }
            Self::Clip(error) => write!(formatter, "invalid motion: {error}"),
        }
    }
}

impl std::error::Error for MotionError {}

impl From<ClipError<MotionTarget>> for MotionError {
    fn from(error: ClipError<MotionTarget>) -> Self {
        Self::Clip(error)
    }
}

/// One phase of one target's animation, relative to its resting pose (see
/// the module documentation).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhaseMotion {
    clip: MotionClip,
    width: u32,
    height: u32,
}

impl PhaseMotion {
    /// A motion from its clip and the `width` x `height` box its crop keys
    /// were authored against.
    ///
    /// # Errors
    ///
    /// [`MotionError`] when the clip is empty, has an Out phase or Out
    /// tracks, holds too many keys, or the box has a zero side.
    pub fn new(clip: MotionClip, width: u32, height: u32) -> Result<Self, MotionError> {
        clip.validate()?;
        if clip.out_frames() != 0
            || clip
                .tracks()
                .iter()
                .any(|track| track.key.phase != Phase::In)
        {
            return Err(MotionError::NotOnePhase);
        }
        if clip.tracks().is_empty() {
            return Err(MotionError::Empty);
        }
        let count = clip.key_count();
        if count > MAX_MOTION_KEYS {
            return Err(MotionError::TooManyKeys { count });
        }
        if width == 0 || height == 0 {
            return Err(MotionError::ZeroReference);
        }
        Ok(Self {
            clip,
            width,
            height,
        })
    }

    /// The tracks `target` has in `phase` of `clip`, over the phase's length
    /// in the clip's timebase, measured against a `width` x `height` box.
    ///
    /// # Errors
    ///
    /// [`MotionError::Empty`] when `target` has no track in `phase`, or any
    /// other bound [`Self::new`] checks.
    pub fn capture<T: Copy + Ord>(
        clip: &Clip<T>,
        target: T,
        phase: Phase,
        width: u32,
        height: u32,
    ) -> Result<Self, MotionError> {
        let tracks = clip
            .tracks()
            .iter()
            .filter(|track| track.key.target == target && track.key.phase == phase)
            .map(|track| {
                Track::new(
                    TrackKey::new(MotionTarget, Phase::In, track.key.property),
                    track.keys.clone(),
                )
            })
            .collect::<Vec<_>>();
        if tracks.is_empty() {
            return Err(MotionError::Empty);
        }
        let clip = Clip::from_tracks(clip.timebase(), clip.phase_frames(phase), 0, tracks)?;
        Self::new(clip, width, height)
    }

    /// The motion's clip: its tracks in [`Phase::In`].
    #[must_use]
    pub const fn clip(&self) -> &MotionClip {
        &self.clip
    }

    #[must_use]
    pub const fn timebase(&self) -> Timebase {
        self.clip.timebase()
    }

    /// The motion's length in frames of its timebase.
    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.clip.in_frames()
    }

    /// Width of the box the crop keys were authored against.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height of the box the crop keys were authored against.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Every animated property, in property order.
    pub fn properties(&self) -> impl Iterator<Item = Property> + '_ {
        self.clip.tracks().iter().map(|track| track.key.property)
    }

    /// The motion's length in frames of `timebase`, the same duration
    /// rounded once, half up, and capped at [`MAX_PHASE_FRAMES`].
    #[must_use]
    pub fn frames_at(&self, timebase: Timebase) -> u32 {
        let own = self.timebase().rate();
        let target = timebase.rate();
        // frames * (tn / td) / (on / od)
        let numerator = i128::from(self.frames())
            * i128::from(target.numerator())
            * i128::from(own.denominator());
        let denominator = i128::from(target.denominator()) * i128::from(own.numerator());
        let frames = round_half_up(numerator, denominator.max(1));
        u32::try_from(frames.clamp(0, i128::from(MAX_PHASE_FRAMES))).unwrap_or(MAX_PHASE_FRAMES)
    }

    /// The tracks this motion puts on a `width` x `height` target over
    /// `frames` frames: keys retimed like [`Clip::stretch_phase`] and crop
    /// keys scaled to the box, as `(property, keys)` in property order.
    ///
    /// # Errors
    ///
    /// [`ClipError::RetimeCollision`] (with the shortest length that fits)
    /// when two keys of a track would share a frame, or
    /// [`ClipError::PhaseTooLong`].
    pub fn fitted(
        &self,
        frames: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<(Property, Vec<Keyframe>)>, ClipError<MotionTarget>> {
        let mut clip = self.clip.clone();
        if frames != clip.in_frames() {
            if clip.in_frames() == 0 {
                // A cut has nothing to stretch: it stays at frame zero.
                clip.set_phase_frames_keeping_keys(Phase::In, frames)?;
            } else {
                clip.stretch_phase(Phase::In, frames)?;
            }
        }
        let scale = |value: i64, to: u32, from: u32| -> i64 {
            let scaled = round_half_up(i128::from(value) * i128::from(to), i128::from(from));
            i64::try_from(scaled.clamp(0, i128::from(i32::MAX))).unwrap_or(i64::from(i32::MAX))
        };
        Ok(clip
            .tracks()
            .iter()
            .map(|track| {
                let property = track.key.property;
                let extent = match property {
                    Property::CropLeft | Property::CropRight => Some((width, self.width)),
                    Property::CropTop | Property::CropBottom => Some((height, self.height)),
                    _ => None,
                };
                let keys = track
                    .keys
                    .iter()
                    .map(|key| match (extent, key.value) {
                        (Some((to, from)), Value::Scalar(value)) => Keyframe::new(
                            key.frame,
                            Value::Scalar(scale(value, to, from)),
                            key.easing,
                        ),
                        _ => *key,
                    })
                    .collect();
                (property, keys)
            })
            .collect())
    }
}

/// `error` about a motion track as the same error about `target`'s track in
/// `phase`.
fn retarget<T: Copy>(error: &ClipError<MotionTarget>, target: T, phase: Phase) -> ClipError<T> {
    let track = |key: TrackKey<MotionTarget>| TrackKey::new(target, phase, key.property);
    match *error {
        ClipError::PhaseTooLong { frames, .. } => ClipError::PhaseTooLong { phase, frames },
        ClipError::TooManyTracks { count } => ClipError::TooManyTracks { count },
        ClipError::TooManyTrackKeys { track: key, count } => ClipError::TooManyTrackKeys {
            track: track(key),
            count,
        },
        ClipError::TooManyKeys { count } => ClipError::TooManyKeys { count },
        ClipError::DuplicateTrack(key) => ClipError::DuplicateTrack(track(key)),
        ClipError::EmptyTrack(key) => ClipError::EmptyTrack(track(key)),
        ClipError::NonIncreasingKeys { track: key, frame } => ClipError::NonIncreasingKeys {
            track: track(key),
            frame,
        },
        ClipError::KeyOutsidePhase { track: key, frame } => ClipError::KeyOutsidePhase {
            track: track(key),
            frame,
        },
        ClipError::InvalidValue { track: key, frame } => ClipError::InvalidValue {
            track: track(key),
            frame,
        },
        ClipError::RetimeCollision {
            track: key,
            min_len,
        } => ClipError::RetimeCollision {
            track: track(key),
            min_len,
        },
        ClipError::KeyCollision { track: key, frame } => ClipError::KeyCollision {
            track: track(key),
            frame,
        },
        ClipError::ZeroSpan => ClipError::ZeroSpan,
        ClipError::UnknownKey { track: key, frame } => ClipError::UnknownKey {
            track: track(key),
            frame,
        },
    }
}

impl<T: Copy + Ord> Clip<T> {
    /// Puts `motion` on `target` over `frames` frames of `phase`, sized for a
    /// `width` x `height` box (see [`PhaseMotion::fitted`]), atomically:
    /// the phase grows to `frames` when it is shorter (keys already in it
    /// stay where they are), and `target`'s track of every property the
    /// motion animates and `animates` accepts is replaced. Returns how many
    /// tracks were put.
    ///
    /// # Errors
    ///
    /// A retime collision or any limit violation; nothing changes on error.
    #[allow(clippy::too_many_arguments)] // Target, phase, motion, length, and box.
    pub fn apply_motion(
        &mut self,
        target: T,
        phase: Phase,
        motion: &PhaseMotion,
        frames: u32,
        width: u32,
        height: u32,
        animates: impl Fn(Property) -> bool,
    ) -> Result<usize, ClipError<T>> {
        let tracks = motion
            .fitted(frames, width, height)
            .map_err(|error| retarget(&error, target, phase))?;
        let mut put = 0;
        self.try_edit(|clip| {
            let length = clip.phase_frames(phase).max(frames);
            clip.set_phase_frames(phase, length);
            for (property, keys) in tracks {
                if animates(property) {
                    clip.put_track(Track::new(TrackKey::new(target, phase, property), keys));
                    put += 1;
                }
            }
            Ok(())
        })?;
        Ok(put)
    }
}
