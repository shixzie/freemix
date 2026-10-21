//! Media List inputs (IN-006): one media input that plays an ordered list of
//! video and audio files, one item at a time.
//!
//! A list input is an ordinary [`crate::InputKind::Media`] input whose
//! `asset_uri` is always the *active* item's file, plus one [`MediaList`] in
//! [`crate::Project::media_lists`]. Activating an item swaps the input's
//! source to that item's file and its trim to the item's marks, so the
//! media transport, its decoder, thumbnails, and audio all follow the list
//! without a separate playback path.
//!
//! What is persisted: the items in authored order (each with a stable id, an
//! `asset://` file, an optional title, and its in/out marks), the options
//! (Auto Next, Auto First, loop, an optional shuffle seed, and the
//! transition between items), the active item, and the loop cycle. The *play order* is derived: the authored order,
//! or with a shuffle seed a deterministic permutation of it for the current
//! cycle, so a show replayed from its journal shuffles identically.
//!
//! [`parse_m3u`] reads an M3U/M3U8 playlist into host paths and titles.

use core::fmt;
use std::path::Path;

/// Most items one media list holds.
pub const MAX_MEDIA_LIST_ITEMS: usize = 256;
/// Most UTF-8 bytes of every item's file and title together, so a whole
/// list fits one protocol record.
pub const MAX_MEDIA_LIST_TEXT_BYTES: usize = 16 * 1024;
/// Most bytes every item's file and title together take once escaped for
/// the wire ([`media_list_wire_text_bytes`]). The protocol escapes an
/// item's text inside its `items` field and then the field inside its line,
/// so a byte other than an ASCII letter, digit, `-`, `_`, or `.` costs five
/// bytes; this bound keeps a whole list, with every item's id and marks and
/// its play order, inside one protocol line.
pub const MAX_MEDIA_LIST_WIRE_TEXT_BYTES: usize = 40 * 1024;
/// Most media list inputs one project holds.
pub const MAX_MEDIA_LISTS: usize = 64;
/// Longest item title, in UTF-8 bytes.
pub const MAX_MEDIA_LIST_TITLE_BYTES: usize = 128;
/// Largest item id.
pub const MAX_MEDIA_LIST_ITEM_ID: u32 = 1_000_000;
/// Longest transition between two items, in output frames.
pub const MAX_MEDIA_LIST_TRANSITION_FRAMES: u32 = 600;
/// The transition length a new list starts with, in output frames.
pub const DEFAULT_MEDIA_LIST_TRANSITION_FRAMES: u32 = 15;

/// How one item gives way to the next.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum MediaListTransitionKind {
    /// The next item replaces the active one on the step's frame.
    #[default]
    Cut,
    /// The two items cross-fade (a mix).
    Fade,
    /// The active item fades to black over the first half, and the next
    /// item fades up from black over the second half.
    Dip,
    /// The next item wipes in from the left edge to the right.
    Wipe,
}

impl MediaListTransitionKind {
    /// Every kind, in operator order.
    pub const ALL: [Self; 4] = [Self::Cut, Self::Fade, Self::Dip, Self::Wipe];

    /// `cut`, `fade`, `dip`, or `wipe`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Cut => "cut",
            Self::Fade => "fade",
            Self::Dip => "dip",
            Self::Wipe => "wipe",
        }
    }

    /// The kind a token names.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.token() == token)
    }
}

/// The transition a list plays between two items when it steps while
/// playing (Auto Next, or Next, Previous, and the other steps): the two
/// items are composited for `duration_frames` output frames, the outgoing
/// one still playing. Under Auto Next the step comes `duration_frames`
/// before the active item's out point, so the items overlap instead of
/// the outgoing item holding its last frame.
///
/// The duration is kept, but unused, by a cut.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MediaListTransition {
    pub kind: MediaListTransitionKind,
    /// `1..=`[`MAX_MEDIA_LIST_TRANSITION_FRAMES`].
    pub duration_frames: u32,
}

impl Default for MediaListTransition {
    fn default() -> Self {
        Self::CUT
    }
}

impl MediaListTransition {
    /// A cut, keeping the default duration for a later blend.
    pub const CUT: Self = Self {
        kind: MediaListTransitionKind::Cut,
        duration_frames: DEFAULT_MEDIA_LIST_TRANSITION_FRAMES,
    };

    /// Whether the duration is in bounds.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.duration_frames >= 1 && self.duration_frames <= MAX_MEDIA_LIST_TRANSITION_FRAMES
    }

    /// The frames the two items are composited for: zero for a cut.
    #[must_use]
    pub const fn blend_frames(self) -> u32 {
        match self.kind {
            MediaListTransitionKind::Cut => 0,
            _ => self.duration_frames,
        }
    }

    /// The incoming item's weight, in basis points, on the `step`th
    /// composited frame (`1..=duration_frames`): `step * 10_000 /
    /// (duration_frames + 1)`, so neither end of a blend repeats a whole
    /// picture; 10 000 after the blend and for a cut.
    #[must_use]
    pub fn mix_basis_points(self, step: u32) -> u16 {
        let frames = self.blend_frames();
        if frames == 0 || step > frames {
            return 10_000;
        }
        let mix = u64::from(step) * 10_000 / (u64::from(frames) + 1);
        u16::try_from(mix).unwrap_or(10_000)
    }
}

/// One file of a media list.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MediaListItem {
    /// Stable within its list, `1..=MAX_MEDIA_LIST_ITEM_ID`.
    pub id: u32,
    /// The item's `asset://` media file (video or audio, not a still).
    pub asset_uri: String,
    /// A display title (from `#EXTINF`, or set by the operator).
    pub title: Option<String>,
    /// First source frame played (inclusive).
    pub mark_in: u64,
    /// Source frame playback stops before (exclusive); `None` plays to the
    /// end of the file.
    pub mark_out: Option<u64>,
}

impl MediaListItem {
    /// An unmarked, untitled item.
    #[must_use]
    pub fn new(id: u32, asset_uri: impl Into<String>) -> Self {
        Self {
            id,
            asset_uri: asset_uri.into(),
            title: None,
            mark_in: 0,
            mark_out: None,
        }
    }

    /// The name the operator sees: the title, else the file's stem.
    #[must_use]
    pub fn display_name(&self) -> String {
        self.title
            .clone()
            .unwrap_or_else(|| crate::media_input_name(&self.asset_uri))
    }
}

/// How a media list progresses.
///
/// * `auto_next`: when an item's clip ends, the next item in play order
///   starts playing.
/// * `auto_first`: when the last item ends under Auto Next without loop, the
///   first item is cued, stopped, instead of holding the last frame.
/// * `loop_list`: stepping past the last item (automatically or by Next)
///   wraps to the first and starts a new cycle; Previous on the first item
///   wraps to the last.
/// * `shuffle_seed`: play order is a permutation derived from the seed and
///   the cycle; each wrap reshuffles.
/// * `transition`: how a playing list steps from one item to the next.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct MediaListOptions {
    pub auto_next: bool,
    pub auto_first: bool,
    pub loop_list: bool,
    pub shuffle_seed: Option<u64>,
    pub transition: MediaListTransition,
}

/// A media list input's persisted list.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MediaList {
    /// Items in authored order.
    pub items: Vec<MediaListItem>,
    pub options: MediaListOptions,
    /// The id of the item the input plays.
    pub active: u32,
    /// The loop cycle: how many times the list has wrapped past its end.
    pub cycle: u64,
}

impl MediaList {
    /// A list on its first item, cycle zero.
    ///
    /// # Errors
    ///
    /// Returns a [`MediaListError`] for items that break a bound.
    pub fn new(
        items: Vec<MediaListItem>,
        options: MediaListOptions,
    ) -> Result<Self, MediaListError> {
        validate_media_list_items(&items)?;
        validate_media_list_options(options)?;
        let mut list = Self {
            active: items[0].id,
            items,
            options,
            cycle: 0,
        };
        list.active = list.play_order()[0];
        Ok(list)
    }

    /// The item with id `id`.
    #[must_use]
    pub fn item(&self, id: u32) -> Option<&MediaListItem> {
        self.items.iter().find(|item| item.id == id)
    }

    /// The active item.
    #[must_use]
    pub fn active_item(&self) -> Option<&MediaListItem> {
        self.item(self.active)
    }

    /// Item ids in the current cycle's play order.
    #[must_use]
    pub fn play_order(&self) -> Vec<u32> {
        media_list_play_order(self.items.len(), self.options.shuffle_seed, self.cycle)
            .into_iter()
            .map(|index| self.items[index].id)
            .collect()
    }

    /// The active item's zero-based position in play order.
    #[must_use]
    pub fn position(&self) -> Option<usize> {
        self.play_order().iter().position(|id| *id == self.active)
    }

    /// The smallest id not used by any item.
    #[must_use]
    pub fn next_item_id(&self) -> u32 {
        self.items.iter().map(|item| item.id).max().unwrap_or(0) + 1
    }

    /// Checks every bound.
    ///
    /// # Errors
    ///
    /// Returns the first [`MediaListError`].
    pub fn validate(&self) -> Result<(), MediaListError> {
        validate_media_list_items(&self.items)?;
        validate_media_list_options(self.options)?;
        if self.item(self.active).is_none() {
            return Err(MediaListError::UnknownActiveItem(self.active));
        }
        Ok(())
    }
}

/// The play order of `len` items: authored order, or with `seed` a
/// Fisher-Yates permutation driven by `SplitMix64` seeded from the seed and
/// `cycle`. A pure function: the same arguments always give the same order.
#[must_use]
pub fn media_list_play_order(len: usize, seed: Option<u64>, cycle: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    let Some(seed) = seed else {
        return order;
    };
    let mut state = seed ^ cycle.wrapping_mul(0xD1B5_4A32_D192_ED03);
    for index in (1..len).rev() {
        let bound = u64::try_from(index + 1).unwrap_or(u64::MAX);
        let pick = usize::try_from(splitmix64(&mut state) % bound).unwrap_or(0);
        order.swap(index, pick);
    }
    order
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

/// Why a media list is unusable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaListError {
    Empty,
    TooManyItems {
        count: usize,
    },
    InvalidItemId(u32),
    DuplicateItem(u32),
    /// The item's file is not an `asset://` URI.
    InvalidAsset(u32),
    /// The item's file is not a video or audio file (stills are refused).
    UnsupportedType(u32),
    InvalidTitle(u32),
    InvalidMarks(u32),
    UnknownActiveItem(u32),
    /// The items' files and titles together exceed
    /// [`MAX_MEDIA_LIST_TEXT_BYTES`].
    TooMuchText,
    /// The transition's duration is outside
    /// `1..=`[`MAX_MEDIA_LIST_TRANSITION_FRAMES`].
    InvalidTransition(u32),
}

impl MediaListError {
    /// The stable code of the error on the wire.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Empty | Self::TooManyItems { .. } | Self::TooMuchText => "invalid_items",
            Self::InvalidItemId(_) => "invalid_item_id",
            Self::DuplicateItem(_) => "duplicate_item",
            Self::InvalidAsset(_) => "invalid_uri",
            Self::UnsupportedType(_) => "unsupported_type",
            Self::InvalidTitle(_) => "invalid_title",
            Self::InvalidMarks(_) => "invalid_marks",
            Self::UnknownActiveItem(_) => "unknown_item",
            Self::InvalidTransition(_) => "invalid_transition",
        }
    }
}

impl fmt::Display for MediaListError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("a media list holds at least one item"),
            Self::TooManyItems { count } => write!(
                formatter,
                "a media list holds at most {MAX_MEDIA_LIST_ITEMS} items, not {count}"
            ),
            Self::InvalidItemId(id) => write!(
                formatter,
                "media list item id {id} is outside 1..={MAX_MEDIA_LIST_ITEM_ID}"
            ),
            Self::DuplicateItem(id) => write!(formatter, "media list item {id} is repeated"),
            Self::InvalidAsset(id) => {
                write!(
                    formatter,
                    "media list item {id} is not a project asset: name an asset:// file \
                     or an absolute path to a file on the server's computer, which is \
                     copied in"
                )
            }
            Self::UnsupportedType(id) => write!(
                formatter,
                "media list item {id} is not a video or audio file"
            ),
            Self::InvalidTitle(id) => write!(
                formatter,
                "media list item {id}'s title is blank, too long, or has control characters"
            ),
            Self::InvalidMarks(id) => write!(
                formatter,
                "media list item {id}'s out mark must follow its in mark within the frame limit"
            ),
            Self::UnknownActiveItem(id) => {
                write!(formatter, "the media list has no item {id}")
            }
            Self::TooMuchText => write!(
                formatter,
                "a media list's file names and titles hold at most {MAX_MEDIA_LIST_TEXT_BYTES} bytes, \
                 and at most {MAX_MEDIA_LIST_WIRE_TEXT_BYTES} once escaped for the wire"
            ),
            Self::InvalidTransition(frames) => write!(
                formatter,
                "a media list transition of {frames} frames is outside \
                 1..={MAX_MEDIA_LIST_TRANSITION_FRAMES}"
            ),
        }
    }
}

impl std::error::Error for MediaListError {}

/// Whether `path` names a file a media list plays: a media file that is not
/// a still image.
#[must_use]
pub fn is_media_list_file_name(path: &str) -> bool {
    crate::is_media_file_name(path) && fm_types::media_list_files::is_media_list_file_name(path)
}

/// The bytes `text` (an item's file or title) takes on the wire, escaped
/// twice: once inside the `items` field and once inside the protocol line.
#[must_use]
pub fn media_list_wire_text_bytes(text: &str) -> usize {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                1
            } else {
                5
            }
        })
        .sum()
}

/// Checks a title's bounds.
#[must_use]
pub fn media_list_title_is_valid(title: &str) -> bool {
    !title.trim().is_empty()
        && title.len() <= MAX_MEDIA_LIST_TITLE_BYTES
        && !title.chars().any(char::is_control)
}

/// Checks options' bounds: the transition's duration.
///
/// # Errors
///
/// Returns [`MediaListError::InvalidTransition`].
pub const fn validate_media_list_options(options: MediaListOptions) -> Result<(), MediaListError> {
    if options.transition.is_valid() {
        Ok(())
    } else {
        Err(MediaListError::InvalidTransition(
            options.transition.duration_frames,
        ))
    }
}

/// Checks items' bounds: count, unique ids in range, `asset://` video or
/// audio files, titles, and marks.
///
/// # Errors
///
/// Returns the first [`MediaListError`].
pub fn validate_media_list_items(items: &[MediaListItem]) -> Result<(), MediaListError> {
    if items.is_empty() {
        return Err(MediaListError::Empty);
    }
    if items.len() > MAX_MEDIA_LIST_ITEMS {
        return Err(MediaListError::TooManyItems { count: items.len() });
    }
    let text: usize = items
        .iter()
        .map(|item| item.asset_uri.len() + item.title.as_ref().map_or(0, String::len))
        .sum();
    let wire: usize = items
        .iter()
        .map(|item| {
            media_list_wire_text_bytes(&item.asset_uri)
                + item.title.as_deref().map_or(0, |title| {
                    // A title of exactly `-` is written escaped (`%2D`).
                    if title == "-" {
                        5
                    } else {
                        media_list_wire_text_bytes(title)
                    }
                })
        })
        .sum();
    if text > MAX_MEDIA_LIST_TEXT_BYTES || wire > MAX_MEDIA_LIST_WIRE_TEXT_BYTES {
        return Err(MediaListError::TooMuchText);
    }
    for (index, item) in items.iter().enumerate() {
        if item.id == 0 || item.id > MAX_MEDIA_LIST_ITEM_ID {
            return Err(MediaListError::InvalidItemId(item.id));
        }
        if items[..index].iter().any(|previous| previous.id == item.id) {
            return Err(MediaListError::DuplicateItem(item.id));
        }
        if crate::validate_media_asset_uri(&item.asset_uri).is_err() {
            return Err(MediaListError::InvalidAsset(item.id));
        }
        if !is_media_list_file_name(&item.asset_uri) {
            return Err(MediaListError::UnsupportedType(item.id));
        }
        if item
            .title
            .as_deref()
            .is_some_and(|title| !media_list_title_is_valid(title))
        {
            return Err(MediaListError::InvalidTitle(item.id));
        }
        if item.mark_in > crate::MAX_MEDIA_FRAME
            || item
                .mark_out
                .is_some_and(|out| out <= item.mark_in || out > crate::MAX_MEDIA_FRAME)
        {
            return Err(MediaListError::InvalidMarks(item.id));
        }
    }
    Ok(())
}

pub use fm_types::media_list_files::{M3uEntry, MAX_M3U_BYTES, is_m3u_file_name};

/// Reads an M3U or M3U8 playlist ([`fm_types::media_list_files::parse_m3u`])
/// with the media list bounds: at most [`MAX_MEDIA_LIST_ITEMS`] entries and
/// titles of at most [`MAX_MEDIA_LIST_TITLE_BYTES`].
#[must_use]
pub fn parse_m3u(text: &str, base: &Path) -> Vec<M3uEntry> {
    fm_types::media_list_files::parse_m3u(
        text,
        base,
        MAX_MEDIA_LIST_ITEMS,
        MAX_MEDIA_LIST_TITLE_BYTES,
    )
}
