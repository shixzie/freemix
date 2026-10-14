//! A bounded set of tracks with In and Out phases in one timebase.

use crate::{
    property::Property,
    time::{ClipPosition, Timebase},
    track::{Keyframe, Phase, Track, TrackKey},
    value::Value,
};
use core::fmt;
use fm_types::FrameRate;

/// Most tracks one clip may hold.
pub const MAX_CLIP_TRACKS: usize = 256;
/// Most keyframes one track may hold.
pub const MAX_TRACK_KEYS: usize = 256;
/// Most keyframes one clip may hold across all tracks.
pub const MAX_CLIP_KEYS: usize = 4_096;
/// Longest phase, in frames (ten minutes at 60 fps).
pub const MAX_PHASE_FRAMES: u32 = 36_000;

/// Why a clip or an edit was refused. Edits are atomic: a refused edit leaves
/// the clip unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipError<T> {
    /// A phase is longer than [`MAX_PHASE_FRAMES`].
    PhaseTooLong { phase: Phase, frames: u32 },
    /// More than [`MAX_CLIP_TRACKS`] tracks.
    TooManyTracks { count: usize },
    /// A track holds more than [`MAX_TRACK_KEYS`] keyframes.
    TooManyTrackKeys { track: TrackKey<T>, count: usize },
    /// More than [`MAX_CLIP_KEYS`] keyframes across the clip.
    TooManyKeys { count: usize },
    /// Two tracks share a key.
    DuplicateTrack(TrackKey<T>),
    /// A track has no keyframes.
    EmptyTrack(TrackKey<T>),
    /// A track's keyframe frames are not strictly increasing.
    NonIncreasingKeys { track: TrackKey<T>, frame: u32 },
    /// A keyframe lies after the end of its phase.
    KeyOutsidePhase { track: TrackKey<T>, frame: u32 },
    /// A keyframe value is the wrong kind or out of range for the property.
    InvalidValue { track: TrackKey<T>, frame: u32 },
    /// Retiming would put two keys of `track` closer than one frame. `min_len`
    /// is the shortest length longer than the one requested that retimes
    /// without a collision (the original length when growing collides).
    RetimeCollision { track: TrackKey<T>, min_len: u32 },
    /// A moved or pasted key would land on another key of `track`.
    KeyCollision { track: TrackKey<T>, frame: u32 },
    /// A span to retime from is zero.
    ZeroSpan,
    /// A selected key does not exist.
    UnknownKey { track: TrackKey<T>, frame: u32 },
}

impl<T: fmt::Debug> fmt::Display for ClipError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PhaseTooLong { phase, frames } => write!(
                formatter,
                "{phase:?} phase of {frames} frames exceeds {MAX_PHASE_FRAMES}"
            ),
            Self::TooManyTracks { count } => {
                write!(formatter, "{count} tracks exceed {MAX_CLIP_TRACKS}")
            }
            Self::TooManyTrackKeys { track, count } => write!(
                formatter,
                "track {track:?} has {count} keys, more than {MAX_TRACK_KEYS}"
            ),
            Self::TooManyKeys { count } => {
                write!(formatter, "{count} keys exceed {MAX_CLIP_KEYS}")
            }
            Self::DuplicateTrack(track) => write!(formatter, "duplicate track {track:?}"),
            Self::EmptyTrack(track) => write!(formatter, "track {track:?} has no keys"),
            Self::NonIncreasingKeys { track, frame } => write!(
                formatter,
                "track {track:?} keys are not strictly increasing at frame {frame}"
            ),
            Self::KeyOutsidePhase { track, frame } => write!(
                formatter,
                "track {track:?} key at frame {frame} is outside its phase"
            ),
            Self::InvalidValue { track, frame } => write!(
                formatter,
                "track {track:?} key at frame {frame} has an invalid value"
            ),
            Self::RetimeCollision { track, min_len } => write!(
                formatter,
                "retiming collides keys on track {track:?}; shortest length is {min_len}"
            ),
            Self::KeyCollision { track, frame } => write!(
                formatter,
                "a key already exists on track {track:?} at frame {frame}"
            ),
            Self::ZeroSpan => formatter.write_str("cannot retime from a zero-length span"),
            Self::UnknownKey { track, frame } => {
                write!(formatter, "no key on track {track:?} at frame {frame}")
            }
        }
    }
}

impl<T: fmt::Debug> std::error::Error for ClipError<T> {}

/// Where in its life a clip is sampled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipState {
    /// Static values only.
    Static,
    /// The In phase at a position.
    In(ClipPosition),
    /// On air: the In phase held at its last frame.
    Hold,
    /// The Out phase at a position.
    Out(ClipPosition),
}

/// Keyframed animation of targets `T` over an In and an Out phase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Clip<T> {
    timebase: Timebase,
    in_frames: u32,
    out_frames: u32,
    /// Sorted by key, keys unique.
    tracks: Vec<Track<T>>,
}

impl<T: Copy + Ord> Clip<T> {
    /// An empty clip.
    ///
    /// # Errors
    ///
    /// [`ClipError::PhaseTooLong`] when a phase exceeds [`MAX_PHASE_FRAMES`].
    pub fn new(timebase: Timebase, in_frames: u32, out_frames: u32) -> Result<Self, ClipError<T>> {
        Self::from_tracks(timebase, in_frames, out_frames, Vec::new())
    }

    /// A clip from tracks in any order, validated as a whole.
    ///
    /// # Errors
    ///
    /// Any [`ClipError`] describing a limit or structural violation.
    pub fn from_tracks(
        timebase: Timebase,
        in_frames: u32,
        out_frames: u32,
        mut tracks: Vec<Track<T>>,
    ) -> Result<Self, ClipError<T>> {
        tracks.sort_by_key(|track| track.key);
        let clip = Self {
            timebase,
            in_frames,
            out_frames,
            tracks,
        };
        clip.validate()?;
        Ok(clip)
    }

    #[must_use]
    pub const fn timebase(&self) -> Timebase {
        self.timebase
    }

    #[must_use]
    pub const fn in_frames(&self) -> u32 {
        self.in_frames
    }

    #[must_use]
    pub const fn out_frames(&self) -> u32 {
        self.out_frames
    }

    /// The length of `phase` in frames.
    #[must_use]
    pub const fn phase_frames(&self, phase: Phase) -> u32 {
        match phase {
            Phase::In => self.in_frames,
            Phase::Out => self.out_frames,
        }
    }

    pub(crate) fn set_phase_frames(&mut self, phase: Phase, frames: u32) {
        match phase {
            Phase::In => self.in_frames = frames,
            Phase::Out => self.out_frames = frames,
        }
    }

    /// Every track, sorted by key.
    #[must_use]
    pub fn tracks(&self) -> &[Track<T>] {
        &self.tracks
    }

    #[must_use]
    pub fn track(&self, key: TrackKey<T>) -> Option<&Track<T>> {
        self.tracks
            .binary_search_by_key(&key, |track| track.key)
            .ok()
            .map(|index| &self.tracks[index])
    }

    pub(crate) fn tracks_mut(&mut self) -> &mut Vec<Track<T>> {
        &mut self.tracks
    }

    /// Total keyframes across all tracks.
    #[must_use]
    pub fn key_count(&self) -> usize {
        self.tracks.iter().map(|track| track.keys.len()).sum()
    }

    /// Inserts or replaces one track, atomically.
    ///
    /// # Errors
    ///
    /// Any [`ClipError`] the resulting clip would violate.
    pub fn set_track(&mut self, track: Track<T>) -> Result<(), ClipError<T>> {
        self.try_edit(|clip| {
            clip.put_track(track);
            Ok(())
        })
    }

    /// Removes a track, returning it.
    pub fn remove_track(&mut self, key: TrackKey<T>) -> Option<Track<T>> {
        let index = self
            .tracks
            .binary_search_by_key(&key, |track| track.key)
            .ok()?;
        Some(self.tracks.remove(index))
    }

    /// Changes a phase length, keeping keys where they are.
    ///
    /// # Errors
    ///
    /// [`ClipError::PhaseTooLong`] or [`ClipError::KeyOutsidePhase`].
    pub fn set_phase_frames_keeping_keys(
        &mut self,
        phase: Phase,
        frames: u32,
    ) -> Result<(), ClipError<T>> {
        self.try_edit(|clip| {
            clip.set_phase_frames(phase, frames);
            Ok(())
        })
    }

    pub(crate) fn put_track(&mut self, track: Track<T>) {
        match self
            .tracks
            .binary_search_by_key(&track.key, |existing| existing.key)
        {
            Ok(index) => self.tracks[index] = track,
            Err(index) => self.tracks.insert(index, track),
        }
    }

    /// Applies `edit` to a copy and commits only if it and validation succeed.
    pub(crate) fn try_edit(
        &mut self,
        edit: impl FnOnce(&mut Self) -> Result<(), ClipError<T>>,
    ) -> Result<(), ClipError<T>> {
        let mut next = self.clone();
        edit(&mut next)?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    /// Checks every limit and structural rule.
    ///
    /// # Errors
    ///
    /// The first violation found.
    pub fn validate(&self) -> Result<(), ClipError<T>> {
        for (phase, frames) in [(Phase::In, self.in_frames), (Phase::Out, self.out_frames)] {
            if frames > MAX_PHASE_FRAMES {
                return Err(ClipError::PhaseTooLong { phase, frames });
            }
        }
        if self.tracks.len() > MAX_CLIP_TRACKS {
            return Err(ClipError::TooManyTracks {
                count: self.tracks.len(),
            });
        }
        let total = self.key_count();
        if total > MAX_CLIP_KEYS {
            return Err(ClipError::TooManyKeys { count: total });
        }
        for pair in self.tracks.windows(2) {
            if pair[0].key >= pair[1].key {
                return Err(ClipError::DuplicateTrack(pair[1].key));
            }
        }
        for track in &self.tracks {
            self.validate_track(track)?;
        }
        Ok(())
    }

    fn validate_track(&self, track: &Track<T>) -> Result<(), ClipError<T>> {
        let key = track.key;
        if track.keys.is_empty() {
            return Err(ClipError::EmptyTrack(key));
        }
        if track.keys.len() > MAX_TRACK_KEYS {
            return Err(ClipError::TooManyTrackKeys {
                track: key,
                count: track.keys.len(),
            });
        }
        let length = self.phase_frames(key.phase);
        let mut previous: Option<u32> = None;
        for keyframe in &track.keys {
            if previous.is_some_and(|previous| keyframe.frame <= previous) {
                return Err(ClipError::NonIncreasingKeys {
                    track: key,
                    frame: keyframe.frame,
                });
            }
            if keyframe.frame > length {
                return Err(ClipError::KeyOutsidePhase {
                    track: key,
                    frame: keyframe.frame,
                });
            }
            if !key.property.accepts(keyframe.value) {
                return Err(ClipError::InvalidValue {
                    track: key,
                    frame: keyframe.frame,
                });
            }
            previous = Some(keyframe.frame);
        }
        Ok(())
    }

    /// The sampled value of `property` on `target` in `state`, clamped to the
    /// property range; `None` when there is no such track or in
    /// [`ClipState::Static`]. Positions past the phase end hold the end.
    #[must_use]
    pub fn sample(&self, target: T, property: Property, state: ClipState) -> Option<Value> {
        let (phase, at) = match state {
            ClipState::Static => return None,
            ClipState::In(at) => (Phase::In, at),
            ClipState::Hold => (Phase::In, ClipPosition::frame(self.in_frames)),
            ClipState::Out(at) => (Phase::Out, at),
        };
        let track = self.track(TrackKey::new(target, phase, property))?;
        let at = at.min_frame(self.phase_frames(phase));
        track.sample(at).map(|value| property.clamp(value))
    }

    /// The In phase held at its last frame: what is on air between In and
    /// Out.
    #[must_use]
    pub fn sample_hold(&self, target: T, property: Property) -> Option<Value> {
        self.sample(target, property, ClipState::Hold)
    }

    /// Samples `phase` at `project_frame` frames into it, for a project
    /// running at `project_rate`. When the project and clip rates match this
    /// samples clip frame `project_frame` exactly.
    #[must_use]
    pub fn sample_project(
        &self,
        target: T,
        property: Property,
        phase: Phase,
        project_frame: u64,
        project_rate: FrameRate,
    ) -> Option<Value> {
        let at = self.timebase.position_at(project_frame, project_rate);
        let state = match phase {
            Phase::In => ClipState::In(at),
            Phase::Out => ClipState::Out(at),
        };
        self.sample(target, property, state)
    }

    /// Every animated property of `target` in `state`, in property order.
    #[must_use]
    pub fn evaluate(&self, target: T, state: ClipState) -> Vec<(Property, Value)> {
        Property::ALL
            .into_iter()
            .filter_map(|property| {
                self.sample(target, property, state)
                    .map(|value| (property, value))
            })
            .collect()
    }

    /// The keyframe at exactly `frame` on `track`.
    #[must_use]
    pub fn key(&self, track: TrackKey<T>, frame: u32) -> Option<&Keyframe> {
        let track = self.track(track)?;
        track.key_index(frame).map(|index| &track.keys[index])
    }
}
