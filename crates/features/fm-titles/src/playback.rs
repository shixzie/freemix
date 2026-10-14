//! Frame-indexed take/off-air playback of a design's transition timelines.
//!
//! [`TitlePlayback`] is driven only by engine frame indices supplied by the
//! caller; it reads no clock. Frames must never go backwards.
//!
//! | state            | `take(f)`                   | `off_air(f)`              |
//! |------------------|-----------------------------|---------------------------|
//! | off              | start the in timeline at f  | ignored                   |
//! | transition in    | ignored                     | start the out timeline    |
//! | hold             | ignored                     | start the out timeline    |
//! | transition out   | restart the in timeline     | ignored                   |
//!
//! A transition of `n` frames shows local frames `0..n` on the `n` engine
//! frames starting at its start frame; the next frame is Hold (after In) or
//! Off (after Out). A zero-length transition therefore completes on the frame
//! that started it. Hold samples the In timeline at its final frame.

use crate::{Composition, TimelineSample, TitleDesign, TransitionKind};
use core::fmt;

/// What is on screen at one frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackPhase {
    Off,
    TransitionIn { frame: u32 },
    Hold,
    TransitionOut { frame: u32 },
}

/// What a command did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackOutcome {
    Started(TransitionKind),
    Ignored,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackError {
    /// A frame earlier than the last command frame.
    FrameRegressed { frame: u64, last_frame: u64 },
}

impl fmt::Display for PlaybackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameRegressed { frame, last_frame } => write!(
                formatter,
                "frame {frame} precedes the last playback command at frame {last_frame}"
            ),
        }
    }
}

impl std::error::Error for PlaybackError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Off,
    In { start: u64 },
    Out { start: u64 },
}

/// The playback state machine for one title.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitlePlayback {
    in_frames: u32,
    out_frames: u32,
    state: State,
    last_frame: u64,
}

impl TitlePlayback {
    /// Starts off air with explicit transition lengths.
    #[must_use]
    pub const fn new(in_frames: u32, out_frames: u32) -> Self {
        Self {
            in_frames,
            out_frames,
            state: State::Off,
            last_frame: 0,
        }
    }

    /// Starts off air with the design's current timeline lengths. Rebuild
    /// after editing a timeline duration.
    #[must_use]
    pub fn for_design(design: &TitleDesign) -> Self {
        Self::new(
            design.timeline(TransitionKind::In).duration_frames,
            design.timeline(TransitionKind::Out).duration_frames,
        )
    }

    #[must_use]
    pub const fn last_frame(&self) -> u64 {
        self.last_frame
    }

    fn check(&self, frame: u64) -> Result<(), PlaybackError> {
        if frame < self.last_frame {
            return Err(PlaybackError::FrameRegressed {
                frame,
                last_frame: self.last_frame,
            });
        }
        Ok(())
    }

    /// The phase at `frame`, without changing state.
    ///
    /// # Errors
    ///
    /// Returns [`PlaybackError::FrameRegressed`] for a frame before the last
    /// command.
    pub fn phase(&self, frame: u64) -> Result<PlaybackPhase, PlaybackError> {
        self.check(frame)?;
        Ok(match self.state {
            State::Off => PlaybackPhase::Off,
            State::In { start } => match local(frame, start, self.in_frames) {
                Some(frame) => PlaybackPhase::TransitionIn { frame },
                None => PlaybackPhase::Hold,
            },
            State::Out { start } => match local(frame, start, self.out_frames) {
                Some(frame) => PlaybackPhase::TransitionOut { frame },
                None => PlaybackPhase::Off,
            },
        })
    }

    /// Takes the title to air at `frame`.
    ///
    /// # Errors
    ///
    /// Returns [`PlaybackError::FrameRegressed`] without changing state.
    pub fn take(&mut self, frame: u64) -> Result<PlaybackOutcome, PlaybackError> {
        let phase = self.phase(frame)?;
        self.last_frame = frame;
        Ok(match phase {
            PlaybackPhase::Off | PlaybackPhase::TransitionOut { .. } => {
                self.state = State::In { start: frame };
                PlaybackOutcome::Started(TransitionKind::In)
            }
            PlaybackPhase::TransitionIn { .. } | PlaybackPhase::Hold => PlaybackOutcome::Ignored,
        })
    }

    /// Takes the title off air at `frame`.
    ///
    /// # Errors
    ///
    /// Returns [`PlaybackError::FrameRegressed`] without changing state.
    pub fn off_air(&mut self, frame: u64) -> Result<PlaybackOutcome, PlaybackError> {
        let phase = self.phase(frame)?;
        self.last_frame = frame;
        Ok(match phase {
            PlaybackPhase::TransitionIn { .. } | PlaybackPhase::Hold => {
                self.state = State::Out { start: frame };
                PlaybackOutcome::Started(TransitionKind::Out)
            }
            PlaybackPhase::Off | PlaybackPhase::TransitionOut { .. } => PlaybackOutcome::Ignored,
        })
    }

    /// The timeline sample for `frame`, or `None` while off air.
    ///
    /// # Errors
    ///
    /// Returns [`PlaybackError::FrameRegressed`].
    pub fn sample(&self, frame: u64) -> Result<Option<TimelineSample>, PlaybackError> {
        Ok(match self.phase(frame)? {
            PlaybackPhase::Off => None,
            PlaybackPhase::TransitionIn { frame } => Some(TimelineSample::At {
                transition: TransitionKind::In,
                frame,
            }),
            PlaybackPhase::Hold => Some(TimelineSample::At {
                transition: TransitionKind::In,
                frame: self.in_frames,
            }),
            PlaybackPhase::TransitionOut { frame } => Some(TimelineSample::At {
                transition: TransitionKind::Out,
                frame,
            }),
        })
    }

    /// The design composed for `frame`; empty while off air.
    ///
    /// # Errors
    ///
    /// Returns [`PlaybackError::FrameRegressed`].
    pub fn compose(&self, design: &TitleDesign, frame: u64) -> Result<Composition, PlaybackError> {
        Ok(self
            .sample(frame)?
            .map_or_else(Composition::empty, |sample| design.compose(sample)))
    }
}

fn local(frame: u64, start: u64, length: u32) -> Option<u32> {
    let elapsed = frame - start;
    if elapsed < u64::from(length) {
        u32::try_from(elapsed).ok()
    } else {
        None
    }
}
