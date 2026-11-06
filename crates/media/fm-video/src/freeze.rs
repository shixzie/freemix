//! Deterministic per-input freeze/live-pause reference controller.
//!
//! Timestamps use `fm-frame`'s normalized nanosecond types
//! ([`NormalizedTimestamp`] and [`NormalizedDuration`]), so the controller
//! shares one time base with the media pipeline. The controller holds at most two source
//! frames (the latched freeze frame and the newest live arrival), so memory is
//! bounded regardless of how long an input stays frozen.
//!
//! # Output timestamp rule
//!
//! The output grid origin is the PTS of the first accepted source frame. A
//! source PTS `p` maps onto the output cadence as
//! `map(p) = origin + ceil((p - origin) / d) * d`, where `d` is the output
//! frame duration. Every emitted output frame then receives
//!
//! * live (including the first frame after resume):
//!   `pts = max(map(source_pts), last_output_pts + d)`;
//! * first presentation of the latched freeze frame: the live rule;
//! * frozen repeat: `pts = last_output_pts + d`;
//! * very first output: `pts = map(source_pts)`.
//!
//! Output PTS is therefore strictly increasing by at least `d` per emitted
//! frame and never regresses across freeze or resume.

use core::fmt;

use fm_frame::{NormalizedDuration, NormalizedTimestamp};

/// One source arrival: frame index, presentation timestamp, payload handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFrame<H> {
    pub index: u64,
    pub pts: NormalizedTimestamp,
    pub payload: H,
}

impl<H> SourceFrame<H> {
    #[must_use]
    pub const fn new(index: u64, pts: NormalizedTimestamp, payload: H) -> Self {
        Self {
            index,
            pts,
            payload,
        }
    }
}

/// Why a given output frame was emitted.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OutputOrigin {
    /// A live source frame passed through.
    Live,
    /// The latched freeze frame (first presentation or a repeat).
    Frozen,
}

/// One output-cadence frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputFrame<H> {
    pub pts: NormalizedTimestamp,
    pub source_index: u64,
    pub source_pts: NormalizedTimestamp,
    pub payload: H,
    pub origin: OutputOrigin,
}

/// Observable controller state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FreezeState {
    Live,
    /// Freeze requested; the first source frame with `index >= at_index`
    /// will be latched.
    FreezePending {
        at_index: u64,
    },
    /// Frozen on the latched source frame.
    Frozen {
        latched_index: u64,
    },
}

/// Explicit freeze controller failures. Failed calls leave state unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FreezeError {
    FrameDurationTooLong {
        nanos: u64,
    },
    SourceIndexNotIncreasing {
        previous: u64,
        actual: u64,
    },
    SourcePtsNotIncreasing {
        previous: NormalizedTimestamp,
        actual: NormalizedTimestamp,
    },
    /// The requested boundary is at or before a frame already received.
    BoundaryAlreadyPassed {
        requested: u64,
        last_received: u64,
    },
    AlreadyFrozen {
        latched_index: u64,
    },
    FreezeAlreadyPending {
        at_index: u64,
    },
    NoPendingFreeze,
    NotFrozen,
    TimestampOverflow,
}

impl fmt::Display for FreezeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameDurationTooLong { nanos } => write!(
                formatter,
                "output frame duration {nanos} ns exceeds the controller's range"
            ),
            Self::SourceIndexNotIncreasing { previous, actual } => write!(
                formatter,
                "source frame index {actual} does not follow {previous}"
            ),
            Self::SourcePtsNotIncreasing { previous, actual } => write!(
                formatter,
                "source PTS {} does not increase past {}",
                actual.as_nanos(),
                previous.as_nanos()
            ),
            Self::BoundaryAlreadyPassed {
                requested,
                last_received,
            } => write!(
                formatter,
                "freeze boundary {requested} is not after received frame {last_received}"
            ),
            Self::AlreadyFrozen { latched_index } => {
                write!(
                    formatter,
                    "input is already frozen on frame {latched_index}"
                )
            }
            Self::FreezeAlreadyPending { at_index } => {
                write!(formatter, "a freeze at frame {at_index} is already pending")
            }
            Self::NoPendingFreeze => formatter.write_str("no freeze is pending"),
            Self::NotFrozen => formatter.write_str("input is not frozen"),
            Self::TimestampOverflow => formatter.write_str("output timestamp overflow"),
        }
    }
}

impl std::error::Error for FreezeError {}

/// Per-input freeze/live-pause controller.
///
/// Drive it with [`push_source`](Self::push_source) for every source arrival
/// and [`emit`](Self::emit) once per output cadence slot.
#[derive(Clone, Debug)]
pub struct FreezeController<H> {
    frame_duration: NormalizedDuration,
    frame_duration_nanos: i64,
    state: FreezeState,
    origin: Option<i64>,
    last_source: Option<(u64, i64)>,
    last_output_pts: Option<i64>,
    latched: Option<SourceFrame<H>>,
    latched_presented: bool,
    newest_live: Option<SourceFrame<H>>,
    dropped_source_frames: u64,
}

impl<H: Clone> FreezeController<H> {
    /// Creates a live controller emitting on a cadence of `frame_duration`.
    ///
    /// # Errors
    ///
    /// Returns [`FreezeError::FrameDurationTooLong`] when the duration does
    /// not fit the controller's signed nanosecond range.
    pub fn new(frame_duration: NormalizedDuration) -> Result<Self, FreezeError> {
        let Ok(duration_nanos) = i64::try_from(frame_duration.as_nanos()) else {
            return Err(FreezeError::FrameDurationTooLong {
                nanos: frame_duration.as_nanos(),
            });
        };
        Ok(Self {
            frame_duration,
            frame_duration_nanos: duration_nanos,
            state: FreezeState::Live,
            origin: None,
            last_source: None,
            last_output_pts: None,
            latched: None,
            latched_presented: false,
            newest_live: None,
            dropped_source_frames: 0,
        })
    }

    #[must_use]
    pub const fn state(&self) -> FreezeState {
        self.state
    }

    #[must_use]
    pub const fn frame_duration(&self) -> NormalizedDuration {
        self.frame_duration
    }

    #[must_use]
    pub fn last_output_pts(&self) -> Option<NormalizedTimestamp> {
        self.last_output_pts.map(NormalizedTimestamp::from_nanos)
    }

    /// Source frames replaced before they were emitted (including every
    /// arrival superseded while frozen).
    #[must_use]
    pub const fn dropped_source_frames(&self) -> u64 {
        self.dropped_source_frames
    }

    /// Requests a freeze at the exact source frame boundary `at_index`. The
    /// first arriving frame with `index >= at_index` is latched.
    ///
    /// # Errors
    ///
    /// Fails when already frozen or pending, or when `at_index` is not after
    /// the last received source frame.
    pub fn request_freeze(&mut self, at_index: u64) -> Result<(), FreezeError> {
        match self.state {
            FreezeState::Frozen { latched_index } => {
                return Err(FreezeError::AlreadyFrozen { latched_index });
            }
            FreezeState::FreezePending { at_index } => {
                return Err(FreezeError::FreezeAlreadyPending { at_index });
            }
            FreezeState::Live => {}
        }
        if let Some((last_received, _)) = self.last_source
            && at_index <= last_received
        {
            return Err(FreezeError::BoundaryAlreadyPassed {
                requested: at_index,
                last_received,
            });
        }
        self.state = FreezeState::FreezePending { at_index };
        Ok(())
    }

    /// Cancels a pending (not yet latched) freeze.
    ///
    /// # Errors
    ///
    /// Returns [`FreezeError::NoPendingFreeze`] unless a freeze is pending.
    pub fn cancel_freeze(&mut self) -> Result<(), FreezeError> {
        if let FreezeState::FreezePending { .. } = self.state {
            self.state = FreezeState::Live;
            Ok(())
        } else {
            Err(FreezeError::NoPendingFreeze)
        }
    }

    /// Resumes live output. The next [`emit`](Self::emit) presents the newest
    /// source frame received after the latched frame, if any; otherwise
    /// output waits for the next source arrival.
    ///
    /// # Errors
    ///
    /// Returns [`FreezeError::NotFrozen`] unless the input is frozen.
    pub fn resume(&mut self) -> Result<(), FreezeError> {
        if let FreezeState::Frozen { .. } = self.state {
            self.state = FreezeState::Live;
            self.latched = None;
            Ok(())
        } else {
            Err(FreezeError::NotFrozen)
        }
    }

    /// Accepts one source arrival.
    ///
    /// # Errors
    ///
    /// Rejects non-increasing frame indexes or PTS values.
    pub fn push_source(&mut self, frame: SourceFrame<H>) -> Result<(), FreezeError> {
        if let Some((previous_index, previous_pts)) = self.last_source {
            if frame.index <= previous_index {
                return Err(FreezeError::SourceIndexNotIncreasing {
                    previous: previous_index,
                    actual: frame.index,
                });
            }
            if frame.pts.as_nanos() <= previous_pts {
                return Err(FreezeError::SourcePtsNotIncreasing {
                    previous: NormalizedTimestamp::from_nanos(previous_pts),
                    actual: frame.pts,
                });
            }
        }
        let source_pts = frame.pts.as_nanos();
        self.last_source = Some((frame.index, source_pts));
        if self.origin.is_none() {
            self.origin = Some(source_pts);
        }
        if let FreezeState::FreezePending { at_index } = self.state
            && frame.index >= at_index
        {
            self.state = FreezeState::Frozen {
                latched_index: frame.index,
            };
            if self.newest_live.take().is_some() {
                self.dropped_source_frames = self.dropped_source_frames.saturating_add(1);
            }
            self.latched = Some(frame);
            self.latched_presented = false;
            return Ok(());
        }
        if self.newest_live.replace(frame).is_some() {
            self.dropped_source_frames = self.dropped_source_frames.saturating_add(1);
        }
        Ok(())
    }

    /// Produces the output frame for the next cadence slot, or `None` when
    /// live and no new source frame is available.
    ///
    /// # Errors
    ///
    /// Returns [`FreezeError::TimestampOverflow`] if the output PTS would
    /// overflow; state is unchanged in that case.
    pub fn emit(&mut self) -> Result<Option<OutputFrame<H>>, FreezeError> {
        match self.state {
            FreezeState::Frozen { .. } => {
                let Some(latched) = self.latched.as_ref() else {
                    return Ok(None);
                };
                let pts = match self.last_output_pts {
                    Some(last) if self.latched_presented => last
                        .checked_add(self.frame_duration_nanos)
                        .ok_or(FreezeError::TimestampOverflow)?,
                    _ => self.live_output_pts(latched.pts)?.as_nanos(),
                };
                let output = OutputFrame {
                    pts: NormalizedTimestamp::from_nanos(pts),
                    source_index: latched.index,
                    source_pts: latched.pts,
                    payload: latched.payload.clone(),
                    origin: OutputOrigin::Frozen,
                };
                self.last_output_pts = Some(pts);
                self.latched_presented = true;
                Ok(Some(output))
            }
            FreezeState::Live | FreezeState::FreezePending { .. } => {
                let Some(live) = self.newest_live.as_ref() else {
                    return Ok(None);
                };
                let pts = self.live_output_pts(live.pts)?.as_nanos();
                let Some(live) = self.newest_live.take() else {
                    return Ok(None);
                };
                self.last_output_pts = Some(pts);
                Ok(Some(OutputFrame {
                    pts: NormalizedTimestamp::from_nanos(pts),
                    source_index: live.index,
                    source_pts: live.pts,
                    payload: live.payload,
                    origin: OutputOrigin::Live,
                }))
            }
        }
    }

    /// Applies `max(map(source_pts), last_output_pts + d)`.
    ///
    /// # Errors
    ///
    /// Returns [`FreezeError::TimestampOverflow`] on arithmetic overflow.
    pub fn live_output_pts(
        &self,
        source_pts: NormalizedTimestamp,
    ) -> Result<NormalizedTimestamp, FreezeError> {
        let mapped = self.map_to_cadence(source_pts.as_nanos())?;
        let pts = match self.last_output_pts {
            Some(last) => {
                let floor = last
                    .checked_add(self.frame_duration_nanos)
                    .ok_or(FreezeError::TimestampOverflow)?;
                mapped.max(floor)
            }
            None => mapped,
        };
        Ok(NormalizedTimestamp::from_nanos(pts))
    }

    /// Rounds `source_pts` up onto the output grid anchored at the first
    /// source PTS.
    fn map_to_cadence(&self, source_pts: i64) -> Result<i64, FreezeError> {
        let origin = self.origin.unwrap_or(source_pts);
        let offset = i128::from(source_pts) - i128::from(origin);
        let duration = i128::from(self.frame_duration_nanos);
        let slots = offset.div_euclid(duration) + i128::from(offset.rem_euclid(duration) != 0);
        let mapped = i128::from(origin) + slots * duration;
        i64::try_from(mapped).map_err(|_| FreezeError::TimestampOverflow)
    }
}
