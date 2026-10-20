//! Deterministic photo slideshow engine.
//!
//! A [`Slideshow`] sequences still-image entries (stable [`SlideId`] plus a
//! filename) with exact per-slide durations in output frames, ordered or
//! seeded-random play order, cut or crossfade transitions, and Ken Burns
//! pan/zoom expressed as an exact rational crop rectangle per output frame.
//!
//! The engine never decodes images, reads the filesystem, or consults a wall
//! clock or ambient randomness. The caller drives it with exactly one
//! [`Slideshow::tick`] per output frame; each tick returns the frame to
//! present and then advances.
//!
//! # Timeline
//!
//! Each slide is held alone for its duration `D`. With a crossfade of `T`
//! frames, a transition of exactly `T` frames follows each hold in which the
//! outgoing and incoming slides are both visible; incoming weight for
//! transition step `k` (`0..T`) is `floor((k + 1) * 10000 / (T + 1))` basis
//! points, so neither endpoint duplicates a pure hold frame. A cut has no
//! transition frames.
//!
//! # Ken Burns
//!
//! A slide's motion runs over a fixed span of `T + D + T` slide-local frames:
//! local frame `0` is the first incoming-transition frame, `T` is the first
//! hold frame, and `T + D` is the first outgoing-transition frame. The very
//! first slide of a show starts at local frame `T`. Local frames past the span
//! (an end-of-show hold) clamp to the end rectangle. The span depends only on
//! configuration, so later list edits never alter motion already on air.
//!
//! # Live edits
//!
//! Insert, remove, and move are applied immediately to the authored list, but
//! the next slide is resolved lazily when the current hold ends. Slides that
//! are on air (held, outgoing, or incoming) cannot be removed, so an edit can
//! never disturb the displayed or transitioning slide.
//!
//! [`Slideshow::sync`] replaces the whole authored list at once (a rescanned
//! folder): slides that left the list but are still on air stay until they
//! go off air, then leave.
//!
//! # Navigation
//!
//! [`Slideshow::jump`] moves to any slide on the next tick through the
//! configured transition, from a hold or from the middle of a transition:
//! the slide shown so far becomes the outgoing slide and keeps its Ken
//! Burns motion where it was. With `auto_advance` off a slide holds until
//! the caller jumps.
//!
//! In random order each cycle is a seeded permutation of the authored list at
//! the moment the cycle starts. Slides inserted mid-cycle are appended to the
//! remaining cycle order unless already played this cycle; removed slides
//! leave it. No slide repeats within a cycle, and when a new cycle would start
//! with the slide just shown (and more than one slide exists) its first entry
//! is swapped with its last to avoid a back-to-back repeat.

use core::fmt;
use std::collections::{BTreeSet, VecDeque};

/// Maximum number of slides in one slideshow.
pub const MAX_SLIDES: usize = 65_536;
/// Maximum per-slide hold duration in output frames.
pub const MAX_SLIDE_DURATION_FRAMES: u64 = 1 << 32;
/// Maximum crossfade length in output frames.
pub const MAX_SLIDE_TRANSITION_FRAMES: u32 = 3_600;
/// Maximum source image width or height in pixels.
pub const MAX_SLIDE_SOURCE_DIMENSION: u32 = 65_536;
/// Maximum filename length in bytes.
pub const MAX_SLIDE_FILENAME_BYTES: usize = 1_024;
/// Maximum number of entries returned by [`Slideshow::browser_page`].
pub const MAX_SLIDE_BROWSER_PAGE: usize = 1_024;
/// Default slide count above which thumbnails are suppressed.
pub const DEFAULT_LARGE_SET_THRESHOLD: usize = 500;
/// Full incoming weight in basis points.
pub const FULL_MIX_BASIS_POINTS: u16 = 10_000;

/// Stable identity of one slide, independent of its list position.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SlideId(u128);

impl SlideId {
    #[must_use]
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u128 {
        self.0
    }
}

impl fmt::Display for SlideId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// An integer pixel rectangle inside a source image.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn fits(self, source_width: u32, source_height: u32) -> bool {
        self.width > 0
            && self.height > 0
            && u64::from(self.x) + u64::from(self.width) <= u64::from(source_width)
            && u64::from(self.y) + u64::from(self.height) <= u64::from(source_height)
    }
}

/// Ken Burns motion from a start crop to an end crop.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct KenBurns {
    pub start: PixelRect,
    pub end: PixelRect,
}

/// An exact rational crop rectangle: every coordinate is `numerator /
/// denominator` source pixels. The fraction is fully reduced.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CropRect {
    x: u64,
    y: u64,
    width: u64,
    height: u64,
    denominator: u64,
}

impl CropRect {
    #[must_use]
    pub const fn x_numerator(self) -> u64 {
        self.x
    }

    #[must_use]
    pub const fn y_numerator(self) -> u64 {
        self.y
    }

    #[must_use]
    pub const fn width_numerator(self) -> u64 {
        self.width
    }

    #[must_use]
    pub const fn height_numerator(self) -> u64 {
        self.height
    }

    #[must_use]
    pub const fn denominator(self) -> u64 {
        self.denominator
    }

    /// Returns the rectangle when every coordinate is a whole pixel.
    #[must_use]
    pub fn as_pixel_rect(self) -> Option<PixelRect> {
        if self.denominator != 1 {
            return None;
        }
        Some(PixelRect::new(
            u32::try_from(self.x).ok()?,
            u32::try_from(self.y).ok()?,
            u32::try_from(self.width).ok()?,
            u32::try_from(self.height).ok()?,
        ))
    }
}

/// One still-image slide.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Slide {
    id: SlideId,
    filename: String,
    source_width: u32,
    source_height: u32,
    duration: Option<u64>,
    ken_burns: Option<KenBurns>,
}

impl Slide {
    #[must_use]
    pub fn new(
        id: SlideId,
        filename: impl Into<String>,
        source_width: u32,
        source_height: u32,
    ) -> Self {
        Self {
            id,
            filename: filename.into(),
            source_width,
            source_height,
            duration: None,
            ken_burns: None,
        }
    }

    /// Overrides the configured default hold duration for this slide.
    #[must_use]
    pub const fn with_duration(mut self, frames: u64) -> Self {
        self.duration = Some(frames);
        self
    }

    #[must_use]
    pub const fn with_ken_burns(mut self, motion: KenBurns) -> Self {
        self.ken_burns = Some(motion);
        self
    }

    #[must_use]
    pub const fn id(&self) -> SlideId {
        self.id
    }

    #[must_use]
    pub fn filename(&self) -> &str {
        &self.filename
    }

    #[must_use]
    pub const fn source_size(&self) -> (u32, u32) {
        (self.source_width, self.source_height)
    }

    #[must_use]
    pub const fn duration(&self) -> Option<u64> {
        self.duration
    }

    #[must_use]
    pub const fn ken_burns(&self) -> Option<KenBurns> {
        self.ken_burns
    }

    fn validate(&self) -> Result<(), SlideshowError> {
        let id = self.id;
        if self.filename.is_empty()
            || self.filename.len() > MAX_SLIDE_FILENAME_BYTES
            || self.filename.contains('\0')
        {
            return Err(SlideshowError::InvalidFilename(id));
        }
        if self.source_width == 0
            || self.source_height == 0
            || self.source_width > MAX_SLIDE_SOURCE_DIMENSION
            || self.source_height > MAX_SLIDE_SOURCE_DIMENSION
        {
            return Err(SlideshowError::InvalidSourceSize(id));
        }
        if let Some(duration) = self.duration {
            validate_duration(duration).map_err(|()| SlideshowError::InvalidDuration(id))?;
        }
        if let Some(motion) = self.ken_burns {
            for (edge, rect) in [(RectEdge::Start, motion.start), (RectEdge::End, motion.end)] {
                if !rect.fits(self.source_width, self.source_height) {
                    return Err(SlideshowError::CropOutOfBounds { slide: id, edge });
                }
            }
        }
        Ok(())
    }
}

fn validate_duration(frames: u64) -> Result<(), ()> {
    if frames == 0 || frames > MAX_SLIDE_DURATION_FRAMES {
        Err(())
    } else {
        Ok(())
    }
}

/// Transition between consecutive slides.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SlideTransition {
    Cut,
    /// A crossfade of exactly `frames` mixed output frames.
    Crossfade {
        frames: u32,
    },
}

impl SlideTransition {
    const fn frames(self) -> u64 {
        match self {
            Self::Cut => 0,
            Self::Crossfade { frames } => frames as u64,
        }
    }
}

/// Play order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SlideOrder {
    Ordered,
    /// Seeded permutation per cycle, deterministic for (seed, cycle).
    Random {
        seed: u64,
    },
}

/// Slideshow configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlideshowConfig {
    pub default_duration_frames: u64,
    pub transition: SlideTransition,
    pub order: SlideOrder,
    /// Wrap to a new cycle after the last slide instead of holding it.
    pub loop_show: bool,
    /// Slide counts strictly above this are a large set.
    pub large_set_threshold: usize,
    /// Suppress thumbnails regardless of the slide count.
    pub suppress_thumbnails: bool,
    /// Move on when a hold ends; off, a slide holds until [`Slideshow::jump`].
    pub auto_advance: bool,
}

impl SlideshowConfig {
    #[must_use]
    pub const fn new(default_duration_frames: u64) -> Self {
        Self {
            default_duration_frames,
            transition: SlideTransition::Cut,
            order: SlideOrder::Ordered,
            loop_show: false,
            large_set_threshold: DEFAULT_LARGE_SET_THRESHOLD,
            suppress_thumbnails: false,
            auto_advance: true,
        }
    }

    fn validate(self) -> Result<(), SlideshowError> {
        validate_duration(self.default_duration_frames)
            .map_err(|()| SlideshowError::InvalidDefaultDuration)?;
        if let SlideTransition::Crossfade { frames } = self.transition
            && (frames == 0 || frames > MAX_SLIDE_TRANSITION_FRAMES)
        {
            return Err(SlideshowError::InvalidTransition { frames });
        }
        if self.large_set_threshold == 0 {
            return Err(SlideshowError::ZeroLargeSetThreshold);
        }
        Ok(())
    }
}

/// Which Ken Burns rectangle failed validation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RectEdge {
    Start,
    End,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SlideshowError {
    Empty,
    TooManySlides { count: usize, max: usize },
    DuplicateSlide(SlideId),
    InvalidFilename(SlideId),
    InvalidSourceSize(SlideId),
    InvalidDuration(SlideId),
    CropOutOfBounds { slide: SlideId, edge: RectEdge },
    InvalidDefaultDuration,
    InvalidTransition { frames: u32 },
    ZeroLargeSetThreshold,
}

impl fmt::Display for SlideshowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("a slideshow must contain at least one slide"),
            Self::TooManySlides { count, max } => {
                write!(
                    formatter,
                    "slideshow has {count} slides; the limit is {max}"
                )
            }
            Self::DuplicateSlide(id) => write!(formatter, "slide {id} is duplicated"),
            Self::InvalidFilename(id) => write!(
                formatter,
                "slide {id} filename must be 1..={MAX_SLIDE_FILENAME_BYTES} bytes without NUL"
            ),
            Self::InvalidSourceSize(id) => write!(
                formatter,
                "slide {id} source size must be 1..={MAX_SLIDE_SOURCE_DIMENSION} per axis"
            ),
            Self::InvalidDuration(id) => write!(
                formatter,
                "slide {id} duration must be 1..={MAX_SLIDE_DURATION_FRAMES} frames"
            ),
            Self::CropOutOfBounds { slide, edge } => write!(
                formatter,
                "slide {slide} Ken Burns {edge:?} rectangle is empty or outside the source"
            ),
            Self::InvalidDefaultDuration => write!(
                formatter,
                "default slide duration must be 1..={MAX_SLIDE_DURATION_FRAMES} frames"
            ),
            Self::InvalidTransition { frames } => write!(
                formatter,
                "crossfade of {frames} frames is outside 1..={MAX_SLIDE_TRANSITION_FRAMES}"
            ),
            Self::ZeroLargeSetThreshold => {
                formatter.write_str("large-set threshold must be at least 1")
            }
        }
    }
}

impl std::error::Error for SlideshowError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SlideshowEditError {
    Invalid(SlideshowError),
    DuplicateSlide(SlideId),
    UnknownSlide(SlideId),
    /// The slide is held, outgoing, or incoming and cannot be removed.
    OnAir(SlideId),
    IndexOutOfRange {
        index: usize,
        len: usize,
    },
    TooManySlides {
        max: usize,
    },
}

impl fmt::Display for SlideshowEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => error.fmt(formatter),
            Self::DuplicateSlide(id) => write!(formatter, "slide {id} already exists"),
            Self::UnknownSlide(id) => write!(formatter, "slide {id} does not exist"),
            Self::OnAir(id) => write!(formatter, "slide {id} is on air and cannot be removed"),
            Self::IndexOutOfRange { index, len } => {
                write!(formatter, "index {index} is outside a list of {len} slides")
            }
            Self::TooManySlides { max } => {
                write!(formatter, "slideshow is full; the limit is {max} slides")
            }
        }
    }
}

impl std::error::Error for SlideshowEditError {}

/// One visible slide in an output frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlideView {
    pub id: SlideId,
    /// Slide-local Ken Burns frame (before clamping to the motion span).
    pub local_frame: u64,
    pub crop: CropRect,
}

/// The output for one tick.
///
/// Hold frames have no `outgoing` slide and full incoming weight. Transition
/// frames carry both slides and the incoming weight in basis points.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlideshowFrame {
    /// Zero-based output frame counter since construction.
    pub frame: u64,
    pub cycle: u64,
    pub outgoing: Option<SlideView>,
    pub incoming: SlideView,
    pub mix_basis_points: u16,
    /// True when holding the final slide of a non-looping show past its
    /// duration.
    pub at_end: bool,
}

/// A browser/list label for one slide.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlideBrowserEntry {
    pub id: SlideId,
    pub filename: String,
    /// False when thumbnails are suppressed; the filename is then the only
    /// label.
    pub show_thumbnail: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OnAir {
    id: SlideId,
    duration: u64,
    source_width: u32,
    source_height: u32,
    ken_burns: Option<KenBurns>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Phase {
    Hold {
        slide: OnAir,
        elapsed: u64,
    },
    Transition {
        outgoing: OnAir,
        incoming: OnAir,
        step: u64,
        /// The outgoing slide's local frame at the first transition step.
        outgoing_base: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NextPlan {
    id: SlideId,
    new_cycle: bool,
    /// Remaining random order after `id` is taken (random mode only).
    pending: VecDeque<SlideId>,
}

/// Deterministic slideshow transport. See the module documentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Slideshow {
    slides: Vec<Slide>,
    config: SlideshowConfig,
    phase: Phase,
    cycle: u64,
    frame: u64,
    pending: VecDeque<SlideId>,
    played: BTreeSet<SlideId>,
    /// Slides [`Self::sync`] dropped while they were on air; each leaves
    /// the list once it is off air.
    retiring: BTreeSet<SlideId>,
}

impl Slideshow {
    /// Validates configuration and slides and cues the first slide.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, an empty or oversized list,
    /// duplicate IDs, or an invalid slide.
    pub fn new(slides: Vec<Slide>, config: SlideshowConfig) -> Result<Self, SlideshowError> {
        config.validate()?;
        if slides.is_empty() {
            return Err(SlideshowError::Empty);
        }
        if slides.len() > MAX_SLIDES {
            return Err(SlideshowError::TooManySlides {
                count: slides.len(),
                max: MAX_SLIDES,
            });
        }
        let mut seen = BTreeSet::new();
        for slide in &slides {
            if !seen.insert(slide.id) {
                return Err(SlideshowError::DuplicateSlide(slide.id));
            }
            slide.validate()?;
        }
        let mut pending = match config.order {
            SlideOrder::Ordered => VecDeque::new(),
            SlideOrder::Random { seed } => random_cycle(&slides, seed, 0, None),
        };
        let first_id = match config.order {
            SlideOrder::Ordered => slides[0].id,
            SlideOrder::Random { .. } => pending.pop_front().unwrap_or(slides[0].id),
        };
        let first_index = slides
            .iter()
            .position(|slide| slide.id == first_id)
            .unwrap_or(0);
        let first = on_air(&slides[first_index], config);
        let mut played = BTreeSet::new();
        played.insert(first_id);
        Ok(Self {
            slides,
            config,
            phase: Phase::Hold {
                slide: first,
                elapsed: 0,
            },
            cycle: 0,
            frame: 0,
            pending,
            played,
            retiring: BTreeSet::new(),
        })
    }

    /// Like [`Self::new`], but holds slide `first` from its first frame
    /// (for example to keep the slide on air when the settings change). In
    /// random order the rest of the first cycle is a seeded permutation of
    /// the other slides.
    ///
    /// # Errors
    ///
    /// Returns every error of [`Self::new`], and
    /// [`SlideshowError::Empty`] when `first` is not listed.
    pub fn new_at(
        slides: Vec<Slide>,
        config: SlideshowConfig,
        first: SlideId,
    ) -> Result<Self, SlideshowError> {
        let mut show = Self::new(slides, config)?;
        let index = show.position(first).ok_or(SlideshowError::Empty)?;
        show.phase = Phase::Hold {
            slide: on_air(&show.slides[index], config),
            elapsed: 0,
        };
        show.played.clear();
        show.played.insert(first);
        if let SlideOrder::Random { seed } = config.order {
            let mut pending = random_cycle(&show.slides, seed, 0, None);
            pending.retain(|id| *id != first);
            show.pending = pending;
        }
        Ok(show)
    }

    #[must_use]
    pub fn slides(&self) -> &[Slide] {
        &self.slides
    }

    #[must_use]
    pub const fn config(&self) -> SlideshowConfig {
        self.config
    }

    #[must_use]
    pub const fn cycle(&self) -> u64 {
        self.cycle
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.slides.len()
    }

    /// Always false: on-air slides cannot be removed, so the list never
    /// empties.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slides.is_empty()
    }

    /// Slides currently visible: the held slide, or outgoing then incoming.
    #[must_use]
    pub fn on_air(&self) -> Vec<SlideId> {
        match &self.phase {
            Phase::Hold { slide, .. } => vec![slide.id],
            Phase::Transition {
                outgoing, incoming, ..
            } => vec![outgoing.id, incoming.id],
        }
    }

    /// The slide that would follow the current hold if the list were not
    /// edited first. `None` during a transition or at the end of a
    /// non-looping show.
    #[must_use]
    pub fn peek_next(&self) -> Option<SlideId> {
        match &self.phase {
            Phase::Hold { slide, .. } => self.plan_next(slide.id).map(|plan| plan.id),
            Phase::Transition { .. } => None,
        }
    }

    /// True when the list is above the large-set threshold.
    #[must_use]
    pub fn is_large_set(&self) -> bool {
        self.slides.len() > self.config.large_set_threshold
    }

    /// True when thumbnails are suppressed explicitly or by large-set mode;
    /// browser entries then fall back to filename labels.
    #[must_use]
    pub fn thumbnails_suppressed(&self) -> bool {
        self.config.suppress_thumbnails || self.is_large_set()
    }

    /// A bounded page of browser entries in authored order.
    ///
    /// # Errors
    ///
    /// Returns [`SlideshowEditError::IndexOutOfRange`] when `offset` exceeds
    /// the list length. `limit` is clamped to [`MAX_SLIDE_BROWSER_PAGE`].
    pub fn browser_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<SlideBrowserEntry>, SlideshowEditError> {
        if offset > self.slides.len() {
            return Err(SlideshowEditError::IndexOutOfRange {
                index: offset,
                len: self.slides.len(),
            });
        }
        let show_thumbnail = !self.thumbnails_suppressed();
        Ok(self.slides[offset..]
            .iter()
            .take(limit.min(MAX_SLIDE_BROWSER_PAGE))
            .map(|slide| SlideBrowserEntry {
                id: slide.id,
                filename: slide.filename.clone(),
                show_thumbnail,
            })
            .collect())
    }

    /// Inserts a slide at `index` in authored order.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid slide, a duplicate ID, an index past
    /// the end, or a full list.
    pub fn insert(&mut self, index: usize, slide: Slide) -> Result<(), SlideshowEditError> {
        if index > self.slides.len() {
            return Err(SlideshowEditError::IndexOutOfRange {
                index,
                len: self.slides.len(),
            });
        }
        if self.slides.len() >= MAX_SLIDES {
            return Err(SlideshowEditError::TooManySlides { max: MAX_SLIDES });
        }
        if self.position(slide.id).is_some() {
            return Err(SlideshowEditError::DuplicateSlide(slide.id));
        }
        slide.validate().map_err(SlideshowEditError::Invalid)?;
        let id = slide.id;
        self.slides.insert(index, slide);
        if matches!(self.config.order, SlideOrder::Random { .. }) && !self.played.contains(&id) {
            self.pending.push_back(id);
        }
        Ok(())
    }

    /// Removes a slide that is not on air.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown or on-air slide.
    pub fn remove(&mut self, id: SlideId) -> Result<Slide, SlideshowEditError> {
        let index = self
            .position(id)
            .ok_or(SlideshowEditError::UnknownSlide(id))?;
        if self.on_air().contains(&id) {
            return Err(SlideshowEditError::OnAir(id));
        }
        self.pending.retain(|pending| *pending != id);
        Ok(self.slides.remove(index))
    }

    /// Moves a slide to `index` in authored order (index after removal).
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown slide or an out-of-range index.
    pub fn move_slide(&mut self, id: SlideId, index: usize) -> Result<(), SlideshowEditError> {
        let from = self
            .position(id)
            .ok_or(SlideshowEditError::UnknownSlide(id))?;
        if index >= self.slides.len() {
            return Err(SlideshowEditError::IndexOutOfRange {
                index,
                len: self.slides.len(),
            });
        }
        let slide = self.slides.remove(from);
        self.slides.insert(index, slide);
        Ok(())
    }

    /// Returns the frame to present now and advances one output frame.
    pub fn tick(&mut self) -> SlideshowFrame {
        let frame = self.render();
        self.advance();
        self.frame = self.frame.saturating_add(1);
        frame
    }

    fn transition_frames(&self) -> u64 {
        self.config.transition.frames()
    }

    fn render(&self) -> SlideshowFrame {
        let t = self.transition_frames();
        match &self.phase {
            Phase::Hold { slide, elapsed } => SlideshowFrame {
                frame: self.frame,
                cycle: self.cycle,
                outgoing: None,
                incoming: view(slide, t + elapsed, t),
                mix_basis_points: FULL_MIX_BASIS_POINTS,
                at_end: self.config.auto_advance && *elapsed >= slide.duration,
            },
            Phase::Transition {
                outgoing,
                incoming,
                step,
                outgoing_base,
            } => {
                let mix = (step + 1) * u64::from(FULL_MIX_BASIS_POINTS) / (t + 1);
                SlideshowFrame {
                    frame: self.frame,
                    cycle: self.cycle,
                    outgoing: Some(view(outgoing, outgoing_base + step, t)),
                    incoming: view(incoming, *step, t),
                    mix_basis_points: u16::try_from(mix).unwrap_or(FULL_MIX_BASIS_POINTS),
                    at_end: false,
                }
            }
        }
    }

    fn advance(&mut self) {
        let t = self.transition_frames();
        let phase = match self.phase.clone() {
            Phase::Hold { slide, elapsed } => {
                let elapsed = elapsed.saturating_add(1);
                if elapsed < slide.duration {
                    Phase::Hold { slide, elapsed }
                } else if !self.config.auto_advance {
                    // Hold until told to move; clamp to avoid unbounded
                    // growth.
                    let elapsed = elapsed.min(slide.duration);
                    Phase::Hold { slide, elapsed }
                } else if let Some(plan) = self.plan_next(slide.id) {
                    let incoming = self.commit(plan);
                    if t == 0 {
                        Phase::Hold {
                            slide: incoming,
                            elapsed: 0,
                        }
                    } else {
                        let outgoing_base = t + slide.duration;
                        Phase::Transition {
                            outgoing: slide,
                            incoming,
                            step: 0,
                            outgoing_base,
                        }
                    }
                } else {
                    // Hold the last slide; clamp to avoid unbounded growth.
                    let elapsed = elapsed.min(slide.duration);
                    Phase::Hold { slide, elapsed }
                }
            }
            Phase::Transition {
                outgoing,
                incoming,
                step,
                outgoing_base,
            } => {
                let step = step + 1;
                if step >= t {
                    Phase::Hold {
                        slide: incoming,
                        elapsed: 0,
                    }
                } else {
                    Phase::Transition {
                        outgoing,
                        incoming,
                        step,
                        outgoing_base,
                    }
                }
            }
        };
        self.phase = phase;
        self.retire_off_air();
    }

    /// Drops every retiring slide that is no longer on air.
    fn retire_off_air(&mut self) {
        if self.retiring.is_empty() {
            return;
        }
        let on_air = self.on_air();
        let gone: Vec<SlideId> = self
            .retiring
            .iter()
            .copied()
            .filter(|id| !on_air.contains(id))
            .collect();
        for id in gone {
            self.retiring.remove(&id);
            self.pending.retain(|pending| *pending != id);
            if let Some(index) = self.position(id) {
                self.slides.remove(index);
            }
        }
    }

    /// The slide on air, or the incoming one during a transition.
    #[must_use]
    pub const fn current(&self) -> SlideId {
        match &self.phase {
            Phase::Hold { slide, .. } => slide.id,
            Phase::Transition { incoming, .. } => incoming.id,
        }
    }

    /// The authored position of `id`, if it is listed.
    #[must_use]
    pub fn index_of(&self, id: SlideId) -> Option<usize> {
        self.position(id)
    }

    /// Whether `id` left the list in a [`Self::sync`] and stays only
    /// until it goes off air.
    #[must_use]
    pub fn is_retiring(&self, id: SlideId) -> bool {
        self.retiring.contains(&id)
    }

    /// Moves to slide `id` from the next tick, through the configured
    /// transition. The slide shown now (the incoming one, mid-transition)
    /// becomes the outgoing slide and continues its motion from where it
    /// is. Jumping to the slide already shown does nothing.
    ///
    /// # Errors
    ///
    /// Returns [`SlideshowEditError::UnknownSlide`] for a slide that is
    /// not listed.
    pub fn jump(&mut self, id: SlideId) -> Result<(), SlideshowEditError> {
        let index = self
            .position(id)
            .ok_or(SlideshowEditError::UnknownSlide(id))?;
        let t = self.transition_frames();
        let (shown, local) = match &self.phase {
            Phase::Hold { slide, elapsed } => (slide.clone(), t + elapsed),
            Phase::Transition { incoming, step, .. } => (incoming.clone(), *step),
        };
        if shown.id == id {
            return Ok(());
        }
        self.played.insert(id);
        self.pending.retain(|pending| *pending != id);
        let incoming = on_air(&self.slides[index], self.config);
        self.phase = if t == 0 {
            Phase::Hold {
                slide: incoming,
                elapsed: 0,
            }
        } else {
            Phase::Transition {
                outgoing: shown,
                incoming,
                step: 0,
                outgoing_base: local,
            }
        };
        self.retire_off_air();
        Ok(())
    }

    /// Moves to the slide [`Self::peek_next`] names, from the next tick,
    /// through the configured transition, as [`Self::jump`] would, and
    /// takes it the way auto-advance does: when it leads a new cycle, that
    /// cycle starts (its order is the rest of the new permutation), so no
    /// slide repeats within it. Returns the slide moved to; `None` (and
    /// nothing changes) when [`Self::peek_next`] is `None`.
    pub fn jump_next(&mut self) -> Option<SlideId> {
        let Phase::Hold { slide, .. } = &self.phase else {
            return None;
        };
        let plan = self.plan_next(slide.id)?;
        let id = plan.id;
        if plan.new_cycle {
            self.cycle = self.cycle.saturating_add(1);
            self.played.clear();
        }
        self.pending = plan.pending;
        self.played.insert(id);
        self.jump(id).ok()?;
        Some(id)
    }

    /// Makes `slides` the authored list. Slides absent from it that are on
    /// air stay, at their previous position, until they go off air (see
    /// [`Self::is_retiring`]); in random order slides new to the list join
    /// the rest of the current cycle and slides that left it leave.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or oversized list, a duplicate, or an
    /// invalid slide; the list is unchanged then.
    pub fn sync(&mut self, slides: Vec<Slide>) -> Result<(), SlideshowEditError> {
        if slides.is_empty() {
            return Err(SlideshowEditError::Invalid(SlideshowError::Empty));
        }
        if slides.len() > MAX_SLIDES {
            return Err(SlideshowEditError::TooManySlides { max: MAX_SLIDES });
        }
        let mut seen = BTreeSet::new();
        for slide in &slides {
            if !seen.insert(slide.id) {
                return Err(SlideshowEditError::DuplicateSlide(slide.id));
            }
            slide.validate().map_err(SlideshowEditError::Invalid)?;
        }
        let mut next = slides;
        let mut retiring = BTreeSet::new();
        for id in self.on_air() {
            if seen.contains(&id) {
                continue;
            }
            let Some(old_index) = self.position(id) else {
                continue;
            };
            let slide = self.slides[old_index].clone();
            let index = old_index.min(next.len());
            next.insert(index, slide);
            retiring.insert(id);
        }
        if next.len() > MAX_SLIDES {
            return Err(SlideshowEditError::TooManySlides { max: MAX_SLIDES });
        }
        if matches!(self.config.order, SlideOrder::Random { .. }) {
            self.pending
                .retain(|id| seen.contains(id) && !retiring.contains(id));
            for slide in &next {
                if !self.played.contains(&slide.id)
                    && !self.pending.contains(&slide.id)
                    && !retiring.contains(&slide.id)
                {
                    self.pending.push_back(slide.id);
                }
            }
        }
        self.played
            .retain(|id| seen.contains(id) || retiring.contains(id));
        self.slides = next;
        self.retiring = retiring;
        Ok(())
    }

    fn commit(&mut self, plan: NextPlan) -> OnAir {
        if plan.new_cycle {
            self.cycle = self.cycle.saturating_add(1);
            self.played.clear();
        }
        self.pending = plan.pending;
        self.played.insert(plan.id);
        let index = self.position(plan.id).unwrap_or(0);
        on_air(&self.slides[index], self.config)
    }

    fn plan_next(&self, current: SlideId) -> Option<NextPlan> {
        match self.config.order {
            SlideOrder::Ordered => {
                let index = self.position(current)?;
                if index + 1 < self.slides.len() {
                    Some(NextPlan {
                        id: self.slides[index + 1].id,
                        new_cycle: false,
                        pending: VecDeque::new(),
                    })
                } else if self.config.loop_show {
                    Some(NextPlan {
                        id: self.slides[0].id,
                        new_cycle: true,
                        pending: VecDeque::new(),
                    })
                } else {
                    None
                }
            }
            SlideOrder::Random { seed } => {
                let mut pending = self.pending.clone();
                if let Some(id) = pending.pop_front() {
                    return Some(NextPlan {
                        id,
                        new_cycle: false,
                        pending,
                    });
                }
                if !self.config.loop_show {
                    return None;
                }
                let next_cycle = self.cycle.saturating_add(1);
                let mut pending =
                    random_cycle(&self.live_slides(), seed, next_cycle, Some(current));
                let id = pending.pop_front()?;
                Some(NextPlan {
                    id,
                    new_cycle: true,
                    pending,
                })
            }
        }
    }

    /// The authored slides without the retiring ones.
    fn live_slides(&self) -> Vec<Slide> {
        self.slides
            .iter()
            .filter(|slide| !self.retiring.contains(&slide.id))
            .cloned()
            .collect()
    }

    fn position(&self, id: SlideId) -> Option<usize> {
        self.slides.iter().position(|slide| slide.id == id)
    }
}

fn on_air(slide: &Slide, config: SlideshowConfig) -> OnAir {
    OnAir {
        id: slide.id,
        duration: slide.duration.unwrap_or(config.default_duration_frames),
        source_width: slide.source_width,
        source_height: slide.source_height,
        ken_burns: slide.ken_burns,
    }
}

fn view(slide: &OnAir, local_frame: u64, transition_frames: u64) -> SlideView {
    let span = slide.duration + 2 * transition_frames;
    SlideView {
        id: slide.id,
        local_frame,
        crop: crop_at(slide, local_frame.min(span - 1), span),
    }
}

/// Exact interpolation `start + (end - start) * k / (span - 1)`.
fn crop_at(slide: &OnAir, k: u64, span: u64) -> CropRect {
    let full = PixelRect::new(0, 0, slide.source_width, slide.source_height);
    let motion = slide.ken_burns.unwrap_or(KenBurns {
        start: full,
        end: full,
    });
    let denominator = i128::from(span.saturating_sub(1).max(1));
    let k = i128::from(k);
    let lerp = |start: u32, end: u32| -> i128 {
        let start = i128::from(start);
        start * denominator + (i128::from(end) - start) * k
    };
    let values = [
        lerp(motion.start.x, motion.end.x),
        lerp(motion.start.y, motion.end.y),
        lerp(motion.start.width, motion.end.width),
        lerp(motion.start.height, motion.end.height),
    ];
    let divisor = values
        .iter()
        .fold(denominator, |acc, value| gcd(acc, value.abs()));
    let reduce = |value: i128| u64::try_from(value / divisor).unwrap_or(0);
    CropRect {
        x: reduce(values[0]),
        y: reduce(values[1]),
        width: reduce(values[2]),
        height: reduce(values[3]),
        denominator: reduce(denominator),
    }
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

/// Seeded Fisher-Yates permutation of the authored IDs for one cycle. When
/// `avoid_first` would lead the cycle and more than one slide exists, the
/// first and last entries are swapped.
fn random_cycle(
    slides: &[Slide],
    seed: u64,
    cycle: u64,
    avoid_first: Option<SlideId>,
) -> VecDeque<SlideId> {
    let mut order: Vec<SlideId> = slides.iter().map(|slide| slide.id).collect();
    let mut state = seed ^ cycle.wrapping_mul(0xA076_1D64_78BD_642F) ^ 0x5D1D_E5A0_5EED_0001;
    for index in (1..order.len()).rev() {
        let bound = u64::try_from(index + 1).unwrap_or(u64::MAX);
        let pick = usize::try_from(splitmix64(&mut state) % bound).unwrap_or(0);
        order.swap(index, pick);
    }
    if order.len() > 1 && avoid_first.is_some_and(|id| order[0] == id) {
        let last = order.len() - 1;
        order.swap(0, last);
    }
    order.into()
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
