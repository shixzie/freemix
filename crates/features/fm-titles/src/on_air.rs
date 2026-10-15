//! A title's In/Hold/Out animation driven on air by output frame indices.
//!
//! [`TitleAir`] is the on-air counterpart of [`crate::TitlePlayback`]: it
//! plays a template's [`TitleAnimation`] phases against the frames of an
//! output running at its own rate, which need not be the clip's
//! [`Timebase`]. It reads no clock; callers supply engine frame indices.
//!
//! * A phase of `n` clip frames lasts
//!   `D = Timebase::project_frames(n, rate)` output frames. Output frame
//!   `start + k` (`k < D`) samples the phase at the exact rational clip
//!   position `k * clip_rate / rate` ([`Timebase::position_at`]); no frame
//!   is rounded, so a 25p clip on a 29.97 output keeps its keyframe timing
//!   exactly.
//! * In: frames `start..start + D_in` show the In phase; from `start + D_in`
//!   the title holds ([`AnimSample::Hold`], the In phase at its last frame).
//!   A zero-length In holds from the take frame.
//! * Out: frames `start..start + D_out` show the Out phase; from
//!   `start + D_out` the title is gone ([`TitleAir::at`] is `None`). A
//!   zero-length Out is gone on the off-air frame.
//! * Taking off air while the In phase still plays abandons it: the Out
//!   phase starts at its first frame on the off-air frame.
//! * Re-taking while the Out phase plays restarts the In phase at its first
//!   frame on the re-take frame (the same rule as [`crate::TitlePlayback`]);
//!   the pose jumps to the In phase's first key rather than blending from the
//!   Out pose shown on the previous frame.
//! * The pose is a pure function of `frame - start`, so a sampler that skips
//!   frames (a slow renderer, a late joiner) lands on exactly the pose of the
//!   frame it renders.

use crate::{AnimSample, TitleAnimation};
use fm_animation::{ClipState, Phase, Timebase};
use fm_types::FrameRate;

/// A title animation's phase lengths in its own timebase: everything on-air
/// playback needs from a template.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TitleAirTiming {
    pub timebase: Timebase,
    /// In phase length, in clip frames.
    pub in_frames: u32,
    /// Out phase length, in clip frames.
    pub out_frames: u32,
}

impl Default for TitleAirTiming {
    /// No animation: zero-length phases at [`crate::default_title_timebase`].
    fn default() -> Self {
        Self {
            timebase: crate::default_title_timebase(),
            in_frames: 0,
            out_frames: 0,
        }
    }
}

impl TitleAirTiming {
    /// The phase lengths of `animation`.
    #[must_use]
    pub const fn of(animation: &TitleAnimation) -> Self {
        Self {
            timebase: animation.clip.timebase(),
            in_frames: animation.clip.in_frames(),
            out_frames: animation.clip.out_frames(),
        }
    }

    /// The length of `phase` in clip frames.
    #[must_use]
    pub const fn clip_frames(self, phase: Phase) -> u32 {
        match phase {
            Phase::In => self.in_frames,
            Phase::Out => self.out_frames,
        }
    }

    /// The length of `phase` in frames of an output running at `rate`.
    #[must_use]
    pub fn output_frames(self, phase: Phase, rate: FrameRate) -> u64 {
        self.timebase.project_frames(self.clip_frames(phase), rate)
    }
}

/// Where an on-air title is in its life.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleAirPhase {
    /// Playing the In phase.
    In,
    /// Holding the In phase's last frame.
    Hold,
    /// Playing the Out phase.
    Out,
}

/// One output frame of an on-air title.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TitleAirFrame {
    pub phase: TitleAirPhase,
    /// Output frames since the phase began: `0..phase_frames` while In or
    /// Out plays, zero while holding.
    pub phase_frame: u64,
    /// The phase length in output frames; zero while holding.
    pub phase_frames: u64,
    /// The pose to render.
    pub sample: AnimSample,
}

/// An on-air title's animation, started by a take and ended by an off-air.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TitleAir {
    timing: TitleAirTiming,
    rate: FrameRate,
    phase: Phase,
    start: u64,
}

impl TitleAir {
    /// A title taken to air on output frame `frame` of an output at `rate`:
    /// the In phase starts there.
    #[must_use]
    pub const fn take(timing: TitleAirTiming, rate: FrameRate, frame: u64) -> Self {
        Self {
            timing,
            rate,
            phase: Phase::In,
            start: frame,
        }
    }

    /// Takes this title off air on `frame`: the Out phase starts there,
    /// abandoning an In phase still playing. Already playing Out, nothing
    /// changes.
    #[must_use]
    pub const fn off_air(self, frame: u64) -> Self {
        match self.phase {
            Phase::In => Self {
                phase: Phase::Out,
                start: frame,
                ..self
            },
            Phase::Out => self,
        }
    }

    /// Takes this title to air again on `frame`: while the Out phase plays,
    /// the In phase restarts at its first frame; otherwise nothing changes.
    #[must_use]
    pub const fn retake(self, frame: u64) -> Self {
        match self.phase {
            Phase::Out => Self::take(self.timing, self.rate, frame),
            Phase::In => self,
        }
    }

    #[must_use]
    pub const fn timing(&self) -> TitleAirTiming {
        self.timing
    }

    /// The output frame the current phase started on.
    #[must_use]
    pub const fn start_frame(&self) -> u64 {
        self.start
    }

    /// Whether the title has been taken off air (it may still be playing
    /// Out).
    #[must_use]
    pub const fn is_leaving(&self) -> bool {
        matches!(self.phase, Phase::Out)
    }

    /// The length of the current phase in output frames.
    #[must_use]
    pub fn phase_frames(&self) -> u64 {
        self.timing.output_frames(self.phase, self.rate)
    }

    /// The title at output frame `frame`, or `None` once its Out phase has
    /// finished. Frames before the phase start sample as its first frame.
    #[must_use]
    pub fn at(&self, frame: u64) -> Option<TitleAirFrame> {
        let elapsed = frame.saturating_sub(self.start);
        let length = self.phase_frames();
        if elapsed >= length {
            return match self.phase {
                Phase::In => Some(TitleAirFrame {
                    phase: TitleAirPhase::Hold,
                    phase_frame: 0,
                    phase_frames: 0,
                    sample: ClipState::Hold,
                }),
                Phase::Out => None,
            };
        }
        let position = self.timing.timebase.position_at(elapsed, self.rate);
        let (phase, sample) = match self.phase {
            Phase::In => (TitleAirPhase::In, ClipState::In(position)),
            Phase::Out => (TitleAirPhase::Out, ClipState::Out(position)),
        };
        Some(TitleAirFrame {
            phase,
            phase_frame: elapsed,
            phase_frames: length,
            sample,
        })
    }

    /// The first output frame at or after `frame` on which the title holds
    /// or is gone: the end of the phase playing at `frame`.
    #[must_use]
    pub fn settles_at(&self, frame: u64) -> u64 {
        self.start.saturating_add(self.phase_frames()).max(frame)
    }
}
