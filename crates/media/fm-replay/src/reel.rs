//! Highlight reel compilation into an exact output timeline.
//!
//! A [`HighlightReel`] is an ordered list of event clips, each with one angle,
//! in/out trims on the event range, an exact rational speed, and the
//! transition that brings it in. [`HighlightReel::compile`] turns it into a
//! [`CompiledReel`] whose every output frame maps, using integer arithmetic
//! only, to one or two source layers (camera, synchronized source time, and
//! exact rational mix weight). [`CompiledReel::resolve_frame`] then reuses the
//! replay source timeline and segment catalog to name the exact recorded
//! frame and segment for each layer. Nothing is decoded or rendered.

use core::{fmt, ops::Range};

use fm_frame::NormalizedTimestamp;

use crate::{
    CameraId, EventDatabase, EventId, HighlightId, ReplayError, RollingSegmentCatalog, SegmentId,
    SourceObservation, SourceTimeline, TimelineRange,
};

/// Maximum clips in one highlight reel.
pub const MAX_REEL_CLIPS: usize = 256;
/// Maximum frames in one transition.
pub const MAX_TRANSITION_FRAMES: u32 = 600;
/// Maximum compiled output frames: four hours at 60 frames per second.
pub const MAX_REEL_FRAMES: u64 = 864_000;
/// Highest accepted output frame rate, in frames per second.
pub const MAX_REEL_FPS: u32 = 240;
/// Largest accepted rational speed (16x) and inverse of the smallest (1/16x).
pub const MAX_REEL_SPEED_RATIO: u32 = 16;
/// Largest accepted numerator or denominator for rates and speeds.
pub const MAX_RATIONAL_TERM: u32 = 1_000_000;

const NANOS_PER_SECOND: u128 = 1_000_000_000;

const fn gcd(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let next = left % right;
        left = right;
        right = next;
    }
    left
}

/// Exact output frame rate `numerator / denominator` frames per second,
/// stored in lowest terms.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReelFrameRate {
    numerator: u32,
    denominator: u32,
}

impl ReelFrameRate {
    /// Creates a rate between 1 and [`MAX_REEL_FPS`] frames per second.
    ///
    /// # Errors
    ///
    /// Zero terms, terms above [`MAX_RATIONAL_TERM`], and rates outside the
    /// accepted range are rejected.
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, ReelError> {
        let invalid = ReelError::InvalidFrameRate {
            numerator,
            denominator,
        };
        if numerator == 0
            || denominator == 0
            || numerator > MAX_RATIONAL_TERM
            || denominator > MAX_RATIONAL_TERM
            || numerator < denominator
            || u64::from(numerator) > u64::from(denominator) * u64::from(MAX_REEL_FPS)
        {
            return Err(invalid);
        }
        let divisor = gcd(numerator, denominator);
        Ok(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    #[must_use]
    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    #[must_use]
    pub const fn denominator(self) -> u32 {
        self.denominator
    }
}

/// Exact clip speed `numerator / denominator` times real time, stored in
/// lowest terms. Only forward speeds from 1/16x through 16x are accepted.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReelSpeed {
    numerator: u32,
    denominator: u32,
}

impl ReelSpeed {
    pub const REAL_TIME: Self = Self {
        numerator: 1,
        denominator: 1,
    };

    /// Whether the speed is real time (`n/n`).
    #[must_use]
    pub const fn is_real_time(self) -> bool {
        self.numerator == self.denominator
    }

    /// Creates an exact forward speed.
    ///
    /// # Errors
    ///
    /// Zero terms, terms above [`MAX_RATIONAL_TERM`], and ratios outside
    /// 1/[`MAX_REEL_SPEED_RATIO`] through [`MAX_REEL_SPEED_RATIO`] are rejected.
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, ReelError> {
        let ratio = u64::from(MAX_REEL_SPEED_RATIO);
        if numerator == 0
            || denominator == 0
            || numerator > MAX_RATIONAL_TERM
            || denominator > MAX_RATIONAL_TERM
            || u64::from(numerator) > u64::from(denominator) * ratio
            || u64::from(denominator) > u64::from(numerator) * ratio
        {
            return Err(ReelError::InvalidSpeed {
                numerator,
                denominator,
            });
        }
        let divisor = gcd(numerator, denominator);
        Ok(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    #[must_use]
    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    #[must_use]
    pub const fn denominator(self) -> u32 {
        self.denominator
    }
}

/// How a clip enters from the previous clip.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReelTransition {
    Cut,
    /// Overlaps the last `frames` of the previous clip with the first
    /// `frames` of this clip using a linear rational mix.
    Dissolve {
        frames: u32,
    },
}

impl ReelTransition {
    #[must_use]
    pub const fn frames(self) -> u32 {
        match self {
            Self::Cut => 0,
            Self::Dissolve { frames } => frames,
        }
    }
}

/// Exact layer weight `numerator / denominator` in `[0, 1]`.
///
/// Weights are not reduced, so the two layers of one transition frame share
/// a denominator and their numerators sum to it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MixWeight {
    pub numerator: u32,
    pub denominator: u32,
}

impl MixWeight {
    pub const FULL: Self = Self {
        numerator: 1,
        denominator: 1,
    };
}

/// One requested clip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReelClipSpec {
    pub id: HighlightId,
    pub event_id: EventId,
    pub angle: CameraId,
    /// Nanoseconds trimmed after the event mark-in.
    pub trim_in_nanos: u64,
    /// Nanoseconds trimmed before the event mark-out.
    pub trim_out_nanos: u64,
    pub speed: ReelSpeed,
    /// Transition from the previous clip; the first clip must use a cut.
    pub transition_in: ReelTransition,
    /// Clip carries program audio, so a music bed ducks under it.
    pub program_audio: bool,
}

/// Ordered, editable highlight reel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HighlightReel {
    frame_rate: ReelFrameRate,
    clips: Vec<ReelClipSpec>,
}

impl HighlightReel {
    #[must_use]
    pub const fn new(frame_rate: ReelFrameRate) -> Self {
        Self {
            frame_rate,
            clips: Vec::new(),
        }
    }

    #[must_use]
    pub const fn frame_rate(&self) -> ReelFrameRate {
        self.frame_rate
    }

    #[must_use]
    pub fn clips(&self) -> &[ReelClipSpec] {
        &self.clips
    }

    /// Appends one clip.
    ///
    /// # Errors
    ///
    /// Clip IDs must be unique and at most [`MAX_REEL_CLIPS`] are allowed.
    pub fn push(&mut self, clip: ReelClipSpec) -> Result<(), ReelError> {
        if self.clips.iter().any(|existing| existing.id == clip.id) {
            return Err(ReelError::DuplicateClip(clip.id));
        }
        if self.clips.len() >= MAX_REEL_CLIPS {
            return Err(ReelError::TooManyClips {
                maximum: MAX_REEL_CLIPS,
            });
        }
        self.clips.push(clip);
        Ok(())
    }

    /// Removes one clip and returns it.
    ///
    /// # Errors
    ///
    /// The clip must exist.
    pub fn remove(&mut self, id: HighlightId) -> Result<ReelClipSpec, ReelError> {
        let index = self.index_of(id)?;
        Ok(self.clips.remove(index))
    }

    /// Moves one clip to `index` (clamped to the last position).
    ///
    /// # Errors
    ///
    /// The clip must exist.
    pub fn move_clip(&mut self, id: HighlightId, index: usize) -> Result<(), ReelError> {
        let from = self.index_of(id)?;
        let clip = self.clips.remove(from);
        let to = index.min(self.clips.len());
        self.clips.insert(to, clip);
        Ok(())
    }

    fn index_of(&self, id: HighlightId) -> Result<usize, ReelError> {
        self.clips
            .iter()
            .position(|clip| clip.id == id)
            .ok_or(ReelError::UnknownClip(id))
    }

    /// Compiles the reel against the current event database.
    ///
    /// # Errors
    ///
    /// The reel must be non-empty; every event must exist and contain the
    /// clip angle; trims must leave media; the first clip must cut in; each
    /// dissolve must be 1..=[`MAX_TRANSITION_FRAMES`] frames and fit inside
    /// both adjacent clips without colliding with another transition; and the
    /// output must not exceed [`MAX_REEL_FRAMES`].
    pub fn compile(&self, events: &EventDatabase) -> Result<CompiledReel, ReelError> {
        if self.clips.is_empty() {
            return Err(ReelError::EmptyReel);
        }
        let mut compiled: Vec<CompiledClip> = Vec::with_capacity(self.clips.len());
        for spec in &self.clips {
            let event = events
                .event(spec.event_id)
                .ok_or(ReelError::UnknownEvent(spec.event_id))?;
            if !event.angles.contains(&spec.angle) {
                return Err(ReelError::AngleNotInEvent {
                    event: spec.event_id,
                    camera: spec.angle,
                });
            }
            let start =
                i128::from(event.timeline.start.as_nanos()) + i128::from(spec.trim_in_nanos);
            let end = i128::from(event.timeline.end.as_nanos()) - i128::from(spec.trim_out_nanos);
            if start >= end {
                return Err(ReelError::TrimLeavesNoMedia(spec.id));
            }
            let source = TimelineRange {
                start: NormalizedTimestamp::from_nanos(
                    i64::try_from(start).map_err(|_| ReelError::TrimLeavesNoMedia(spec.id))?,
                ),
                end: NormalizedTimestamp::from_nanos(
                    i64::try_from(end).map_err(|_| ReelError::TrimLeavesNoMedia(spec.id))?,
                ),
            };
            let frame_count = output_frames(source.duration_nanos(), self.frame_rate, spec.speed)
                .filter(|count| *count <= MAX_REEL_FRAMES)
                .ok_or(ReelError::ReelTooLong {
                    maximum: MAX_REEL_FRAMES,
                })?;
            let transition = spec.transition_in.frames();
            let output_start = match compiled.last() {
                None => {
                    if spec.transition_in != ReelTransition::Cut {
                        return Err(ReelError::FirstClipTransition(spec.id));
                    }
                    0
                }
                Some(previous) => {
                    if let ReelTransition::Dissolve { frames } = spec.transition_in
                        && (frames == 0 || frames > MAX_TRANSITION_FRAMES)
                    {
                        return Err(ReelError::InvalidTransition {
                            clip: spec.id,
                            frames,
                        });
                    }
                    let available = (previous.frame_count
                        - u64::from(previous.transition_in_frames))
                    .min(frame_count);
                    if u64::from(transition) > available {
                        return Err(ReelError::TransitionTooLong {
                            clip: spec.id,
                            frames: transition,
                            available,
                        });
                    }
                    previous.output_end() - u64::from(transition)
                }
            };
            if output_start + frame_count > MAX_REEL_FRAMES {
                return Err(ReelError::ReelTooLong {
                    maximum: MAX_REEL_FRAMES,
                });
            }
            compiled.push(CompiledClip {
                id: spec.id,
                event_id: spec.event_id,
                angle: spec.angle,
                source,
                speed: spec.speed,
                output_start,
                frame_count,
                transition_in_frames: transition,
                program_audio: spec.program_audio,
            });
        }
        let total_frames = compiled.last().map_or(0, CompiledClip::output_end);
        Ok(CompiledReel {
            frame_rate: self.frame_rate,
            clips: compiled,
            total_frames,
        })
    }
}

/// Smallest `n` with `n * frame_duration * speed >= duration`.
fn output_frames(duration_nanos: u64, rate: ReelFrameRate, speed: ReelSpeed) -> Option<u64> {
    let numerator =
        u128::from(duration_nanos) * u128::from(rate.numerator) * u128::from(speed.denominator);
    let denominator = NANOS_PER_SECOND * u128::from(rate.denominator) * u128::from(speed.numerator);
    u64::try_from(numerator.div_ceil(denominator)).ok()
}

/// One clip placed on the output timeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompiledClip {
    pub id: HighlightId,
    pub event_id: EventId,
    pub angle: CameraId,
    /// Trimmed half-open source range on the synchronized timeline.
    pub source: TimelineRange,
    pub speed: ReelSpeed,
    /// First output frame of the clip, including its incoming transition.
    pub output_start: u64,
    pub frame_count: u64,
    pub transition_in_frames: u32,
    pub program_audio: bool,
}

impl CompiledClip {
    /// Exclusive output end frame.
    #[must_use]
    pub const fn output_end(&self) -> u64 {
        self.output_start + self.frame_count
    }

    /// Exact synchronized source time for a clip-local output frame:
    /// `source.start + floor(local * frame_duration * speed)`.
    #[must_use]
    pub fn source_time(&self, local_frame: u64, rate: ReelFrameRate) -> NormalizedTimestamp {
        let offset = u128::from(local_frame)
            * NANOS_PER_SECOND
            * u128::from(rate.denominator)
            * u128::from(self.speed.numerator)
            / (u128::from(rate.numerator) * u128::from(self.speed.denominator));
        let offset = i128::try_from(offset).unwrap_or(i128::MAX);
        let time = i128::from(self.source.start.as_nanos())
            .saturating_add(offset)
            .min(i128::from(self.source.end.as_nanos()) - 1);
        NormalizedTimestamp::from_nanos(i64::try_from(time).unwrap_or(i64::MAX))
    }
}

/// One layer of an output frame before media resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReelLayerPlan {
    pub clip_index: usize,
    pub clip: HighlightId,
    pub camera_id: CameraId,
    /// Frame index inside the clip.
    pub local_frame: u64,
    /// Synchronized source time requested for this layer.
    pub target: NormalizedTimestamp,
    pub weight: MixWeight,
    /// The clip's playback speed.
    pub speed: ReelSpeed,
}

/// One layer resolved to an exact recorded frame and segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReelLayer {
    pub plan: ReelLayerPlan,
    /// Latest observation at or before the target.
    pub observation: SourceObservation,
    pub segment_id: SegmentId,
    pub offset_nanos: u64,
}

/// Exact composition of one output frame, bottom layer first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReelFrame {
    pub output_frame: u64,
    pub layers: Vec<ReelLayer>,
}

/// Exact output timeline produced by [`HighlightReel::compile`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledReel {
    frame_rate: ReelFrameRate,
    clips: Vec<CompiledClip>,
    total_frames: u64,
}

impl CompiledReel {
    #[must_use]
    pub const fn frame_rate(&self) -> ReelFrameRate {
        self.frame_rate
    }

    #[must_use]
    pub fn clips(&self) -> &[CompiledClip] {
        &self.clips
    }

    #[must_use]
    pub const fn total_frames(&self) -> u64 {
        self.total_frames
    }

    /// First audio sample of `output_frame` at `sample_rate`:
    /// `floor(frame * sample_rate / frame_rate)`.
    #[must_use]
    pub fn frame_start_sample(&self, output_frame: u64, sample_rate: u32) -> u64 {
        let samples = u128::from(output_frame)
            * u128::from(sample_rate)
            * u128::from(self.frame_rate.denominator)
            / u128::from(self.frame_rate.numerator);
        u64::try_from(samples).unwrap_or(u64::MAX)
    }

    /// Total output samples at `sample_rate`.
    #[must_use]
    pub fn total_samples(&self, sample_rate: u32) -> u64 {
        self.frame_start_sample(self.total_frames, sample_rate)
    }

    /// Merged, ascending output frame ranges where a program-audio clip is
    /// on screen (including its transitions).
    #[must_use]
    pub fn program_audio_frames(&self) -> Vec<Range<u64>> {
        let mut ranges: Vec<Range<u64>> = Vec::new();
        for clip in self.clips.iter().filter(|clip| clip.program_audio) {
            match ranges.last_mut() {
                Some(last) if clip.output_start <= last.end => {
                    last.end = last.end.max(clip.output_end());
                }
                _ => ranges.push(clip.output_start..clip.output_end()),
            }
        }
        ranges
    }

    /// Plans the one or two layers that compose `output_frame` without
    /// touching media. During a dissolve of `n` frames, transition frame `k`
    /// weights the incoming clip `(k + 1) / (n + 1)` and the outgoing clip
    /// `(n - k) / (n + 1)`.
    ///
    /// # Errors
    ///
    /// The frame must be inside the reel.
    pub fn layers_at(&self, output_frame: u64) -> Result<Vec<ReelLayerPlan>, ReelError> {
        if output_frame >= self.total_frames {
            return Err(ReelError::FrameOutOfRange {
                frame: output_frame,
                total: self.total_frames,
            });
        }
        let after = self
            .clips
            .partition_point(|clip| clip.output_start <= output_frame);
        let current_index = after - 1;
        let current = &self.clips[current_index];
        let mut layers = Vec::with_capacity(2);
        let local = output_frame - current.output_start;
        let mut current_weight = MixWeight::FULL;
        if local < u64::from(current.transition_in_frames) && current_index > 0 {
            let previous = &self.clips[current_index - 1];
            let frames = current.transition_in_frames;
            let step = u32::try_from(local).unwrap_or(frames - 1);
            let denominator = frames + 1;
            current_weight = MixWeight {
                numerator: step + 1,
                denominator,
            };
            let previous_local = output_frame - previous.output_start;
            layers.push(self.layer(
                current_index - 1,
                previous_local,
                MixWeight {
                    numerator: frames - step,
                    denominator,
                },
            ));
        }
        layers.push(self.layer(current_index, local, current_weight));
        Ok(layers)
    }

    fn layer(&self, clip_index: usize, local_frame: u64, weight: MixWeight) -> ReelLayerPlan {
        let clip = &self.clips[clip_index];
        ReelLayerPlan {
            clip_index,
            clip: clip.id,
            camera_id: clip.angle,
            local_frame,
            target: clip.source_time(local_frame, self.frame_rate),
            weight,
            speed: clip.speed,
        }
    }

    /// Resolves every layer of `output_frame` to the exact recorded source
    /// frame (latest observation at or before the target) and its segment.
    ///
    /// # Errors
    ///
    /// The frame must be inside the reel and every layer must resolve to an
    /// observation inside a cataloged segment.
    pub fn resolve_frame(
        &self,
        output_frame: u64,
        timeline: &SourceTimeline,
        catalog: &RollingSegmentCatalog,
    ) -> Result<ReelFrame, ReelError> {
        let plans = self.layers_at(output_frame)?;
        let mut layers = Vec::with_capacity(plans.len());
        for plan in plans {
            let unresolved = |error| ReelError::Unresolved {
                clip: plan.clip,
                output_frame,
                error,
            };
            let observation = timeline
                .resolve(plan.camera_id, plan.target)
                .map_err(unresolved)?;
            let segment = catalog
                .segment_at(plan.camera_id, observation.synchronized_at)
                .ok_or_else(|| {
                    unresolved(ReplayError::SegmentUnavailable {
                        camera: plan.camera_id,
                        at: observation.synchronized_at,
                    })
                })?;
            let offset = i128::from(plan.target.as_nanos())
                - i128::from(observation.synchronized_at.as_nanos());
            layers.push(ReelLayer {
                plan,
                observation,
                segment_id: segment.id,
                offset_nanos: u64::try_from(offset).unwrap_or(0),
            });
        }
        Ok(ReelFrame {
            output_frame,
            layers,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReelError {
    EmptyReel,
    TooManyClips {
        maximum: usize,
    },
    DuplicateClip(HighlightId),
    UnknownClip(HighlightId),
    UnknownEvent(EventId),
    AngleNotInEvent {
        event: EventId,
        camera: CameraId,
    },
    InvalidFrameRate {
        numerator: u32,
        denominator: u32,
    },
    InvalidSpeed {
        numerator: u32,
        denominator: u32,
    },
    TrimLeavesNoMedia(HighlightId),
    FirstClipTransition(HighlightId),
    InvalidTransition {
        clip: HighlightId,
        frames: u32,
    },
    TransitionTooLong {
        clip: HighlightId,
        frames: u32,
        available: u64,
    },
    ReelTooLong {
        maximum: u64,
    },
    FrameOutOfRange {
        frame: u64,
        total: u64,
    },
    Unresolved {
        clip: HighlightId,
        output_frame: u64,
        error: ReplayError,
    },
}

impl fmt::Display for ReelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyReel => formatter.write_str("highlight reel has no clips"),
            Self::TooManyClips { maximum } => {
                write!(formatter, "highlight reel exceeds {maximum} clips")
            }
            Self::DuplicateClip(id) => write!(formatter, "duplicate reel clip {id}"),
            Self::UnknownClip(id) => write!(formatter, "unknown reel clip {id}"),
            Self::UnknownEvent(id) => write!(formatter, "unknown replay event {id}"),
            Self::AngleNotInEvent { event, camera } => {
                write!(
                    formatter,
                    "camera {camera} is not an angle of event {event}"
                )
            }
            Self::InvalidFrameRate {
                numerator,
                denominator,
            } => write!(
                formatter,
                "frame rate {numerator}/{denominator} is outside 1..={MAX_REEL_FPS} fps"
            ),
            Self::InvalidSpeed {
                numerator,
                denominator,
            } => write!(
                formatter,
                "speed {numerator}/{denominator} is outside 1/{MAX_REEL_SPEED_RATIO}..={MAX_REEL_SPEED_RATIO}x"
            ),
            Self::TrimLeavesNoMedia(id) => {
                write!(formatter, "trims leave no media in reel clip {id}")
            }
            Self::FirstClipTransition(id) => {
                write!(formatter, "first reel clip {id} must cut in")
            }
            Self::InvalidTransition { clip, frames } => write!(
                formatter,
                "reel clip {clip} dissolve of {frames} frames is outside 1..={MAX_TRANSITION_FRAMES}"
            ),
            Self::TransitionTooLong {
                clip,
                frames,
                available,
            } => write!(
                formatter,
                "reel clip {clip} transition of {frames} frames exceeds the {available} available"
            ),
            Self::ReelTooLong { maximum } => {
                write!(formatter, "highlight reel exceeds {maximum} output frames")
            }
            Self::FrameOutOfRange { frame, total } => {
                write!(
                    formatter,
                    "output frame {frame} is outside a {total}-frame reel"
                )
            }
            Self::Unresolved {
                clip,
                output_frame,
                error,
            } => write!(
                formatter,
                "reel clip {clip} at output frame {output_frame} is unresolved: {error}"
            ),
        }
    }
}

impl std::error::Error for ReelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unresolved { error, .. } => Some(error),
            _ => None,
        }
    }
}
