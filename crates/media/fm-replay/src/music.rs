//! Background music bed planning in the exact sample domain.
//!
//! A [`MusicBedSpec`] lays a playlist of tracks (durations in samples) under a
//! program of known sample length, optionally looping, with linear
//! crossfades between consecutive entries, a ducking envelope under program
//! audio, and a final fade. Gains are unsigned Q16 fixed point
//! ([`BedGain::UNITY`] is 65 536) and every ramp is evaluated with integer
//! floor division, so a plan is bit-for-bit reproducible. Nothing is decoded
//! or mixed here.

use core::{fmt, ops::Range};

use crate::{CompiledReel, MusicTrackId};

/// Maximum tracks in one bed playlist.
pub const MAX_MUSIC_TRACKS: usize = 64;
/// Maximum placed entries after looping.
pub const MAX_BED_ENTRIES: usize = 4_096;
/// Maximum program-audio intervals that duck the bed.
pub const MAX_DUCK_INTERVALS: usize = 1_024;
/// Lowest accepted sample rate.
pub const MIN_BED_SAMPLE_RATE: u32 = 8_000;
/// Highest accepted sample rate.
pub const MAX_BED_SAMPLE_RATE: u32 = 192_000;

/// Unsigned Q16 linear gain no louder than unity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BedGain(u32);

impl BedGain {
    pub const UNITY: Self = Self(65_536);
    pub const SILENCE: Self = Self(0);

    /// Creates a Q16 gain.
    ///
    /// # Errors
    ///
    /// Values above [`Self::UNITY`] are rejected.
    pub const fn from_q16(value: u32) -> Result<Self, MusicBedError> {
        if value > Self::UNITY.0 {
            Err(MusicBedError::InvalidGain(value))
        } else {
            Ok(Self(value))
        }
    }

    #[must_use]
    pub const fn q16(self) -> u32 {
        self.0
    }

    /// Exact product `floor(self * other / UNITY)`.
    #[must_use]
    pub fn scale(self, other: Self) -> Self {
        let product = (u64::from(self.0) * u64::from(other.0)) >> 16;
        Self(u32::try_from(product).unwrap_or(Self::UNITY.0))
    }
}

/// Linear ramp from `from` at `start` to `to` at `start + length`, holding
/// outside; inside it returns `from + floor((to - from) * (sample - start) / length)`.
fn ramp(from: BedGain, to: BedGain, start: u64, length: u64, sample: u64) -> BedGain {
    if sample < start {
        return from;
    }
    let elapsed = sample - start;
    if elapsed >= length {
        return to;
    }
    let delta = i128::from(to.0) - i128::from(from.0);
    let value = i128::from(from.0) + (delta * i128::from(elapsed)).div_euclid(i128::from(length));
    BedGain(u32::try_from(value).expect("ramp stays between its endpoints"))
}

/// One playlist track.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MusicTrack {
    pub id: MusicTrackId,
    pub duration_samples: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BedRepeat {
    /// Play the playlist once; the bed is silent after its last entry.
    Once,
    /// Repeat the playlist until the program ends.
    Loop,
}

/// Bed ducking under program-audio clips.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DuckingSpec {
    /// Bed gain while program audio plays.
    pub ducked_gain: BedGain,
    /// Ramp length ending exactly when program audio starts.
    pub attack_samples: u64,
    /// Ramp length starting exactly when program audio ends.
    pub release_samples: u64,
}

/// Requested music bed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MusicBedSpec {
    pub sample_rate: u32,
    pub tracks: Vec<MusicTrack>,
    pub repeat: BedRepeat,
    /// Overlap between consecutive entries; at most half of every track.
    pub crossfade_samples: u64,
    /// Bed level outside ducking.
    pub bed_gain: BedGain,
    pub ducking: Option<DuckingSpec>,
    /// Fade to silence ending exactly at the program end.
    pub end_fade_samples: u64,
}

/// One track placement on the output sample timeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BedEntry {
    pub track: MusicTrackId,
    /// Playlist position of the track.
    pub playlist_index: usize,
    /// Zero-based playlist repetition.
    pub iteration: u64,
    /// First output sample.
    pub output_start: u64,
    /// Samples placed (truncated at the program end).
    pub length_samples: u64,
    /// Untruncated track duration.
    pub duration_samples: u64,
    pub fade_in_samples: u64,
    /// Fade-out ending at `output_start + duration_samples`.
    pub fade_out_samples: u64,
}

impl BedEntry {
    #[must_use]
    pub const fn output_end(&self) -> u64 {
        self.output_start + self.length_samples
    }

    /// Crossfade gain of this entry alone at an output sample.
    #[must_use]
    pub fn fade_gain(&self, sample: u64) -> BedGain {
        if sample < self.output_start || sample >= self.output_end() {
            return BedGain::SILENCE;
        }
        let fade_in = ramp(
            BedGain::SILENCE,
            BedGain::UNITY,
            self.output_start,
            self.fade_in_samples,
            sample,
        );
        let fade_out = ramp(
            BedGain::UNITY,
            BedGain::SILENCE,
            self.output_start + self.duration_samples - self.fade_out_samples,
            self.fade_out_samples,
            sample,
        );
        fade_in.scale(fade_out)
    }
}

/// Breakpoint of a piecewise-linear gain envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GainPoint {
    pub sample: u64,
    pub gain: BedGain,
}

/// One audible bed voice at an output sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BedVoice {
    pub entry_index: usize,
    pub track: MusicTrackId,
    /// Sample read from the track.
    pub source_sample: u64,
    /// Crossfade x ducking envelope x end fade.
    pub gain: BedGain,
}

/// Exact bed plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MusicBedPlan {
    sample_rate: u32,
    program_samples: u64,
    entries: Vec<BedEntry>,
    envelope: Vec<GainPoint>,
    end_fade_samples: u64,
}

impl MusicBedSpec {
    /// Plans the bed under a compiled reel; clips flagged with program audio
    /// duck the bed.
    ///
    /// # Errors
    ///
    /// See [`Self::plan_samples`].
    pub fn plan(&self, reel: &CompiledReel) -> Result<MusicBedPlan, MusicBedError> {
        self.validate_sample_rate()?;
        let ducks: Vec<Range<u64>> = reel
            .program_audio_frames()
            .into_iter()
            .map(|frames| {
                reel.frame_start_sample(frames.start, self.sample_rate)
                    ..reel.frame_start_sample(frames.end, self.sample_rate)
            })
            .filter(|samples| !samples.is_empty())
            .collect();
        self.plan_samples(reel.total_samples(self.sample_rate), &ducks)
    }

    const fn validate_sample_rate(&self) -> Result<(), MusicBedError> {
        if self.sample_rate < MIN_BED_SAMPLE_RATE || self.sample_rate > MAX_BED_SAMPLE_RATE {
            return Err(MusicBedError::InvalidSampleRate(self.sample_rate));
        }
        Ok(())
    }

    /// Plans the bed under a program of `program_samples` with explicit
    /// ascending, non-overlapping duck intervals.
    ///
    /// # Errors
    ///
    /// The sample rate must be supported, the program non-empty, the
    /// playlist non-empty and bounded, every track non-empty and at least
    /// twice the crossfade, the end fade no longer than the program, duck
    /// intervals non-empty, ascending, disjoint, inside the program, and
    /// bounded, and looping must stay within [`MAX_BED_ENTRIES`].
    pub fn plan_samples(
        &self,
        program_samples: u64,
        ducks: &[Range<u64>],
    ) -> Result<MusicBedPlan, MusicBedError> {
        self.validate_sample_rate()?;
        if program_samples == 0 {
            return Err(MusicBedError::EmptyProgram);
        }
        if self.tracks.is_empty() {
            return Err(MusicBedError::NoTracks);
        }
        if self.tracks.len() > MAX_MUSIC_TRACKS {
            return Err(MusicBedError::TooManyTracks {
                maximum: MAX_MUSIC_TRACKS,
            });
        }
        for track in &self.tracks {
            if track.duration_samples == 0 {
                return Err(MusicBedError::EmptyTrack(track.id));
            }
            if self.crossfade_samples > track.duration_samples / 2 {
                return Err(MusicBedError::CrossfadeTooLong {
                    track: track.id,
                    crossfade_samples: self.crossfade_samples,
                    duration_samples: track.duration_samples,
                });
            }
        }
        if self.end_fade_samples > program_samples {
            return Err(MusicBedError::EndFadeTooLong {
                end_fade_samples: self.end_fade_samples,
                program_samples,
            });
        }
        if ducks.len() > MAX_DUCK_INTERVALS {
            return Err(MusicBedError::TooManyDuckIntervals {
                maximum: MAX_DUCK_INTERVALS,
            });
        }
        let mut previous_end = 0;
        for (index, duck) in ducks.iter().enumerate() {
            if duck.is_empty()
                || duck.end > program_samples
                || (index > 0 && duck.start < previous_end)
            {
                return Err(MusicBedError::InvalidDuckInterval(duck.clone()));
            }
            previous_end = duck.end;
        }
        let entries = self.place_entries(program_samples)?;
        let envelope = self.envelope(ducks);
        Ok(MusicBedPlan {
            sample_rate: self.sample_rate,
            program_samples,
            entries,
            envelope,
            end_fade_samples: self.end_fade_samples,
        })
    }

    fn place_entries(&self, program_samples: u64) -> Result<Vec<BedEntry>, MusicBedError> {
        let mut entries: Vec<BedEntry> = Vec::new();
        let mut start = 0_u64;
        let mut position = 0_usize;
        let mut iteration = 0_u64;
        while start < program_samples {
            if position == self.tracks.len() {
                if self.repeat == BedRepeat::Once {
                    break;
                }
                position = 0;
                iteration += 1;
            }
            if entries.len() == MAX_BED_ENTRIES {
                return Err(MusicBedError::TooManyEntries {
                    maximum: MAX_BED_ENTRIES,
                });
            }
            let track = self.tracks[position];
            if let Some(previous) = entries.last_mut() {
                previous.fade_out_samples = self.crossfade_samples;
            }
            entries.push(BedEntry {
                track: track.id,
                playlist_index: position,
                iteration,
                output_start: start,
                length_samples: track.duration_samples.min(program_samples - start),
                duration_samples: track.duration_samples,
                fade_in_samples: if entries.is_empty() {
                    0
                } else {
                    self.crossfade_samples
                },
                fade_out_samples: 0,
            });
            start = start + track.duration_samples - self.crossfade_samples;
            position += 1;
        }
        Ok(entries)
    }

    fn envelope(&self, ducks: &[Range<u64>]) -> Vec<GainPoint> {
        let bed = self.bed_gain;
        let mut points = vec![GainPoint {
            sample: 0,
            gain: bed,
        }];
        let Some(ducking) = self.ducking else {
            return points;
        };
        let ducked = ducking.ducked_gain.scale(bed);
        // Merge ducks whose release and next attack would collide.
        let mut merged: Vec<Range<u64>> = Vec::with_capacity(ducks.len());
        for duck in ducks {
            match merged.last_mut() {
                Some(last)
                    if duck.start.saturating_sub(ducking.attack_samples)
                        <= last.end.saturating_add(ducking.release_samples) =>
                {
                    last.end = duck.end;
                }
                _ => merged.push(duck.clone()),
            }
        }
        for duck in merged {
            let attack_start = duck.start.saturating_sub(ducking.attack_samples);
            if attack_start > 0 {
                points.push(GainPoint {
                    sample: attack_start,
                    gain: bed,
                });
            }
            points.push(GainPoint {
                sample: duck.start,
                gain: ducked,
            });
            points.push(GainPoint {
                sample: duck.end,
                gain: ducked,
            });
            points.push(GainPoint {
                sample: duck.end.saturating_add(ducking.release_samples),
                gain: bed,
            });
        }
        points
    }
}

impl MusicBedPlan {
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    #[must_use]
    pub const fn program_samples(&self) -> u64 {
        self.program_samples
    }

    #[must_use]
    pub fn entries(&self) -> &[BedEntry] {
        &self.entries
    }

    /// Piecewise-linear bed level including ducking. Points are ordered by
    /// sample; equal samples form an instantaneous step.
    #[must_use]
    pub fn envelope(&self) -> &[GainPoint] {
        &self.envelope
    }

    /// Exclusive output sample where the last bed entry ends.
    #[must_use]
    pub fn bed_end(&self) -> u64 {
        self.entries.last().map_or(0, BedEntry::output_end)
    }

    /// Bed level (bed gain and ducking) at an output sample.
    #[must_use]
    pub fn envelope_gain(&self, sample: u64) -> BedGain {
        let after = self
            .envelope
            .partition_point(|point| point.sample <= sample);
        if after == 0 {
            return self.envelope[0].gain;
        }
        let previous = self.envelope[after - 1];
        match self.envelope.get(after) {
            None => previous.gain,
            Some(next) => ramp(
                previous.gain,
                next.gain,
                previous.sample,
                next.sample - previous.sample,
                sample,
            ),
        }
    }

    /// Final fade gain at an output sample.
    #[must_use]
    pub fn end_fade_gain(&self, sample: u64) -> BedGain {
        ramp(
            BedGain::UNITY,
            BedGain::SILENCE,
            self.program_samples - self.end_fade_samples,
            self.end_fade_samples,
            sample,
        )
    }

    /// Audible voices at an output sample (at most two, during a crossfade),
    /// in entry order. Samples at or past the program end have no voices.
    #[must_use]
    pub fn voices_at(&self, sample: u64) -> Vec<BedVoice> {
        if sample >= self.program_samples {
            return Vec::new();
        }
        let level = self.envelope_gain(sample).scale(self.end_fade_gain(sample));
        let after = self
            .entries
            .partition_point(|entry| entry.output_start <= sample);
        let first = after.saturating_sub(2);
        self.entries[first..after]
            .iter()
            .enumerate()
            .filter(|(_, entry)| sample < entry.output_end())
            .map(|(offset, entry)| BedVoice {
                entry_index: first + offset,
                track: entry.track,
                source_sample: sample - entry.output_start,
                gain: entry.fade_gain(sample).scale(level),
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MusicBedError {
    InvalidSampleRate(u32),
    InvalidGain(u32),
    EmptyProgram,
    NoTracks,
    TooManyTracks {
        maximum: usize,
    },
    EmptyTrack(MusicTrackId),
    CrossfadeTooLong {
        track: MusicTrackId,
        crossfade_samples: u64,
        duration_samples: u64,
    },
    EndFadeTooLong {
        end_fade_samples: u64,
        program_samples: u64,
    },
    TooManyDuckIntervals {
        maximum: usize,
    },
    InvalidDuckInterval(Range<u64>),
    TooManyEntries {
        maximum: usize,
    },
}

impl fmt::Display for MusicBedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSampleRate(rate) => write!(
                formatter,
                "sample rate {rate} is outside {MIN_BED_SAMPLE_RATE}..={MAX_BED_SAMPLE_RATE}"
            ),
            Self::InvalidGain(value) => {
                write!(formatter, "bed gain {value} exceeds Q16 unity")
            }
            Self::EmptyProgram => formatter.write_str("program has no samples"),
            Self::NoTracks => formatter.write_str("music bed has no tracks"),
            Self::TooManyTracks { maximum } => {
                write!(formatter, "music bed exceeds {maximum} tracks")
            }
            Self::EmptyTrack(id) => write!(formatter, "music track {id} has no samples"),
            Self::CrossfadeTooLong {
                track,
                crossfade_samples,
                duration_samples,
            } => write!(
                formatter,
                "crossfade of {crossfade_samples} samples exceeds half of track {track} ({duration_samples} samples)"
            ),
            Self::EndFadeTooLong {
                end_fade_samples,
                program_samples,
            } => write!(
                formatter,
                "end fade of {end_fade_samples} samples exceeds the {program_samples}-sample program"
            ),
            Self::TooManyDuckIntervals { maximum } => {
                write!(formatter, "music bed exceeds {maximum} duck intervals")
            }
            Self::InvalidDuckInterval(range) => write!(
                formatter,
                "duck interval {}..{} is empty, unordered, or outside the program",
                range.start, range.end
            ),
            Self::TooManyEntries { maximum } => {
                write!(formatter, "music bed exceeds {maximum} placed entries")
            }
        }
    }
}

impl std::error::Error for MusicBedError {}
