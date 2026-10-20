//! Paged inputs on the wire (IN-014): document inputs (PDFs and
//! presentations) stepped page by page.
//!
//! A paged input's replicated state is one [`PagedStatus`]: its kind, the
//! persisted current page, and what the server knows of its source (the
//! page count, once known, the [`DocumentState`], the document title, and a
//! status message). The same status rides the `paged_state` follow-on
//! record of a snapshot, the durable `paged_changed` event of an accepted
//! `page_navigate` step, and the `runtime_event` with `event=paged` when
//! the source's state changes, so a client replaces the whole status of
//! one input on every record that names it.
//!
//! Navigation is the generic page step shared with slideshows
//! ([`crate::PagedNavigation`]): `page_navigate` with `action=next|previous|first|
//! last`, or `action=goto` and a zero-based `page`. The server resolves it against the page count (clamping at both
//! ends); a source whose count is not known yet refuses navigation as
//! retryable.
//!
//! Presenter notes and page thumbnails are fetched on demand:
//! `paged_page_request` asks for one page's notes (`thumbnail=0`) or a
//! small three-channel QOI picture (`thumbnail=1`, at most
//! [`MAX_PAGED_THUMBNAIL_WIDTH`] x [`MAX_PAGED_THUMBNAIL_HEIGHT`]), and the
//! server answers with one `paged_page` record carrying the same
//! `request_id` once it has it. Neither is durable nor projected into
//! snapshots.

pub use fm_types::paged::{
    DocumentFormat, DocumentState, MAX_DOCUMENT_INPUTS, MAX_PAGED_MESSAGE_BYTES,
    MAX_PAGED_NOTES_BYTES, MAX_PAGED_PAGES, MAX_PAGED_TITLE_BYTES, bounded_paged_text,
};

use crate::WireInputId;

/// Most `paged_state` records that follow one snapshot.
pub const MAX_PAGED_STATES: u16 = 64;
/// Largest page thumbnail width in pixels.
pub const MAX_PAGED_THUMBNAIL_WIDTH: u16 = 128;
/// Largest page thumbnail height in pixels.
pub const MAX_PAGED_THUMBNAIL_HEIGHT: u16 = 72;
/// Largest raw QOI payload of one page thumbnail; a 128x72 three-channel
/// QOI stream never exceeds it.
pub const MAX_PAGED_THUMBNAIL_BYTES: usize = 40 * 1024;

/// What kind of paged input a status describes.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum PagedKind {
    /// A PDF or a presentation (IN-014).
    #[default]
    Document,
}

impl PagedKind {
    pub const ALL: [Self; 1] = [Self::Document];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Document => "document",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.token() == token)
    }
}

/// The complete replicated state of one paged input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PagedStatus {
    pub input: WireInputId,
    pub kind: PagedKind,
    /// The zero-based page on screen (persisted). Below `count` once the
    /// count is known.
    pub page: u32,
    /// The source's page count; `0` while it is not known.
    pub count: u32,
    pub state: DocumentState,
    /// The document's title, at most [`MAX_PAGED_TITLE_BYTES`] bytes
    /// without control characters.
    pub title: Option<String>,
    /// What the operator should know (a failure, or what to install), at
    /// most [`MAX_PAGED_MESSAGE_BYTES`] bytes without control characters.
    pub message: Option<String>,
}

impl PagedStatus {
    /// Whether the status is inside its wire bounds.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let text_ok = |text: &Option<String>, maximum: usize| {
            text.as_deref().is_none_or(|text| {
                !text.is_empty() && text.len() <= maximum && !text.chars().any(char::is_control)
            })
        };
        self.page < MAX_PAGED_PAGES
            && self.count <= MAX_PAGED_PAGES
            && (self.count == 0 || self.page < self.count)
            && text_ok(&self.title, MAX_PAGED_TITLE_BYTES)
            && text_ok(&self.message, MAX_PAGED_MESSAGE_BYTES)
    }
}

/// One `paged_state` record after a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PagedStateMessage {
    /// The snapshot's revision.
    pub revision: u64,
    pub status: PagedStatus,
}

/// Asks for one page's presenter notes or thumbnail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PagedPageRequest {
    pub request_id: String,
    pub input: WireInputId,
    pub page: u32,
    /// `true` for a thumbnail, `false` for the notes.
    pub thumbnail: bool,
}

/// What a `paged_page` answer carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PagedPageContent {
    /// The page's presenter notes (empty when it has none), at most
    /// [`MAX_PAGED_NOTES_BYTES`] bytes; line breaks are kept.
    Notes(String),
    /// A three-channel QOI picture of the page (sRGB).
    Thumbnail {
        width: u16,
        height: u16,
        /// Raw QOI bytes; base64url (no padding) on the wire.
        data: Vec<u8>,
    },
    /// The page cannot be had (no such input or page, or a source that
    /// does not render).
    Unavailable,
}

/// Answers a [`PagedPageRequest`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PagedPageMessage {
    pub request_id: String,
    pub input: WireInputId,
    pub page: u32,
    pub content: PagedPageContent,
}

/// Whether `width`x`height` is a valid page thumbnail size.
#[must_use]
pub const fn paged_thumbnail_size_valid(width: u16, height: u16) -> bool {
    width != 0
        && height != 0
        && width <= MAX_PAGED_THUMBNAIL_WIDTH
        && height <= MAX_PAGED_THUMBNAIL_HEIGHT
}

/// Whether `notes` fit a `paged_page` answer: bounded, and without control
/// characters other than line breaks.
#[must_use]
pub fn paged_notes_valid(notes: &str) -> bool {
    notes.len() <= MAX_PAGED_NOTES_BYTES && !notes.chars().any(|c| c.is_control() && c != '\n')
}

/// Which answer of one paged input page a client holds.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PagedPageKey {
    pub input: WireInputId,
    pub page: u32,
    /// `true` for the thumbnail, `false` for the notes.
    pub thumbnail: bool,
}

/// A decoded `paged_page` answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PagedPageAnswer {
    /// The page's presenter notes (empty when it has none).
    Notes(String),
    /// The page picture as tightly packed RGBA8 (alpha `255`).
    Thumbnail {
        width: u16,
        height: u16,
        rgba: std::sync::Arc<[u8]>,
    },
    /// The server cannot answer for this page now.
    Unavailable,
}

/// Decodes one `paged_page` answer; a thumbnail is unpacked to RGBA8.
///
/// # Errors
///
/// A thumbnail whose QOI stream does not decode at its size.
pub fn decode_paged_page(
    message: &PagedPageMessage,
) -> Result<PagedPageAnswer, crate::MonitorImageError> {
    Ok(match &message.content {
        PagedPageContent::Notes(notes) => PagedPageAnswer::Notes(notes.clone()),
        PagedPageContent::Thumbnail {
            width,
            height,
            data,
        } => PagedPageAnswer::Thumbnail {
            width: *width,
            height: *height,
            rgba: crate::decode_qoi_rgb(*width, *height, data)?.into(),
        },
        PagedPageContent::Unavailable => PagedPageAnswer::Unavailable,
    })
}
