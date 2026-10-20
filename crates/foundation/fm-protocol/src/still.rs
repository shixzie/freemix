//! Image and slideshow inputs on the wire (IN-008, IN-009).
//!
//! * `input_add kind=image` carries `uri` and `fit`; `kind=slideshow`
//!   carries one `slideshow` field (the compact settings text, see the
//!   codec). `event=input_added` repeats them, and a replacement source is
//!   `image:<fit>:<uri>` or `slideshow:<settings>`.
//! * A snapshot counts the image and slideshow inputs in `still_states`;
//!   one `still_state` record ([`StillStateMessage`]) follows for each,
//!   after the script states, with its source and, for a slideshow, the
//!   slide on air.
//! * `command=page_navigate` steps a paged input (next, previous, first,
//!   last, goto with `page`), a live operation realized on its target
//!   frame and announced by `event=page_navigated`. The slide each frame
//!   shows is runtime state: `event=slideshow_page` reports a change of
//!   slide, slide count, or listing.
//! * `slideshow_slides_request` asks for a window of one slideshow's
//!   listing; `slideshow_slides` answers with as many slides as fit.

use crate::WireInputId;

pub use fm_types::paged::PagedNavigation;
pub use fm_types::still::{
    ImageInputConfig, MAX_IMAGE_URI_BYTES, MAX_SLIDE_CAPTION_BYTES, MAX_SLIDE_FRAMES,
    MAX_SLIDE_LABEL_BYTES, MAX_SLIDE_TRANSITION_FRAMES, MAX_SLIDESHOW_FOLDER_BYTES,
    MAX_SLIDESHOW_INPUTS, MAX_SLIDESHOW_ITEMS, MAX_SLIDESHOW_ITEMS_BYTES, MAX_SLIDESHOW_SLIDES,
    SlideOrder, SlideTransitionStyle, SlideshowConfig, SlideshowItem, SlideshowPage,
    SlideshowSource, StillInputError, slide_label_is_valid,
};

/// Most `still_state` records one snapshot announces (every input could be
/// an image).
pub const MAX_STILL_STATES: u16 = 256;
/// Most slides one `slideshow_slides_request` asks for.
pub const MAX_SLIDESHOW_SLIDES_PAGE: u32 = 64;
/// Most raw bytes of slide URIs and captions one `slideshow_slides`
/// answer carries; an answer stops early rather than exceed it.
pub const MAX_SLIDESHOW_SLIDES_BYTES: usize = 12 * 1024;

/// The source of an image or slideshow input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireStillSource {
    Image(ImageInputConfig),
    Slideshow(SlideshowConfig),
}

impl WireStillSource {
    /// Validates the settings.
    ///
    /// # Errors
    ///
    /// Returns the first [`StillInputError`].
    pub fn validate(&self) -> Result<(), StillInputError> {
        match self {
            Self::Image(config) => config.validate(),
            Self::Slideshow(config) => config.validate(),
        }
    }

    /// As [`Self::validate`], but also accepts the absolute host paths a
    /// client's command names for the server to import.
    ///
    /// # Errors
    ///
    /// Returns the first [`StillInputError`] found.
    pub fn validate_request(&self) -> Result<(), StillInputError> {
        match self {
            Self::Image(config) => config.validate_request(),
            Self::Slideshow(config) => config.validate_request(),
        }
    }
}

/// What a slideshow shows now: its slide, slide count, label (the caption
/// or file name; empty without slides), and listing generation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SlideshowPageStatus {
    pub page: SlideshowPage,
    pub label: String,
    /// Increases whenever the slideshow's listing changes.
    pub generation: u64,
}

impl SlideshowPageStatus {
    /// Whether the page is consistent and the label bounded.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.page.is_valid() && slide_label_is_valid(&self.label)
    }
}

/// One image or slideshow input after a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StillStateMessage {
    /// The snapshot's revision.
    pub revision: u64,
    pub input: WireInputId,
    pub source: WireStillSource,
    /// The slide on air; the default for an image.
    pub status: SlideshowPageStatus,
}

/// Asks for slides `offset..offset + limit` of a slideshow's listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlideshowSlidesRequest {
    pub request_id: String,
    pub input: WireInputId,
    pub offset: u32,
    /// `1..=`[`MAX_SLIDESHOW_SLIDES_PAGE`].
    pub limit: u32,
}

/// One listed slide.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlideshowSlide {
    pub asset_uri: String,
    pub caption: Option<String>,
}

/// Answers a [`SlideshowSlidesRequest`]: the slides from `offset`, at
/// most the requested count and [`MAX_SLIDESHOW_SLIDES_BYTES`], of a
/// listing of `total` slides at `generation`. An input that is not a
/// slideshow answers `total` 0 at generation 0.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlideshowSlidesMessage {
    pub request_id: String,
    pub input: WireInputId,
    pub generation: u64,
    pub offset: u32,
    pub total: u32,
    pub slides: Vec<SlideshowSlide>,
}
