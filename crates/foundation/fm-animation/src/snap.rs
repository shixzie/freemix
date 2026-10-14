//! Snapping a dragged frame to nearby timeline landmarks.

use crate::{clip::MAX_PHASE_FRAMES, time::Timebase};

/// What a frame snapped to. Earlier variants win ties at equal distance and
/// equal frame.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SnapKind {
    Playhead,
    Key,
    PhaseBound,
    WholeSecond,
}

/// A snap result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Snap {
    pub frame: u32,
    pub kind: SnapKind,
}

/// Candidate frames a drag may snap to.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SnapTargets {
    candidates: Vec<Snap>,
}

impl SnapTargets {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            candidates: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_playhead(mut self, frame: u32) -> Self {
        self.push(frame, SnapKind::Playhead);
        self
    }

    /// Other keys' frames; callers exclude the keys being dragged.
    #[must_use]
    pub fn with_keys(mut self, frames: impl IntoIterator<Item = u32>) -> Self {
        for frame in frames {
            self.push(frame, SnapKind::Key);
        }
        self
    }

    /// Frame `0` and the phase end.
    #[must_use]
    pub fn with_phase_bounds(mut self, phase_frames: u32) -> Self {
        self.push(0, SnapKind::PhaseBound);
        self.push(phase_frames, SnapKind::PhaseBound);
        self
    }

    /// The first frame at or after each whole second inside the phase
    /// (`ceil(seconds * rate)`), found in one pass over the phase frames
    /// (at most [`MAX_PHASE_FRAMES`]).
    #[must_use]
    pub fn with_whole_seconds(mut self, timebase: Timebase, phase_frames: u32) -> Self {
        let rate = timebase.rate();
        // Whole seconds elapsed by the end of frame `f`: floor(f * d / n).
        let seconds_by = |frame: u32| {
            u64::from(frame) * u64::from(rate.denominator()) / u64::from(rate.numerator())
        };
        for frame in 0..=phase_frames.min(MAX_PHASE_FRAMES) {
            if frame == 0 || seconds_by(frame) > seconds_by(frame - 1) {
                self.push(frame, SnapKind::WholeSecond);
            }
        }
        self
    }

    pub fn push(&mut self, frame: u32, kind: SnapKind) {
        self.candidates.push(Snap { frame, kind });
    }

    /// The nearest candidate within `threshold` frames of `frame` (inclusive);
    /// ties go to the earlier frame, then the earlier [`SnapKind`].
    #[must_use]
    pub fn snap(&self, frame: u32, threshold: u32) -> Option<Snap> {
        self.candidates
            .iter()
            .copied()
            .filter(|candidate| candidate.frame.abs_diff(frame) <= threshold)
            .min_by_key(|candidate| {
                (
                    candidate.frame.abs_diff(frame),
                    candidate.frame,
                    candidate.kind,
                )
            })
    }

    /// [`Self::snap`]'s frame, or `frame` unchanged when nothing is in reach.
    #[must_use]
    pub fn snap_frame(&self, frame: u32, threshold: u32) -> u32 {
        self.snap(frame, threshold).map_or(frame, |snap| snap.frame)
    }
}
