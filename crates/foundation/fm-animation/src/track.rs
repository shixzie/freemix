//! Keyframes, tracks, and exact sampling.

use crate::{
    easing::{EASE_ONE, Easing},
    property::Property,
    time::ClipPosition,
    value::{Animatable, Value},
};

/// A keyframe; `easing` shapes the interval towards the next keyframe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Keyframe<V = Value> {
    pub frame: u32,
    pub value: V,
    pub easing: Easing,
}

impl<V> Keyframe<V> {
    #[must_use]
    pub const fn new(frame: u32, value: V, easing: Easing) -> Self {
        Self {
            frame,
            value,
            easing,
        }
    }
}

/// Samples `keys` (strictly increasing frames) at a whole frame.
///
/// Holds the first value at or before the first keyframe and the last after
/// the last; between keyframes `k` and `k + 1` it is
/// `V::ease(k.value, next.value, k.easing(progress))` with
/// `progress = floor((frame - k.frame) * EASE_ONE / (next.frame - k.frame))`.
#[must_use]
pub fn sample_frame<V: Animatable>(keys: &[Keyframe<V>], frame: u32) -> Option<V> {
    sample(keys, ClipPosition::frame(frame))
}

/// Samples `keys` at an exact rational position. At a whole-frame position
/// this is [`sample_frame`] exactly.
#[must_use]
pub fn sample<V: Animatable>(keys: &[Keyframe<V>], at: ClipPosition) -> Option<V> {
    let first = keys.first()?;
    if at.at_or_before(first.frame) {
        return Some(first.value);
    }
    for pair in keys.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        if at.before(to.frame) {
            let progress = progress_between(at, from.frame, to.frame);
            return Some(V::ease(from.value, to.value, from.easing.apply(progress)));
        }
    }
    keys.last().map(|keyframe| keyframe.value)
}

/// Q16 `floor((at - from) * EASE_ONE / (to - from))`, `from <= at < to`;
/// equals `progress_q16(at - from, to - from)` for whole frames.
fn progress_between(at: ClipPosition, from: u32, to: u32) -> u32 {
    let denominator = at.denominator();
    let elapsed = at
        .numerator()
        .saturating_sub(u128::from(from) * denominator);
    let duration = u128::from(to.saturating_sub(from)) * denominator;
    if duration == 0 || elapsed >= duration {
        return EASE_ONE;
    }
    u32::try_from(elapsed * u128::from(EASE_ONE) / duration).unwrap_or(EASE_ONE)
}

/// Which half of a clip a track animates.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Phase {
    /// Plays on take; its end state is held while on air.
    In,
    /// Plays on off-air.
    Out,
}

/// Identifies a track: one property of one target in one phase.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrackKey<T> {
    pub target: T,
    pub phase: Phase,
    pub property: Property,
}

impl<T> TrackKey<T> {
    #[must_use]
    pub const fn new(target: T, phase: Phase, property: Property) -> Self {
        Self {
            target,
            phase,
            property,
        }
    }
}

/// One animated property of one target in one phase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Track<T> {
    pub key: TrackKey<T>,
    /// Strictly increasing frames, all inside the phase.
    pub keys: Vec<Keyframe>,
}

impl<T> Track<T> {
    #[must_use]
    pub const fn new(key: TrackKey<T>, keys: Vec<Keyframe>) -> Self {
        Self { key, keys }
    }

    /// The raw sample at `at`, before clamping to the property range.
    #[must_use]
    pub fn sample(&self, at: ClipPosition) -> Option<Value> {
        sample(&self.keys, at)
    }

    /// The index of the keyframe at exactly `frame`.
    #[must_use]
    pub fn key_index(&self, frame: u32) -> Option<usize> {
        self.keys
            .binary_search_by_key(&frame, |keyframe| keyframe.frame)
            .ok()
    }
}
