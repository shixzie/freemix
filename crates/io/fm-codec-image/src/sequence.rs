//! Folder-backed numbered still-image sequence source.
//!
//! Discovery, timing and cache policy come from `fm-playback`; this module
//! adds the filesystem listing, bounded file reads, [`decode_still`] and
//! per-sequence dimension checks. Decoding is synchronous and on the calling
//! thread: [`ImageSequenceSource::prefetch_from`] fills the bounded window
//! ahead of playback and [`ImageSequenceSource::preload_all`] makes a stinger
//! fully resident before it is taken.

use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fm_frame::{CpuVideoFrame, MediaTiming, VideoDimensions};
use fm_playback::{
    CacheInsert, GapPolicy, ImageSequence, ImageSequenceError, MAX_FOLDER_ENTRIES, PrefetchBudget,
    PrefetchBudgetError, PrefetchCache, PrefetchStats, SequenceClock, SequenceClockError,
    SequenceEndMode, SequencePosition, prefetch_order,
};
use fm_types::{FrameRate, TimeBase};

use crate::{StillDecodeError, StillDecodeLimits, decode_still};

/// Configuration for one [`ImageSequenceSource`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageSequenceConfig {
    pub rate: FrameRate,
    pub end_mode: SequenceEndMode,
    pub gap_policy: GapPolicy,
    pub decode_limits: StillDecodeLimits,
    pub prefetch: PrefetchBudget,
    /// Timeline frames after the current one kept in the prefetch window.
    pub lookahead: u32,
    /// Directory entries read before failing; capped at
    /// [`MAX_FOLDER_ENTRIES`].
    pub max_directory_entries: usize,
}

impl ImageSequenceConfig {
    /// A configuration with default decode limits, a 512 MiB / 64-entry
    /// cache, an eight-frame lookahead and the folder entry limit.
    #[must_use]
    pub fn new(rate: FrameRate, end_mode: SequenceEndMode, gap_policy: GapPolicy) -> Self {
        Self {
            rate,
            end_mode,
            gap_policy,
            decode_limits: StillDecodeLimits::default(),
            prefetch: PrefetchBudget {
                max_bytes: 512 * 1024 * 1024,
                max_entries: 64,
            },
            lookahead: 8,
            max_directory_entries: MAX_FOLDER_ENTRIES,
        }
    }
}

#[derive(Debug)]
pub enum ImageSequenceSourceError {
    Directory {
        path: PathBuf,
        kind: ErrorKind,
    },
    TooManyDirectoryEntries {
        maximum: usize,
    },
    Discover(ImageSequenceError),
    Clock(SequenceClockError),
    Budget(PrefetchBudgetError),
    Read {
        name: String,
        kind: ErrorKind,
    },
    Decode {
        name: String,
        error: StillDecodeError,
    },
    EncodedFileTooLarge {
        name: String,
        maximum: usize,
    },
    DimensionMismatch {
        name: String,
        expected: VideoDimensions,
        actual: VideoDimensions,
    },
    /// [`ImageSequenceSource::preload_all`] cannot keep every file resident.
    PreloadExceedsBudget {
        files: usize,
        budget: PrefetchBudget,
    },
}

impl fmt::Display for ImageSequenceSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directory { path, kind } => {
                write!(formatter, "cannot list {}: {kind:?}", path.display())
            }
            Self::TooManyDirectoryEntries { maximum } => {
                write!(formatter, "sequence folder has more than {maximum} entries")
            }
            Self::Discover(error) => error.fmt(formatter),
            Self::Clock(error) => error.fmt(formatter),
            Self::Budget(error) => error.fmt(formatter),
            Self::Read { name, kind } => write!(formatter, "cannot read {name:?}: {kind:?}"),
            Self::Decode { name, error } => write!(formatter, "cannot decode {name:?}: {error}"),
            Self::EncodedFileTooLarge { name, maximum } => {
                write!(
                    formatter,
                    "{name:?} exceeds the {maximum}-byte encoded limit"
                )
            }
            Self::DimensionMismatch {
                name,
                expected,
                actual,
            } => write!(
                formatter,
                "{name:?} is {}x{}; the sequence is {}x{}",
                actual.width(),
                actual.height(),
                expected.width(),
                expected.height()
            ),
            Self::PreloadExceedsBudget { files, budget } => write!(
                formatter,
                "{files} sequence files do not fit the {}-byte, {}-entry preload budget",
                budget.max_bytes, budget.max_entries
            ),
        }
    }
}

impl std::error::Error for ImageSequenceSourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Discover(error) => Some(error),
            Self::Clock(error) => Some(error),
            Self::Budget(error) => Some(error),
            Self::Decode { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// One sequence sample.
#[derive(Clone, Debug, PartialEq)]
pub enum SequenceSample {
    BeforeStart,
    /// A [`SequenceEndMode::Once`] sequence has completed; nothing is shown.
    Ended,
    Frame {
        timeline_frame: u32,
        file_index: usize,
        /// Straight-alpha RGBA8 frame; its timing is the source's retained
        /// timing, and callers present it on their own tick.
        frame: Arc<CpuVideoFrame>,
        cache_hit: bool,
    },
}

/// Opaque/transparent pixel counts of a straight-alpha RGBA8 frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlphaCoverage {
    pub opaque: u64,
    pub transparent: u64,
    pub total: u64,
}

impl AlphaCoverage {
    #[must_use]
    pub const fn is_fully_opaque(self) -> bool {
        self.opaque == self.total
    }
}

/// Counts fully opaque and fully transparent pixels of an RGBA8 frame.
///
/// Returns `None` for non-RGBA8 payloads.
#[must_use]
pub fn alpha_coverage(frame: &CpuVideoFrame) -> Option<AlphaCoverage> {
    let payload = frame.payload();
    if payload.format() != fm_frame::PixelFormat::Rgba8 {
        return None;
    }
    let plane = payload.plane(0)?;
    let dimensions = payload.dimensions();
    let row_bytes = usize::try_from(dimensions.width()).ok()?.checked_mul(4)?;
    let mut coverage = AlphaCoverage {
        opaque: 0,
        transparent: 0,
        total: u64::from(dimensions.width()) * u64::from(dimensions.height()),
    };
    for row in plane
        .bytes()
        .chunks(plane.stride())
        .take(usize::try_from(dimensions.height()).ok()?)
    {
        for pixel in row.get(..row_bytes)?.as_chunks::<4>().0 {
            match pixel[3] {
                u8::MAX => coverage.opaque += 1,
                0 => coverage.transparent += 1,
                _ => {}
            }
        }
    }
    Some(coverage)
}

/// A numbered still-image sequence read from one folder.
#[derive(Debug)]
pub struct ImageSequenceSource {
    directory: PathBuf,
    sequence: ImageSequence,
    clock: SequenceClock,
    config: ImageSequenceConfig,
    timing: MediaTiming,
    cache: PrefetchCache<CpuVideoFrame>,
    dimensions: Option<VideoDimensions>,
    decodes: u64,
    pinned: bool,
}

impl ImageSequenceSource {
    /// Lists `directory`, discovers the sequence containing `seed`, and
    /// prepares an empty cache. Nothing is decoded yet.
    ///
    /// Only regular files with UTF-8 names are listed; symlinks are skipped.
    /// `timing` is attached to every decoded frame.
    ///
    /// # Errors
    ///
    /// Returns listing, discovery, clock or budget errors.
    pub fn open(
        directory: &Path,
        seed: &str,
        config: ImageSequenceConfig,
        timing: MediaTiming,
    ) -> Result<Self, ImageSequenceSourceError> {
        let names = list_regular_files(directory, config.max_directory_entries)?;
        let sequence = ImageSequence::discover(&names, seed, config.gap_policy)
            .map_err(ImageSequenceSourceError::Discover)?;
        let clock = SequenceClock::new(config.rate, sequence.frame_count(), config.end_mode)
            .map_err(ImageSequenceSourceError::Clock)?;
        let cache =
            PrefetchCache::new(config.prefetch).map_err(ImageSequenceSourceError::Budget)?;
        Ok(Self {
            directory: directory.to_owned(),
            sequence,
            clock,
            config,
            timing,
            cache,
            dimensions: None,
            decodes: 0,
            pinned: false,
        })
    }

    #[must_use]
    pub const fn sequence(&self) -> &ImageSequence {
        &self.sequence
    }

    #[must_use]
    pub const fn clock(&self) -> SequenceClock {
        self.clock
    }

    /// Dimensions shared by every decoded file, once one has been decoded.
    #[must_use]
    pub const fn dimensions(&self) -> Option<VideoDimensions> {
        self.dimensions
    }

    /// Total file decodes, including uncached ones.
    #[must_use]
    pub const fn decodes(&self) -> u64 {
        self.decodes
    }

    #[must_use]
    pub const fn cache_stats(&self) -> PrefetchStats {
        self.cache.stats()
    }

    #[must_use]
    pub const fn resident_bytes(&self) -> usize {
        self.cache.resident_bytes()
    }

    #[must_use]
    pub fn resident_files(&self) -> usize {
        self.cache.len()
    }

    /// Sample for output frame `output_frame` (from the sequence start) of an
    /// output at `output_rate`.
    ///
    /// # Errors
    ///
    /// Returns a read, decode or dimension error for the needed file.
    pub fn sample_output_frame(
        &mut self,
        output_frame: u64,
        output_rate: FrameRate,
    ) -> Result<SequenceSample, ImageSequenceSourceError> {
        let position = self
            .clock
            .position_at_output_frame(output_frame, output_rate);
        self.sample(position)
    }

    /// Sample at `ticks` of `time_base` after the sequence start.
    ///
    /// # Errors
    ///
    /// Returns a read, decode or dimension error for the needed file.
    pub fn sample_at_time(
        &mut self,
        ticks: i64,
        time_base: TimeBase,
    ) -> Result<SequenceSample, ImageSequenceSourceError> {
        let position = self.clock.position_at_time(ticks, time_base);
        self.sample(position)
    }

    fn sample(
        &mut self,
        position: SequencePosition,
    ) -> Result<SequenceSample, ImageSequenceSourceError> {
        let timeline_frame = match position {
            SequencePosition::BeforeStart => return Ok(SequenceSample::BeforeStart),
            SequencePosition::Ended => return Ok(SequenceSample::Ended),
            SequencePosition::Frame(frame) => frame,
        };
        let file_index = self.set_window(timeline_frame);
        if let Some(frame) = self.cache.get(file_index) {
            return Ok(SequenceSample::Frame {
                timeline_frame,
                file_index,
                frame,
                cache_hit: true,
            });
        }
        let frame = self.decode(file_index)?;
        let bytes = frame.payload().byte_len();
        self.cache.insert(file_index, Arc::clone(&frame), bytes);
        Ok(SequenceSample::Frame {
            timeline_frame,
            file_index,
            frame,
            cache_hit: false,
        })
    }

    /// Moves the prefetch window to `timeline_frame` and decodes missing
    /// window files, nearest first, until the window is resident or the
    /// budget refuses a file. Returns the number of files decoded.
    ///
    /// # Errors
    ///
    /// Returns a read, decode or dimension error; files decoded before the
    /// error stay resident.
    pub fn prefetch_from(
        &mut self,
        timeline_frame: u32,
    ) -> Result<usize, ImageSequenceSourceError> {
        if timeline_frame >= self.sequence.frame_count() {
            return Ok(0);
        }
        self.set_window(timeline_frame);
        let mut decoded = 0;
        for index in self.cache.missing() {
            let frame = self.decode(index)?;
            let bytes = frame.payload().byte_len();
            decoded += 1;
            if !matches!(
                self.cache.insert(index, frame, bytes),
                CacheInsert::Inserted { .. }
            ) {
                break;
            }
        }
        Ok(decoded)
    }

    /// Decodes every file and keeps all of them resident, as a stinger must be
    /// before it is taken. The prefetch window becomes the whole sequence and
    /// stays pinned (sampling never evicts) until [`Self::release_preload`].
    ///
    /// # Errors
    ///
    /// Returns [`ImageSequenceSourceError::PreloadExceedsBudget`] when every
    /// file cannot be resident at once (checked before decoding by entry count
    /// and after each decode by bytes), or a read, decode or dimension error.
    pub fn preload_all(&mut self) -> Result<(), ImageSequenceSourceError> {
        let files = self.sequence.files().len();
        let budget = self.cache.budget();
        if files > budget.max_entries {
            return Err(ImageSequenceSourceError::PreloadExceedsBudget { files, budget });
        }
        let order: Vec<usize> = (0..files).collect();
        self.pinned = false;
        self.cache.set_window(&order);
        for index in self.cache.missing() {
            let frame = self.decode(index)?;
            let bytes = frame.payload().byte_len();
            if !matches!(
                self.cache.insert(index, frame, bytes),
                CacheInsert::Inserted { evicted: 0 }
            ) {
                return Err(ImageSequenceSourceError::PreloadExceedsBudget { files, budget });
            }
        }
        self.pinned = true;
        Ok(())
    }

    /// Ends a [`Self::preload_all`] pin; the next sample or prefetch narrows
    /// the window back to the lookahead.
    pub fn release_preload(&mut self) {
        self.pinned = false;
    }

    /// True when every sequence file is resident.
    #[must_use]
    pub fn is_fully_resident(&self) -> bool {
        self.cache.len() == self.sequence.files().len()
    }

    /// Suggests a stinger cut point: the first timeline frame whose image is
    /// fully opaque, so switching the base below it is invisible.
    ///
    /// Resident files are reused; others are decoded without caching.
    ///
    /// # Errors
    ///
    /// Returns a read, decode or dimension error.
    pub fn suggest_cut_point(&mut self) -> Result<Option<u32>, ImageSequenceSourceError> {
        for timeline_frame in 0..self.sequence.frame_count() {
            let Some(index) = self.sequence.file_index(timeline_frame) else {
                continue;
            };
            let frame = match self.cache.peek(index) {
                Some(frame) => Arc::clone(frame),
                None => self.decode(index)?,
            };
            if alpha_coverage(&frame).is_some_and(AlphaCoverage::is_fully_opaque) {
                return Ok(Some(timeline_frame));
            }
        }
        Ok(None)
    }

    /// Moves the prefetch window to start at `timeline_frame` and returns the
    /// file shown there. A preloaded sequence keeps its whole-sequence window.
    fn set_window(&mut self, timeline_frame: u32) -> usize {
        let frames = self
            .clock
            .upcoming_frames(timeline_frame, self.config.lookahead);
        let order = prefetch_order(&self.sequence, &frames);
        if !self.pinned {
            self.cache.set_window(&order);
        }
        order[0]
    }

    fn decode(&mut self, index: usize) -> Result<Arc<CpuVideoFrame>, ImageSequenceSourceError> {
        let name = self.sequence.files()[index].name().to_owned();
        let limits = self.config.decode_limits;
        let encoded = read_bounded(&self.directory.join(&name), &name, limits.max_encoded_bytes)?;
        let decoded = decode_still(&encoded, self.timing, limits).map_err(|error| {
            ImageSequenceSourceError::Decode {
                name: name.clone(),
                error,
            }
        })?;
        self.decodes += 1;
        let actual = decoded.frame.payload().dimensions();
        match self.dimensions {
            Some(expected) if expected != actual => {
                return Err(ImageSequenceSourceError::DimensionMismatch {
                    name,
                    expected,
                    actual,
                });
            }
            Some(_) => {}
            None => self.dimensions = Some(actual),
        }
        Ok(Arc::new(decoded.frame))
    }
}

fn list_regular_files(
    directory: &Path,
    maximum: usize,
) -> Result<Vec<String>, ImageSequenceSourceError> {
    let maximum = maximum.min(MAX_FOLDER_ENTRIES);
    let directory_error = |error: std::io::Error| ImageSequenceSourceError::Directory {
        path: directory.to_owned(),
        kind: error.kind(),
    };
    let mut names = Vec::new();
    for (count, entry) in std::fs::read_dir(directory)
        .map_err(directory_error)?
        .enumerate()
    {
        if count >= maximum {
            return Err(ImageSequenceSourceError::TooManyDirectoryEntries { maximum });
        }
        let entry = entry.map_err(directory_error)?;
        if !entry.file_type().map_err(directory_error)?.is_file() {
            continue;
        }
        if let Ok(name) = entry.file_name().into_string() {
            names.push(name);
        }
    }
    Ok(names)
}

fn read_bounded(
    path: &Path,
    name: &str,
    maximum: usize,
) -> Result<Vec<u8>, ImageSequenceSourceError> {
    let read_error = |error: std::io::Error| ImageSequenceSourceError::Read {
        name: name.to_owned(),
        kind: error.kind(),
    };
    let file = File::open(path).map_err(read_error)?;
    let mut encoded = Vec::new();
    file.take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut encoded)
        .map_err(read_error)?;
    if encoded.len() > maximum {
        return Err(ImageSequenceSourceError::EncodedFileTooLarge {
            name: name.to_owned(),
            maximum,
        });
    }
    Ok(encoded)
}
