//! Live input bank thumbnails (Protocol 2.59).
//!
//! A raw Studio peer whose session is eligible for monitor frames may send
//! `input_thumbnail_subscription` with `enabled=1` while its input bank is
//! visible, and `enabled=0` when it no longer is. While subscribed, the
//! server sends lossy, latest-wins `input_thumbnail` records: one small
//! three-channel QOI picture (at most [`MAX_INPUT_THUMBNAIL_WIDTH`] x
//! [`MAX_INPUT_THUMBNAIL_HEIGHT`], at most [`MAX_INPUT_THUMBNAIL_BYTES`]
//! encoded) of one input that has a picture. Like `monitor_frame` they are
//! never durable, never resumable, and never projected into snapshots; a
//! fresh connection starts unsubscribed.
//!
//! A record with `item` (Protocol 2.80) is not the input's live picture
//! but one media list item's (IN-006): the picture at the item's in mark,
//! re-sent only when the item's file or in mark changes (and to a peer that
//! just subscribed). Records without `item` are the input's own picture.
//!
//! `sequence` strictly increases per input within one connection.
//! `age_ms` is how long ago the server last saw the input's picture change
//! (saturating at `u32::MAX`), so a client can tell a frozen live source
//! from a thumbnail that merely arrived late.

use std::sync::Arc;

use crate::{MonitorFrameOrigin, MonitorImageError, ServerIdentity, WireInputId, decode_qoi_rgb};

/// Largest input thumbnail width in pixels.
pub const MAX_INPUT_THUMBNAIL_WIDTH: u16 = 160;
/// Largest input thumbnail height in pixels.
pub const MAX_INPUT_THUMBNAIL_HEIGHT: u16 = 90;
/// Largest raw (pre-base64url) QOI payload of one `input_thumbnail` record,
/// so a whole thumbnail always fits one record. A sender whose
/// full-size picture encodes larger sends a smaller one: a 128x72 QOI
/// stream never exceeds this bound.
pub const MAX_INPUT_THUMBNAIL_BYTES: usize = 40 * 1024;

/// Turns the input thumbnail feed on or off for this session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputThumbnailSubscription {
    pub enabled: bool,
}

/// One lossy input thumbnail: a whole three-channel QOI stream (sRGB).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputThumbnailMessage {
    pub server: ServerIdentity,
    pub input: WireInputId,
    /// The media list item (`1..=MAX_MEDIA_LIST_ITEM_ID`) the picture
    /// shows, or `None` for the input's own live picture.
    pub item: Option<u32>,
    pub sequence: u64,
    pub origin: MonitorFrameOrigin,
    /// Milliseconds since the server last saw this input's picture change.
    pub age_ms: u32,
    pub width: u16,
    pub height: u16,
    /// Raw QOI bytes; base64url (no padding) on the wire.
    pub data: Vec<u8>,
}

/// One decoded input thumbnail, ready for display.
///
/// `rgba` holds exactly `width * height` straight RGBA8 pixels with alpha
/// `255`, row-major and tightly packed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputThumbnailImage {
    pub input: WireInputId,
    /// The media list item the picture shows, if it is one's.
    pub item: Option<u32>,
    pub sequence: u64,
    pub origin: MonitorFrameOrigin,
    pub age_ms: u32,
    pub width: u16,
    pub height: u16,
    pub rgba: Arc<[u8]>,
}

/// Whether `width`x`height` is a valid input thumbnail size.
#[must_use]
pub const fn input_thumbnail_size_valid(width: u16, height: u16) -> bool {
    width != 0
        && height != 0
        && width <= MAX_INPUT_THUMBNAIL_WIDTH
        && height <= MAX_INPUT_THUMBNAIL_HEIGHT
}

/// Decodes one received thumbnail's QOI stream.
///
/// # Errors
///
/// Returns [`MonitorImageError::InvalidDimensions`] for a size outside the
/// thumbnail bounds, or the QOI decoder's error for a malformed stream.
pub fn decode_input_thumbnail(
    message: &InputThumbnailMessage,
) -> Result<InputThumbnailImage, MonitorImageError> {
    if !input_thumbnail_size_valid(message.width, message.height) {
        return Err(MonitorImageError::InvalidDimensions {
            width: message.width,
            height: message.height,
        });
    }
    let rgba = decode_qoi_rgb(message.width, message.height, &message.data)?;
    Ok(InputThumbnailImage {
        input: message.input,
        item: message.item,
        sequence: message.sequence,
        origin: message.origin,
        age_ms: message.age_ms,
        width: message.width,
        height: message.height,
        rgba: rgba.into(),
    })
}
