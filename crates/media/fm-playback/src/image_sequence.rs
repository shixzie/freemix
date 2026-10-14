//! Numbered image-sequence discovery, frame-accurate timing and a bounded,
//! memory-accounted prefetch cache.
//!
//! Like [`crate::ImageFolderListing`], nothing here touches the filesystem or
//! decodes pixels: callers supply directory names and decoded values.

use core::fmt;
use std::collections::BTreeMap;
use std::sync::Arc;

use fm_types::{FrameRate, TimeBase};

use crate::image_folder::{FolderListingError, ImageFolderListing, is_supported_image};

/// Maximum number of timeline frames in one sequence (including held gaps).
pub const MAX_SEQUENCE_FRAMES: u32 = 1 << 20;
/// Maximum digits in a frame number; longer runs cannot be sequence members.
pub const MAX_SEQUENCE_DIGITS: usize = 18;

/// A supported image name split around its frame number.
///
/// The frame number is the last ASCII digit run before the final `.`; for
/// `shot_v2_0042.matte.png` the prefix is `shot_v2_`, digits `0042`, infix
/// `.matte` and extension `png`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SequenceName<'a> {
    pub prefix: &'a str,
    pub digits: &'a str,
    pub infix: &'a str,
    pub extension: &'a str,
    pub number: u64,
}

/// Splits a supported still-image name around its frame number.
///
/// Returns `None` when the name has no supported extension, no digit run in
/// its stem, or a digit run longer than [`MAX_SEQUENCE_DIGITS`].
#[must_use]
pub fn parse_sequence_name(name: &str) -> Option<SequenceName<'_>> {
    if !is_supported_image(name) {
        return None;
    }
    let (stem, extension) = name.rsplit_once('.')?;
    let end = stem.rfind(|c: char| c.is_ascii_digit())? + 1;
    // A non-ASCII character may precede the digits, so measure the digit
    // run instead of stepping one byte past the preceding character.
    let start = stem[..end]
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .len();
    let digits = &stem[start..end];
    if digits.len() > MAX_SEQUENCE_DIGITS {
        return None;
    }
    Some(SequenceName {
        prefix: &stem[..start],
        digits,
        infix: &stem[end..],
        extension,
        number: digits.parse().ok()?,
    })
}

/// How missing frame numbers inside a sequence are treated.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GapPolicy {
    /// Any missing number between the first and last member is an error.
    Reject,
    /// The timeline spans every number from first to last; a missing number
    /// shows the nearest earlier present file.
    HoldPrevious,
    /// The timeline contains only present files, in number order.
    Skip,
}

/// Observed zero-padding of member frame numbers.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SequencePadding {
    /// Every digit run has this length (greater than one).
    Fixed(usize),
    /// No digit run has a leading zero.
    Unpadded,
    /// Padded and unpadded or differently padded runs are combined, for
    /// example `999.png` followed by `1000.png` after `001.png`.
    Mixed,
}

/// One file of a discovered sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequenceFile {
    name: String,
    number: u64,
}

impl SequenceFile {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImageSequenceError {
    Listing(FolderListingError),
    SeedNotFound(String),
    /// The seed is not a supported image with a bounded frame number.
    SeedNotNumbered(String),
    /// Two members encode the same number with different padding.
    DuplicateNumber {
        number: u64,
        first: String,
        second: String,
    },
    /// [`GapPolicy::Reject`] found missing numbers `first_missing..=last_missing`.
    Gap {
        first_missing: u64,
        last_missing: u64,
    },
    TooManyFrames {
        frames: u64,
        maximum: u32,
    },
}

impl fmt::Display for ImageSequenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Listing(error) => write!(formatter, "invalid sequence folder: {error}"),
            Self::SeedNotFound(name) => write!(formatter, "sequence file {name:?} is not listed"),
            Self::SeedNotNumbered(name) => {
                write!(
                    formatter,
                    "{name:?} is not a numbered supported still image"
                )
            }
            Self::DuplicateNumber {
                number,
                first,
                second,
            } => write!(
                formatter,
                "sequence files {first:?} and {second:?} both encode frame number {number}"
            ),
            Self::Gap {
                first_missing,
                last_missing,
            } => write!(
                formatter,
                "sequence is missing frame numbers {first_missing} through {last_missing}"
            ),
            Self::TooManyFrames { frames, maximum } => write!(
                formatter,
                "sequence spans {frames} frames; the limit is {maximum}"
            ),
        }
    }
}

impl std::error::Error for ImageSequenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Listing(error) => Some(error),
            _ => None,
        }
    }
}

/// A numbered still-image sequence discovered from one folder listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageSequence {
    files: Vec<SequenceFile>,
    gap_policy: GapPolicy,
    padding: SequencePadding,
    frame_count: u32,
    missing_numbers: u64,
    unrelated: usize,
}

impl ImageSequence {
    /// Discovers the sequence that `seed` belongs to.
    ///
    /// Members share the seed's prefix and infix exactly and its extension
    /// ASCII-case-insensitively. Members are ordered by numeric value, so
    /// padding variations (`9.png`, `10.png`; `0999.png`, `1000.png`) order
    /// correctly. The sequence starts at the lowest member number, not at the
    /// seed.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid listing, a missing or unnumbered seed,
    /// two members with the same numeric value, a gap under
    /// [`GapPolicy::Reject`], or more than [`MAX_SEQUENCE_FRAMES`] frames.
    pub fn discover<S: AsRef<str>>(
        names: &[S],
        seed: &str,
        gap_policy: GapPolicy,
    ) -> Result<Self, ImageSequenceError> {
        let listing = ImageFolderListing::from_names(names).map_err(ImageSequenceError::Listing)?;
        if !names.iter().any(|name| name.as_ref() == seed) {
            return Err(ImageSequenceError::SeedNotFound(seed.to_owned()));
        }
        let key = parse_sequence_name(seed)
            .ok_or_else(|| ImageSequenceError::SeedNotNumbered(seed.to_owned()))?;

        let mut members: BTreeMap<u64, (String, usize)> = BTreeMap::new();
        let mut unrelated = 0;
        let mut any_leading_zero = false;
        let mut lengths = Vec::new();
        for name in listing.images() {
            let Some(parsed) = parse_sequence_name(name).filter(|parsed| {
                parsed.prefix == key.prefix
                    && parsed.infix == key.infix
                    && parsed.extension.eq_ignore_ascii_case(key.extension)
            }) else {
                unrelated += 1;
                continue;
            };
            if let Some((first, _)) = members.get(&parsed.number) {
                return Err(ImageSequenceError::DuplicateNumber {
                    number: parsed.number,
                    first: first.clone(),
                    second: name.clone(),
                });
            }
            any_leading_zero |= parsed.digits.len() > 1 && parsed.digits.starts_with('0');
            lengths.push(parsed.digits.len());
            members.insert(parsed.number, (name.clone(), parsed.digits.len()));
        }

        let padding = if !any_leading_zero {
            SequencePadding::Unpadded
        } else if lengths.iter().all(|length| *length == lengths[0]) {
            SequencePadding::Fixed(lengths[0])
        } else {
            SequencePadding::Mixed
        };

        let files: Vec<SequenceFile> = members
            .into_iter()
            .map(|(number, (name, _))| SequenceFile { name, number })
            .collect();
        // The seed is a member, so `files` is nonempty.
        let first = files[0].number;
        let last = files[files.len() - 1].number;
        let span = last - first + 1;
        let present = u64::try_from(files.len()).unwrap_or(u64::MAX);
        let missing_numbers = span - present;
        if gap_policy == GapPolicy::Reject
            && let Some(window) = files
                .windows(2)
                .find(|window| window[1].number != window[0].number + 1)
        {
            return Err(ImageSequenceError::Gap {
                first_missing: window[0].number + 1,
                last_missing: window[1].number - 1,
            });
        }
        let frames = if gap_policy == GapPolicy::HoldPrevious {
            span
        } else {
            present
        };
        let frame_count = u32::try_from(frames)
            .ok()
            .filter(|frames| *frames <= MAX_SEQUENCE_FRAMES)
            .ok_or(ImageSequenceError::TooManyFrames {
                frames,
                maximum: MAX_SEQUENCE_FRAMES,
            })?;
        Ok(Self {
            files,
            gap_policy,
            padding,
            frame_count,
            missing_numbers,
            unrelated,
        })
    }

    /// Present files in ascending number order.
    #[must_use]
    pub fn files(&self) -> &[SequenceFile] {
        &self.files
    }

    #[must_use]
    pub const fn gap_policy(&self) -> GapPolicy {
        self.gap_policy
    }

    #[must_use]
    pub const fn padding(&self) -> SequencePadding {
        self.padding
    }

    /// Timeline frames; always at least one.
    #[must_use]
    pub const fn frame_count(&self) -> u32 {
        self.frame_count
    }

    /// Numbers between the first and last member with no file.
    #[must_use]
    pub const fn missing_numbers(&self) -> u64 {
        self.missing_numbers
    }

    /// Supported images in the listing that are not members.
    #[must_use]
    pub const fn unrelated_images(&self) -> usize {
        self.unrelated
    }

    /// Index into [`Self::files`] shown on timeline frame `frame`.
    #[must_use]
    pub fn file_index(&self, frame: u32) -> Option<usize> {
        if frame >= self.frame_count {
            return None;
        }
        let frame = usize::try_from(frame).ok()?;
        match self.gap_policy {
            GapPolicy::Reject | GapPolicy::Skip => Some(frame),
            GapPolicy::HoldPrevious => {
                let number = self.files[0].number + u64::try_from(frame).ok()?;
                Some(self.files.partition_point(|file| file.number <= number) - 1)
            }
        }
    }

    /// File shown on timeline frame `frame`.
    #[must_use]
    pub fn file(&self, frame: u32) -> Option<&SequenceFile> {
        self.file_index(frame).map(|index| &self.files[index])
    }
}

/// Behaviour after the last timeline frame.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SequenceEndMode {
    /// Play once; afterwards nothing is shown (a stinger's completion).
    Once,
    /// Wrap to frame zero.
    Loop,
    /// Keep showing the last frame.
    Hold,
}

/// Resolved sequence position for one presentation time.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SequencePosition {
    BeforeStart,
    Frame(u32),
    Ended,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SequenceClockError {
    EmptySequence,
}

impl fmt::Display for SequenceClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySequence => formatter.write_str("sequence clock needs at least one frame"),
        }
    }
}

impl std::error::Error for SequenceClockError {}

/// Exact mapping from media time to sequence frame at a rational rate.
///
/// Timeline frame `k` is shown for media times `t` (seconds from the
/// sequence start) with `k = ⌊t · rate⌋`. All arithmetic is exact integer
/// arithmetic, so fractional rates such as 30000/1001 never drift.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SequenceClock {
    rate: FrameRate,
    frame_count: u32,
    end_mode: SequenceEndMode,
}

impl SequenceClock {
    /// # Errors
    ///
    /// Returns [`SequenceClockError::EmptySequence`] for zero frames.
    pub const fn new(
        rate: FrameRate,
        frame_count: u32,
        end_mode: SequenceEndMode,
    ) -> Result<Self, SequenceClockError> {
        if frame_count == 0 {
            return Err(SequenceClockError::EmptySequence);
        }
        Ok(Self {
            rate,
            frame_count,
            end_mode,
        })
    }

    #[must_use]
    pub const fn rate(self) -> FrameRate {
        self.rate
    }

    #[must_use]
    pub const fn frame_count(self) -> u32 {
        self.frame_count
    }

    #[must_use]
    pub const fn end_mode(self) -> SequenceEndMode {
        self.end_mode
    }

    /// Position at `ticks` of `time_base` after the sequence start.
    #[must_use]
    pub fn position_at_time(self, ticks: i64, time_base: TimeBase) -> SequencePosition {
        let Ok(ticks) = u64::try_from(ticks) else {
            return SequencePosition::BeforeStart;
        };
        // k = ⌊ticks · tb_num · rate_num / (tb_den · rate_den)⌋
        let numerator = u128::from(time_base.numerator()) * u128::from(self.rate.numerator());
        let denominator = u128::from(time_base.denominator()) * u128::from(self.rate.denominator());
        self.resolve(mul_div_floor(ticks, numerator, denominator))
    }

    /// Position on output frame `output_frame` (counted from the sequence
    /// start) of an output running at `output_rate`.
    #[must_use]
    pub fn position_at_output_frame(
        self,
        output_frame: u64,
        output_rate: FrameRate,
    ) -> SequencePosition {
        // Output frame n starts at n · out_den / out_num seconds.
        let numerator = u128::from(output_rate.denominator()) * u128::from(self.rate.numerator());
        let denominator = u128::from(output_rate.numerator()) * u128::from(self.rate.denominator());
        self.resolve(mul_div_floor(output_frame, numerator, denominator))
    }

    /// Output frames until one pass completes: the first output frame at
    /// which a [`SequenceEndMode::Once`] clock reports
    /// [`SequencePosition::Ended`].
    #[must_use]
    pub fn output_frames_per_pass(self, output_rate: FrameRate) -> u128 {
        // Smallest n with ⌊n·out_den·rate_num/(out_num·rate_den)⌋ ≥ count.
        let numerator = u128::from(self.frame_count)
            * u128::from(output_rate.numerator())
            * u128::from(self.rate.denominator());
        let denominator = u128::from(output_rate.denominator()) * u128::from(self.rate.numerator());
        numerator.div_ceil(denominator)
    }

    fn resolve(self, media_frame: u128) -> SequencePosition {
        let count = u128::from(self.frame_count);
        if media_frame < count {
            return SequencePosition::Frame(u32::try_from(media_frame).unwrap_or(u32::MAX));
        }
        match self.end_mode {
            SequenceEndMode::Once => SequencePosition::Ended,
            SequenceEndMode::Hold => SequencePosition::Frame(self.frame_count - 1),
            SequenceEndMode::Loop => {
                SequencePosition::Frame(u32::try_from(media_frame % count).unwrap_or(0))
            }
        }
    }

    /// Timeline frames from `current` through `lookahead` following frames,
    /// in display order, honouring the end mode (wrapping only for
    /// [`SequenceEndMode::Loop`]) and without repeats.
    #[must_use]
    pub fn upcoming_frames(self, current: u32, lookahead: u32) -> Vec<u32> {
        if current >= self.frame_count {
            return Vec::new();
        }
        let span = match self.end_mode {
            SequenceEndMode::Loop => lookahead.min(self.frame_count - 1),
            SequenceEndMode::Once | SequenceEndMode::Hold => {
                lookahead.min(self.frame_count - 1 - current)
            }
        };
        (0..=span)
            .map(|offset| {
                let frame = u64::from(current) + u64::from(offset);
                u32::try_from(frame % u64::from(self.frame_count)).unwrap_or(0)
            })
            .collect()
    }
}

/// `⌊value · numerator / denominator⌋`, saturating; `value < 2^64`,
/// `numerator, denominator ≤ 2^64`, `denominator > 0`.
fn mul_div_floor(value: u64, numerator: u128, denominator: u128) -> u128 {
    let value = u128::from(value);
    let quotient = value / denominator;
    let remainder = value % denominator;
    quotient
        .saturating_mul(numerator)
        .saturating_add(remainder * numerator / denominator)
}

/// File indices to keep resident for timeline `frames`, nearest first and
/// without repeats (held gaps map several frames to one file).
#[must_use]
pub fn prefetch_order(sequence: &ImageSequence, frames: &[u32]) -> Vec<usize> {
    let mut order: Vec<usize> = Vec::with_capacity(frames.len());
    for frame in frames {
        if let Some(index) = sequence.file_index(*frame)
            && !order.contains(&index)
        {
            order.push(index);
        }
    }
    order
}

/// Resource bounds for a [`PrefetchCache`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PrefetchBudget {
    pub max_bytes: usize,
    pub max_entries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrefetchBudgetError {
    ZeroBytes,
    ZeroEntries,
}

impl fmt::Display for PrefetchBudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ZeroBytes => "prefetch byte budget must be nonzero",
            Self::ZeroEntries => "prefetch entry budget must be nonzero",
        })
    }
}

impl std::error::Error for PrefetchBudgetError {}

/// Why an insert left the cache unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheRejection {
    /// One value exceeds the whole byte budget.
    LargerThanBudget { bytes: usize, maximum: usize },
    /// Making room would evict a value needed at least as soon.
    WouldEvictNearer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheInsert {
    Inserted { evicted: usize },
    AlreadyResident,
    Rejected(CacheRejection),
}

/// Cumulative cache counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PrefetchStats {
    pub hits: u64,
    pub misses: u64,
    pub inserts: u64,
    pub evictions: u64,
    pub rejections: u64,
    pub peak_resident_bytes: usize,
}

#[derive(Debug)]
struct CacheEntry<T> {
    value: Arc<T>,
    bytes: usize,
}

/// A bounded cache of decoded sequence files keyed by file index.
///
/// Callers declare the current prefetch window with [`Self::set_window`];
/// residents outside the window are evicted immediately, and an insert evicts
/// the furthest-ahead residents only when the new value is needed sooner. The
/// resident byte and entry totals never exceed the budget.
#[derive(Debug)]
pub struct PrefetchCache<T> {
    budget: PrefetchBudget,
    entries: BTreeMap<usize, CacheEntry<T>>,
    window: BTreeMap<usize, usize>,
    window_order: Vec<usize>,
    resident_bytes: usize,
    stats: PrefetchStats,
}

impl<T> PrefetchCache<T> {
    /// # Errors
    ///
    /// Returns an error when either budget dimension is zero.
    pub fn new(budget: PrefetchBudget) -> Result<Self, PrefetchBudgetError> {
        if budget.max_bytes == 0 {
            return Err(PrefetchBudgetError::ZeroBytes);
        }
        if budget.max_entries == 0 {
            return Err(PrefetchBudgetError::ZeroEntries);
        }
        Ok(Self {
            budget,
            entries: BTreeMap::new(),
            window: BTreeMap::new(),
            window_order: Vec::new(),
            resident_bytes: 0,
            stats: PrefetchStats::default(),
        })
    }

    #[must_use]
    pub const fn budget(&self) -> PrefetchBudget {
        self.budget
    }

    /// Replaces the prefetch window (nearest first) and evicts every resident
    /// outside it. Returns the number evicted.
    pub fn set_window(&mut self, order: &[usize]) -> usize {
        self.window.clear();
        self.window_order.clear();
        for (rank, index) in order.iter().enumerate() {
            if !self.window.contains_key(index) {
                self.window.insert(*index, rank);
                self.window_order.push(*index);
            }
        }
        let outside: Vec<usize> = self
            .entries
            .keys()
            .filter(|index| !self.window.contains_key(index))
            .copied()
            .collect();
        for index in &outside {
            self.evict(*index);
        }
        outside.len()
    }

    /// Window indices that are not resident, nearest first.
    #[must_use]
    pub fn missing(&self) -> Vec<usize> {
        self.window_order
            .iter()
            .filter(|index| !self.entries.contains_key(index))
            .copied()
            .collect()
    }

    /// Returns a resident value and counts a hit or miss.
    pub fn get(&mut self, index: usize) -> Option<Arc<T>> {
        let value = self
            .entries
            .get(&index)
            .map(|entry| Arc::clone(&entry.value));
        if value.is_some() {
            self.stats.hits += 1;
        } else {
            self.stats.misses += 1;
        }
        value
    }

    /// Returns a resident value without counting.
    #[must_use]
    pub fn peek(&self, index: usize) -> Option<&Arc<T>> {
        self.entries.get(&index).map(|entry| &entry.value)
    }

    #[must_use]
    pub fn contains(&self, index: usize) -> bool {
        self.entries.contains_key(&index)
    }

    /// Inserts `value` accounted as `bytes`.
    pub fn insert(&mut self, index: usize, value: Arc<T>, bytes: usize) -> CacheInsert {
        if self.entries.contains_key(&index) {
            return CacheInsert::AlreadyResident;
        }
        if bytes > self.budget.max_bytes {
            self.stats.rejections += 1;
            return CacheInsert::Rejected(CacheRejection::LargerThanBudget {
                bytes,
                maximum: self.budget.max_bytes,
            });
        }
        let rank = self.rank(index);
        let mut victims: Vec<(usize, usize)> = self
            .entries
            .keys()
            .map(|resident| (self.rank(*resident), *resident))
            .collect();
        // Furthest first; ties by larger index.
        victims.sort_unstable_by(|left, right| right.cmp(left));
        let mut bytes_after = self.resident_bytes + bytes;
        let mut entries_after = self.entries.len() + 1;
        let mut chosen = Vec::new();
        for (victim_rank, victim) in victims {
            if bytes_after <= self.budget.max_bytes && entries_after <= self.budget.max_entries {
                break;
            }
            if victim_rank <= rank {
                self.stats.rejections += 1;
                return CacheInsert::Rejected(CacheRejection::WouldEvictNearer);
            }
            bytes_after -= self.entries[&victim].bytes;
            entries_after -= 1;
            chosen.push(victim);
        }
        for victim in &chosen {
            self.evict(*victim);
        }
        self.entries.insert(index, CacheEntry { value, bytes });
        self.resident_bytes += bytes;
        self.stats.inserts += 1;
        self.stats.peak_resident_bytes = self.stats.peak_resident_bytes.max(self.resident_bytes);
        CacheInsert::Inserted {
            evicted: chosen.len(),
        }
    }

    fn rank(&self, index: usize) -> usize {
        self.window.get(&index).copied().unwrap_or(usize::MAX)
    }

    fn evict(&mut self, index: usize) {
        if let Some(entry) = self.entries.remove(&index) {
            self.resident_bytes -= entry.bytes;
            self.stats.evictions += 1;
        }
    }

    /// Drops every resident value; counters are kept.
    pub fn clear(&mut self) {
        let indices: Vec<usize> = self.entries.keys().copied().collect();
        for index in indices {
            self.evict(index);
        }
    }

    #[must_use]
    pub const fn resident_bytes(&self) -> usize {
        self.resident_bytes
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub const fn stats(&self) -> PrefetchStats {
        self.stats
    }
}
