//! Deterministic ticker crawl (horizontal) and roll (vertical).
//!
//! A [`Crawl`] is an endless *tape* of items separated by a separator, moving
//! across a viewport at an exact rational speed in pixels per second. Motion is
//! a pure function of an engine frame index and a [`FrameRate`]: there is no
//! wall clock anywhere, so the same frame index always yields the same pixel
//! positions on every host.
//!
//! # Tape geometry
//!
//! Tape offsets are whole pixels along the motion axis. Each item occupies
//! `extent` pixels, followed by a *separator group*: `gap`, the separator,
//! `gap` when a separator is configured, or a single `gap` when it is empty.
//! After the last item the list wraps seamlessly to the first one, so an
//! unchanged list is periodic with no blank stretch beyond the separator group.
//!
//! The distance travelled at frame `f` is
//! `floor((f - start_frame) * pixels * rate_den / (seconds * rate_num))`, and a
//! tape offset `p` is drawn at viewport offset `viewport + p - distance`: the
//! first item's leading edge sits on the far (right or bottom) edge at
//! `start_frame` and enters on the next frame that moves at least one pixel.
//! `Crawl` moves right to left and `Roll` bottom to top; `CrawlRight` and
//! `RollDown` are the mirror images, entering from the left or top edge. Roll
//! modes word-wrap each item to the viewport width.
//!
//! # Live updates never jump
//!
//! [`Crawl::set_items`] does not replace the tape at the update frame. It finds
//! the next *item boundary* - the first item start at or beyond the current
//! distance, i.e. an item whose leading edge has not yet entered the viewport -
//! and splices the new list in there. Everything already on screen, including
//! the in-progress item's trailing separator, keeps moving exactly as it was.
//! The new list continues at the same list position (`index % new_len`) so a
//! feed that updates faster than one cycle still advances through its items.
//! Each splice is an *epoch*; epochs that have fully left the viewport are
//! pruned, and the number of live epochs is capped.
//!
//! # Bounds
//!
//! Item count, characters per item, separator length, gap, epochs, and the
//! number of segments reported for one frame are all capped. Every item has an
//! extent of at least one pixel, so walking the tape always makes progress.

use crate::{
    Alignment, AssetCatalog, Bounds, Color, ElementId, FontFace, FontStyle, MAX_FONT_SIZE_PX,
    font::ScaledFace,
    text::{LaidText, layout_text},
};
use core::fmt;
use fm_types::FrameRate;
use std::sync::Arc;

/// Maximum items in one crawl list.
pub const MAX_CRAWL_ITEMS: usize = 256;
/// Maximum characters in one crawl item.
pub const MAX_CRAWL_ITEM_CHARS: usize = 512;
/// Maximum characters in the separator.
pub const MAX_CRAWL_SEPARATOR_CHARS: usize = 32;
/// Maximum gap on either side of the separator, in pixels.
pub const MAX_CRAWL_GAP_PX: u32 = 4_096;
/// Maximum live epochs (the visible ones plus the pending splice).
pub const MAX_CRAWL_EPOCHS: usize = 16;
/// Maximum segments reported (and drawn) for one frame.
pub const MAX_CRAWL_VISIBLE_SEGMENTS: usize = 1_024;

/// Direction of travel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrawlMode {
    /// Horizontal, right to left, one unwrapped line per item.
    Crawl,
    /// Horizontal, left to right, one unwrapped line per item.
    CrawlRight,
    /// Vertical, bottom to top, each item word-wrapped to the viewport width.
    Roll,
    /// Vertical, top to bottom, each item word-wrapped to the viewport width.
    RollDown,
}

impl CrawlMode {
    #[must_use]
    pub const fn is_horizontal(self) -> bool {
        matches!(self, Self::Crawl | Self::CrawlRight)
    }

    /// Whether items travel toward increasing x or y (left to right, or top
    /// to bottom).
    #[must_use]
    pub const fn is_reversed(self) -> bool {
        matches!(self, Self::CrawlRight | Self::RollDown)
    }
}

/// One item of a crawl with optional per-item styling: `fill` overrides the
/// crawl's fill colour for that item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CrawlItem<'a> {
    pub text: &'a str,
    pub fill: Option<Color>,
}

impl<'a> CrawlItem<'a> {
    #[must_use]
    pub const fn text(text: &'a str) -> Self {
        Self { text, fill: None }
    }

    #[must_use]
    pub const fn with_fill(text: &'a str, fill: Color) -> Self {
        Self {
            text,
            fill: Some(fill),
        }
    }
}

/// An exact rational speed: `pixels` every `seconds`, normalized.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CrawlSpeed {
    pixels: u32,
    seconds: u32,
}

impl CrawlSpeed {
    /// Creates a normalized speed of `pixels / seconds` pixels per second.
    ///
    /// # Errors
    ///
    /// Returns [`CrawlError::ZeroSpeed`] when either part is zero; a static
    /// line belongs in a text element, not a crawl.
    pub fn new(pixels: u32, seconds: u32) -> Result<Self, CrawlError> {
        if pixels == 0 || seconds == 0 {
            return Err(CrawlError::ZeroSpeed);
        }
        let divisor = gcd(pixels, seconds);
        Ok(Self {
            pixels: pixels / divisor,
            seconds: seconds / divisor,
        })
    }

    #[must_use]
    pub const fn pixels(self) -> u32 {
        self.pixels
    }

    #[must_use]
    pub const fn seconds(self) -> u32 {
        self.seconds
    }
}

/// Static configuration of one crawl.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrawlConfig {
    pub mode: CrawlMode,
    /// The viewport on the output canvas. Text is clipped to it.
    pub bounds: Bounds,
    pub speed: CrawlSpeed,
    /// Engine frame rate the frame index counts in.
    pub frame_rate: FrameRate,
    /// Frame at which the first item's leading edge sits on the entry edge.
    pub start_frame: u64,
    /// Drawn between consecutive items. May be empty.
    pub separator: String,
    /// Space on each side of the separator (or between items when there is
    /// no separator), in pixels.
    pub gap_px: u32,
    pub font: FontStyle,
    pub fill: Color,
    pub background: Option<Color>,
    pub opacity: u8,
    /// Cross-axis alignment: vertical for a crawl, horizontal for a roll.
    pub alignment: Alignment,
}

impl CrawlConfig {
    /// Viewport length along the motion axis.
    #[must_use]
    pub const fn viewport_length(&self) -> u32 {
        if self.mode.is_horizontal() {
            self.bounds.width
        } else {
            self.bounds.height
        }
    }

    /// Distance travelled at `frame`, in whole pixels (floor of the exact
    /// rational distance). Zero at and before `start_frame`.
    #[must_use]
    pub fn distance_at(&self, frame: u64) -> u64 {
        let frames = u128::from(frame.saturating_sub(self.start_frame));
        let numerator =
            frames * u128::from(self.speed.pixels) * u128::from(self.frame_rate.denominator());
        let denominator = u128::from(self.speed.seconds) * u128::from(self.frame_rate.numerator());
        u64::try_from(numerator / denominator).unwrap_or(u64::MAX)
    }

    /// The first frame whose distance is at least `distance`.
    #[must_use]
    pub fn first_frame_reaching(&self, distance: u64) -> u64 {
        let numerator = u128::from(distance)
            * u128::from(self.speed.seconds)
            * u128::from(self.frame_rate.numerator());
        let denominator = u128::from(self.speed.pixels) * u128::from(self.frame_rate.denominator());
        let frames = numerator.div_ceil(denominator);
        self.start_frame
            .saturating_add(u64::try_from(frames).unwrap_or(u64::MAX))
    }
}

/// Measures an item's extent along the motion axis, in pixels.
///
/// Rendering always lays text out with the crawl's font, so a measure used for
/// a crawl that is rendered must agree with that font; [`FontCrawlMeasure`]
/// does by construction. Other measures are useful for pure timing models.
pub trait CrawlMeasure {
    fn measure(&self, text: &str) -> u32;
}

/// Measures with the crawl's own font exactly as the renderer lays it out:
/// the kerned advance width for a crawl, or the wrapped line count times the
/// line height for a roll.
pub struct FontCrawlMeasure<'a> {
    face: &'a FontFace,
    size_px: u32,
    mode: CrawlMode,
    wrap_width: u32,
}

impl<'a> FontCrawlMeasure<'a> {
    /// # Errors
    ///
    /// Returns [`CrawlError::MissingFont`] when `assets` has no face for the
    /// configured family, or [`CrawlError::FontSizeTooLarge`].
    pub fn new(config: &CrawlConfig, assets: &'a AssetCatalog) -> Result<Self, CrawlError> {
        check_font_size(config.font.size_px)?;
        let face = assets
            .font(&config.font.family)
            .ok_or_else(|| CrawlError::MissingFont(config.font.family.clone()))?;
        Ok(Self {
            face,
            size_px: config.font.size_px.max(1),
            mode: config.mode,
            wrap_width: config.bounds.width,
        })
    }
}

impl CrawlMeasure for FontCrawlMeasure<'_> {
    fn measure(&self, text: &str) -> u32 {
        let scaled = self.face.scaled(self.size_px);
        let laid = layout_segment(&scaled, self.mode, self.wrap_width, text);
        segment_extent(self.mode, &laid)
    }
}

/// Lays out one segment the way both measurement and rendering do.
pub(crate) fn layout_segment(
    scaled: &ScaledFace<'_>,
    mode: CrawlMode,
    wrap_width: u32,
    text: &str,
) -> LaidText {
    let wrap = if mode.is_horizontal() {
        None
    } else {
        Some(wrap_width)
    };
    layout_text(scaled, text, wrap, MAX_CRAWL_ITEM_CHARS)
}

fn segment_extent(mode: CrawlMode, laid: &LaidText) -> u32 {
    if mode.is_horizontal() {
        laid.width_px
    } else {
        let lines = i64::try_from(laid.lines.len()).unwrap_or(i64::MAX);
        u32::try_from(laid.line_height_px.saturating_mul(lines).max(0)).unwrap_or(u32::MAX)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Measured {
    text: String,
    extent: u32,
    fill: Option<Color>,
}

/// A contiguous stretch of tape driven by one item list.
#[derive(Clone, Debug)]
struct Epoch {
    start: u64,
    start_index: usize,
    items: Arc<[Measured]>,
    revision: u64,
}

/// What a segment on screen is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrawlSegmentKind {
    /// Item `index` of the list accepted at the segment's revision.
    Item {
        index: usize,
    },
    Separator,
}

/// One item or separator at least partly inside the viewport at a frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CrawlSegment<'a> {
    pub kind: CrawlSegmentKind,
    pub text: &'a str,
    /// Per-item fill override; `None` uses the crawl's fill.
    pub fill: Option<Color>,
    /// Leading edge relative to the viewport origin along the motion axis:
    /// x from the viewport's left for a horizontal mode, y from its top for a
    /// vertical one.
    pub offset: i64,
    pub extent: u32,
    /// Item-list revision that produced the segment.
    pub revision: u64,
}

/// Everything of a crawl that is on screen at one frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrawlView<'a> {
    pub frame: u64,
    pub distance: u64,
    /// In increasing viewport offset, i.e. left to right or top to bottom.
    pub segments: Vec<CrawlSegment<'a>>,
    /// [`MAX_CRAWL_VISIBLE_SEGMENTS`] stopped the walk early.
    pub truncated: bool,
}

/// Where an accepted item list joins the tape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CrawlSchedule {
    pub revision: u64,
    /// Tape offset of the splice (an item boundary).
    pub boundary: u64,
    /// List index the new list starts at.
    pub start_index: usize,
    /// First frame on which the new list's first item is inside the viewport.
    pub enters_at_frame: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CrawlError {
    ZeroSpeed,
    EmptyViewport,
    FontSizeTooLarge {
        size_px: u32,
        maximum: u32,
    },
    MissingFont(String),
    GapTooLarge {
        gap_px: u32,
        maximum: u32,
    },
    SeparatorTooLong {
        maximum: usize,
    },
    TooManyItems {
        count: usize,
        maximum: usize,
    },
    ItemTooLong {
        index: usize,
        maximum: usize,
    },
    BlankItem {
        index: usize,
    },
    /// Items and the separator must not contain control characters
    /// (including newlines); a roll wraps by width instead.
    ControlCharacter {
        index: Option<usize>,
    },
    RevisionConflict {
        expected: u64,
        actual: u64,
    },
    RevisionExhausted,
    /// Frames passed to a crawl must not go backwards past the last update.
    FrameRegressed {
        frame: u64,
        last_frame: u64,
    },
    /// More than [`MAX_CRAWL_EPOCHS`] splices are still live.
    UpdateBacklog {
        maximum: usize,
    },
}

impl fmt::Display for CrawlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroSpeed => formatter.write_str("crawl speed must be nonzero"),
            Self::EmptyViewport => formatter.write_str("crawl viewport is empty"),
            Self::FontSizeTooLarge { size_px, maximum } => {
                write!(formatter, "crawl font size {size_px}px exceeds {maximum}px")
            }
            Self::MissingFont(family) => write!(formatter, "crawl font {family:?} is missing"),
            Self::GapTooLarge { gap_px, maximum } => {
                write!(formatter, "crawl gap {gap_px}px exceeds {maximum}px")
            }
            Self::SeparatorTooLong { maximum } => {
                write!(formatter, "crawl separator exceeds {maximum} characters")
            }
            Self::TooManyItems { count, maximum } => {
                write!(formatter, "crawl has {count} items, maximum is {maximum}")
            }
            Self::ItemTooLong { index, maximum } => {
                write!(formatter, "crawl item {index} exceeds {maximum} characters")
            }
            Self::BlankItem { index } => write!(formatter, "crawl item {index} is blank"),
            Self::ControlCharacter { index: Some(index) } => {
                write!(formatter, "crawl item {index} contains a control character")
            }
            Self::ControlCharacter { index: None } => {
                formatter.write_str("crawl separator contains a control character")
            }
            Self::RevisionConflict { expected, actual } => write!(
                formatter,
                "expected crawl revision {expected}, current revision is {actual}"
            ),
            Self::RevisionExhausted => formatter.write_str("crawl revision is exhausted"),
            Self::FrameRegressed { frame, last_frame } => write!(
                formatter,
                "crawl frame {frame} precedes last update frame {last_frame}"
            ),
            Self::UpdateBacklog { maximum } => {
                write!(formatter, "crawl has more than {maximum} live item lists")
            }
        }
    }
}

impl std::error::Error for CrawlError {}

/// A live ticker crawl or roll. See the module documentation.
#[derive(Clone, Debug)]
pub struct Crawl {
    id: ElementId,
    config: CrawlConfig,
    separator: Measured,
    epochs: Vec<Epoch>,
    revision: u64,
    last_frame: u64,
}

impl Crawl {
    /// Creates a crawl whose first item enters at `config.start_frame`.
    ///
    /// # Errors
    ///
    /// Returns a [`CrawlError`] for an empty viewport, oversized font or gap,
    /// or an invalid separator or item list.
    pub fn new(
        id: ElementId,
        config: CrawlConfig,
        items: &[&str],
        measure: &dyn CrawlMeasure,
    ) -> Result<Self, CrawlError> {
        let items: Vec<CrawlItem<'_>> = items.iter().map(|text| CrawlItem::text(text)).collect();
        Self::new_styled(id, config, &items, measure)
    }

    /// Creates a crawl whose items carry optional per-item fill colours.
    ///
    /// # Errors
    ///
    /// Returns a [`CrawlError`] for an empty viewport, oversized font or gap,
    /// or an invalid separator or item list.
    pub fn new_styled(
        id: ElementId,
        config: CrawlConfig,
        items: &[CrawlItem<'_>],
        measure: &dyn CrawlMeasure,
    ) -> Result<Self, CrawlError> {
        if config.bounds.width == 0 || config.bounds.height == 0 {
            return Err(CrawlError::EmptyViewport);
        }
        check_font_size(config.font.size_px)?;
        if config.gap_px > MAX_CRAWL_GAP_PX {
            return Err(CrawlError::GapTooLarge {
                gap_px: config.gap_px,
                maximum: MAX_CRAWL_GAP_PX,
            });
        }
        if config.separator.chars().count() > MAX_CRAWL_SEPARATOR_CHARS {
            return Err(CrawlError::SeparatorTooLong {
                maximum: MAX_CRAWL_SEPARATOR_CHARS,
            });
        }
        if config.separator.chars().any(char::is_control) {
            return Err(CrawlError::ControlCharacter { index: None });
        }
        let separator = if config.separator.trim().is_empty() {
            Measured {
                text: String::new(),
                extent: 0,
                fill: None,
            }
        } else {
            Measured {
                extent: measure.measure(&config.separator),
                text: config.separator.clone(),
                fill: None,
            }
        };
        let items = measure_items(items, measure)?;
        Ok(Self {
            id,
            config,
            separator,
            epochs: vec![Epoch {
                start: 0,
                start_index: 0,
                items,
                revision: 0,
            }],
            revision: 0,
            last_frame: 0,
        })
    }

    #[must_use]
    pub const fn id(&self) -> ElementId {
        self.id
    }

    #[must_use]
    pub const fn config(&self) -> &CrawlConfig {
        &self.config
    }

    /// Revision of the most recently accepted item list.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Frame of the most recent accepted update; earlier frames are refused.
    #[must_use]
    pub const fn last_frame(&self) -> u64 {
        self.last_frame
    }

    /// Number of live epochs (on-screen lists plus a pending splice).
    #[must_use]
    pub fn live_epochs(&self) -> usize {
        self.epochs.len()
    }

    /// Tape length of one full cycle of the most recently accepted list.
    #[must_use]
    pub fn cycle_length(&self) -> u64 {
        self.epochs
            .last()
            .map_or(0, |epoch| self.cycle(&epoch.items))
    }

    fn group(&self) -> u64 {
        let gap = u64::from(self.config.gap_px);
        if self.separator.text.is_empty() {
            gap
        } else {
            gap * 2 + u64::from(self.separator.extent)
        }
    }

    fn step(&self, item: &Measured) -> u64 {
        u64::from(item.extent) + self.group()
    }

    fn cycle(&self, items: &[Measured]) -> u64 {
        items.iter().map(|item| self.step(item)).sum()
    }

    /// Replaces the item list at the next item boundary after `frame`.
    ///
    /// # Errors
    ///
    /// Returns a conflict, frame regression, invalid list, backlog, or
    /// revision exhaustion without changing the crawl.
    pub fn set_items(
        &mut self,
        expected_revision: u64,
        frame: u64,
        items: &[&str],
        measure: &dyn CrawlMeasure,
    ) -> Result<CrawlSchedule, CrawlError> {
        let items: Vec<CrawlItem<'_>> = items.iter().map(|text| CrawlItem::text(text)).collect();
        self.set_styled_items(expected_revision, frame, &items, measure)
    }

    /// Replaces the item list with styled items at the next item boundary
    /// after `frame`.
    ///
    /// # Errors
    ///
    /// Returns a conflict, frame regression, invalid list, backlog, or
    /// revision exhaustion without changing the crawl.
    pub fn set_styled_items(
        &mut self,
        expected_revision: u64,
        frame: u64,
        items: &[CrawlItem<'_>],
        measure: &dyn CrawlMeasure,
    ) -> Result<CrawlSchedule, CrawlError> {
        if expected_revision != self.revision {
            return Err(CrawlError::RevisionConflict {
                expected: expected_revision,
                actual: self.revision,
            });
        }
        self.check_frame(frame)?;
        let items = measure_items(items, measure)?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(CrawlError::RevisionExhausted)?;
        let distance = self.config.distance_at(frame);

        // The epoch the leading edge is in: the last one starting at or before
        // `distance`. Epoch 0 starts at 0 and pruning always keeps the epoch
        // containing the current distance, so one exists.
        let current = self
            .epochs
            .iter()
            .rposition(|epoch| epoch.start <= distance)
            .unwrap_or(0);
        let (mut boundary, mut old_index) = self.next_boundary(&self.epochs[current], distance);
        // A pending splice already sits on an item boundary of the current
        // epoch at or beyond `distance`, so it can only coincide with the
        // candidate; keep the current epoch's list position in that case.
        if let Some(next) = self.epochs.get(current + 1)
            && next.start < boundary
        {
            boundary = next.start;
            old_index = next.start_index;
        }

        let mut epochs = self.epochs.clone();
        epochs.truncate(current + 1);
        if epochs.last().is_some_and(|epoch| epoch.start >= boundary) {
            epochs.pop();
        }
        let start_index = if items.is_empty() {
            0
        } else {
            old_index % items.len()
        };
        epochs.push(Epoch {
            start: boundary,
            start_index,
            items,
            revision,
        });
        let visible_from = i128::from(distance) - i128::from(self.config.viewport_length());
        while epochs.len() > 1 && i128::from(epochs[1].start) <= visible_from {
            epochs.remove(0);
        }
        if epochs.len() > MAX_CRAWL_EPOCHS {
            return Err(CrawlError::UpdateBacklog {
                maximum: MAX_CRAWL_EPOCHS,
            });
        }

        self.epochs = epochs;
        self.revision = revision;
        self.last_frame = frame;
        let first_extent = self
            .epochs
            .last()
            .and_then(|epoch| epoch.items.get(epoch.start_index))
            .map_or(1, |item| u64::from(item.extent));
        let enters_at_frame = if self.config.mode.is_reversed() {
            self.config
                .first_frame_reaching(boundary.saturating_sub(first_extent).saturating_add(1))
        } else {
            self.config.first_frame_reaching(boundary.saturating_add(1))
        };
        Ok(CrawlSchedule {
            revision,
            boundary,
            start_index,
            enters_at_frame,
        })
    }

    /// First item start that has not begun entering the viewport at
    /// `distance` within `epoch`, and the old list index of the item that
    /// would have started there.
    fn next_boundary(&self, epoch: &Epoch, distance: u64) -> (u64, usize) {
        let items = &epoch.items;
        if items.is_empty() {
            return (distance.max(epoch.start), epoch.start_index);
        }
        let cycle = self.cycle(items).max(1);
        let relative = distance.saturating_sub(epoch.start);
        let mut position = epoch.start.saturating_add(relative / cycle * cycle);
        let mut index = epoch.start_index % items.len();
        // At most one cycle (<= MAX_CRAWL_ITEMS steps), each >= 1 pixel.
        loop {
            let unseen = if self.config.mode.is_reversed() {
                position >= distance.saturating_add(u64::from(items[index].extent))
            } else {
                position >= distance
            };
            if unseen {
                break;
            }
            position = position.saturating_add(self.step(&items[index]));
            index = (index + 1) % items.len();
        }
        (position, index)
    }

    fn check_frame(&self, frame: u64) -> Result<(), CrawlError> {
        if frame < self.last_frame {
            return Err(CrawlError::FrameRegressed {
                frame,
                last_frame: self.last_frame,
            });
        }
        Ok(())
    }

    /// Lists the segments inside the viewport at `frame`.
    ///
    /// # Errors
    ///
    /// Returns [`CrawlError::FrameRegressed`] for a frame before the last
    /// accepted update: its pruned history can no longer be reproduced.
    pub fn view(&self, frame: u64) -> Result<CrawlView<'_>, CrawlError> {
        self.check_frame(frame)?;
        let distance = self.config.distance_at(frame);
        let viewport = i128::from(self.config.viewport_length());
        let high = i128::from(distance);
        let low = high - viewport;
        let reversed = self.config.mode.is_reversed();
        let raw_offset = |position: u64| -> i128 {
            if reversed {
                high - i128::from(position)
            } else {
                viewport + i128::from(position) - high
            }
        };
        let screen = |offset: i128| -> i64 {
            i64::try_from(offset).unwrap_or(if offset < 0 { i64::MIN } else { i64::MAX })
        };
        let group = self.group();
        let gap = u64::from(self.config.gap_px);
        let mut segments = Vec::new();
        let mut truncated = false;

        'epochs: for (number, epoch) in self.epochs.iter().enumerate() {
            let end = self.epochs.get(number + 1).map(|next| next.start);
            if !reversed && i128::from(epoch.start) >= high {
                break;
            }
            if end.is_some_and(|end| i128::from(end) <= low) || epoch.items.is_empty() {
                continue;
            }
            let items = &epoch.items;
            let cycle = self.cycle(items).max(1);
            let skip = u64::try_from((low - i128::from(epoch.start)).max(0)).unwrap_or(u64::MAX);
            let mut position = epoch.start.saturating_add(skip / cycle * cycle);
            let mut index = epoch.start_index % items.len();
            // Later items are further left for a reversed tape; once even the
            // widest one cannot reach the viewport, the rest of this epoch is
            // invisible.
            let max_extent = items
                .iter()
                .map(|item| i128::from(item.extent))
                .max()
                .unwrap_or(0);
            loop {
                if end.is_some_and(|end| position >= end) {
                    break;
                }
                let offset = raw_offset(position);
                if reversed {
                    if offset + max_extent <= 0 {
                        break;
                    }
                } else if offset >= viewport {
                    break;
                }
                let item = &items[index];
                let item_extent = i128::from(item.extent);
                let item_end = position.saturating_add(u64::from(item.extent));
                let visible = offset < viewport && offset + item_extent > 0;
                if visible {
                    if segments.len() >= MAX_CRAWL_VISIBLE_SEGMENTS {
                        truncated = true;
                        break 'epochs;
                    }
                    segments.push(CrawlSegment {
                        kind: CrawlSegmentKind::Item { index },
                        text: &item.text,
                        fill: item.fill,
                        offset: screen(offset),
                        extent: item.extent,
                        revision: epoch.revision,
                    });
                }
                if !self.separator.text.is_empty() {
                    let separator = item_end.saturating_add(gap);
                    let separator_offset = raw_offset(separator);
                    if separator_offset < viewport
                        && separator_offset + i128::from(self.separator.extent) > 0
                    {
                        if segments.len() >= MAX_CRAWL_VISIBLE_SEGMENTS {
                            truncated = true;
                            break 'epochs;
                        }
                        segments.push(CrawlSegment {
                            kind: CrawlSegmentKind::Separator,
                            text: &self.separator.text,
                            fill: None,
                            offset: screen(separator_offset),
                            extent: self.separator.extent,
                            revision: epoch.revision,
                        });
                    }
                }
                position = item_end.saturating_add(group);
                index = (index + 1) % items.len();
            }
        }
        if reversed {
            segments.reverse();
        }
        Ok(CrawlView {
            frame,
            distance,
            segments,
            truncated,
        })
    }
}

fn check_font_size(size_px: u32) -> Result<(), CrawlError> {
    if size_px > MAX_FONT_SIZE_PX {
        return Err(CrawlError::FontSizeTooLarge {
            size_px,
            maximum: MAX_FONT_SIZE_PX,
        });
    }
    Ok(())
}

fn measure_items(
    items: &[CrawlItem<'_>],
    measure: &dyn CrawlMeasure,
) -> Result<Arc<[Measured]>, CrawlError> {
    if items.len() > MAX_CRAWL_ITEMS {
        return Err(CrawlError::TooManyItems {
            count: items.len(),
            maximum: MAX_CRAWL_ITEMS,
        });
    }
    for (index, item) in items.iter().enumerate() {
        if item.text.chars().nth(MAX_CRAWL_ITEM_CHARS).is_some() {
            return Err(CrawlError::ItemTooLong {
                index,
                maximum: MAX_CRAWL_ITEM_CHARS,
            });
        }
        if item.text.chars().any(char::is_control) {
            return Err(CrawlError::ControlCharacter { index: Some(index) });
        }
        if item.text.trim().is_empty() {
            return Err(CrawlError::BlankItem { index });
        }
    }
    Ok(items
        .iter()
        .map(|item| Measured {
            text: item.text.to_owned(),
            // At least one pixel so walking the tape always progresses.
            extent: measure.measure(item.text).max(1),
            fill: item.fill,
        })
        .collect())
}

const fn gcd(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}
