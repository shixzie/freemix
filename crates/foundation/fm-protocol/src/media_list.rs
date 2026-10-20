//! Media List input DTOs (IN-006).
//!
//! A media list input is a media input (its transport rides the ordinary
//! `media_*` records) that plays one item of an ordered list at a time. Its
//! replicated list is one [`MediaListStatus`]: the items in authored order,
//! the options, the active item, the loop cycle, and the derived play order
//! with the active item's position in it. The same status rides the
//! `media_list_state` follow-on record of a snapshot and the durable
//! `media_list_changed` event, so a client replaces the whole list of one
//! input on every record that names it.
//!
//! Navigation uses the paged-input vocabulary (`next`, `previous`, `first`,
//! `last`, `goto` a zero-based position) plus `item` (an item id), so a
//! list steps like a slideshow or document.

use crate::WireInputId;

/// Most items one media list carries.
pub const MAX_MEDIA_LIST_ITEMS: usize = 256;
/// Most media list inputs (and so `media_list_state` records after one
/// snapshot).
pub const MAX_MEDIA_LIST_STATES: u16 = 64;
/// Longest item title, in UTF-8 bytes.
pub const MAX_MEDIA_LIST_TITLE_BYTES: usize = 128;
/// Most UTF-8 bytes of every item's file and title together.
pub const MAX_MEDIA_LIST_TEXT_BYTES: usize = 16 * 1024;
/// Largest item id.
pub const MAX_MEDIA_LIST_ITEM_ID: u32 = 1_000_000;

/// One item of a media list.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MediaListItemSpec {
    /// Stable within its list, `1..=MAX_MEDIA_LIST_ITEM_ID`.
    pub id: u32,
    /// The item's `asset://` file. In a command an absolute host path is
    /// also accepted: the authority imports it into the project first.
    pub uri: String,
    pub title: Option<String>,
    /// First source frame played.
    pub mark_in: u64,
    /// Source frame playback stops before; `None` plays to the end.
    pub mark_out: Option<u64>,
}

/// How a media list progresses.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct MediaListOptionsSpec {
    /// The next item plays when the active one ends.
    pub auto_next: bool,
    /// After the last item ends (without loop), the first is cued.
    pub auto_first: bool,
    /// Stepping past the last item wraps to the first, in a new cycle.
    pub loop_list: bool,
    /// Play order is the seeded permutation of each cycle.
    pub shuffle_seed: Option<u64>,
}

/// One step through a list's play order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaListNavigation {
    Next,
    Previous,
    First,
    Last,
    /// Zero-based play-order position, clamped to the last item.
    Goto(u32),
    /// The item with this id.
    Item(u32),
}

impl MediaListNavigation {
    /// The step's token; `goto` and `item` carry an `index` field.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Next => "next",
            Self::Previous => "previous",
            Self::First => "first",
            Self::Last => "last",
            Self::Goto(_) => "goto",
            Self::Item(_) => "item",
        }
    }

    /// The step a token names; `index` must be present exactly for `goto`
    /// and `item`.
    #[must_use]
    pub fn from_token(token: &str, index: Option<u32>) -> Option<Self> {
        match (token, index) {
            ("next", None) => Some(Self::Next),
            ("previous", None) => Some(Self::Previous),
            ("first", None) => Some(Self::First),
            ("last", None) => Some(Self::Last),
            ("goto", Some(index)) => Some(Self::Goto(index)),
            ("item", Some(index)) => Some(Self::Item(index)),
            _ => None,
        }
    }

    /// The step's index, for `goto` and `item`.
    #[must_use]
    pub const fn index(self) -> Option<u32> {
        match self {
            Self::Goto(index) | Self::Item(index) => Some(index),
            _ => None,
        }
    }
}

/// One media list command (wire payload `media_list`).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum MediaListOperation {
    /// Appends a new media list input named `name` (an undoable project
    /// edit).
    Create {
        name: String,
        items: Vec<MediaListItemSpec>,
        options: MediaListOptionsSpec,
    },
    /// Replaces the items and options (a padlocked, undoable project edit).
    /// `active` (Protocol 2.77), one of `items`, becomes the active item:
    /// an undone edit that dropped the active item restores it. `None`
    /// keeps the active item when it is kept, else cues the first item in
    /// play order.
    Set {
        items: Vec<MediaListItemSpec>,
        options: MediaListOptionsSpec,
        active: Option<u32>,
    },
    /// Steps the play order (a live operation).
    Navigate(MediaListNavigation),
    /// Item `from` ended under Auto Next (journaled by the authority).
    Advance { from: u32 },
}

impl MediaListOperation {
    /// The operation's token: `create`, `set`, `navigate`, or `advance`.
    #[must_use]
    pub const fn token(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Set { .. } => "set",
            Self::Navigate(_) => "navigate",
            Self::Advance { .. } => "advance",
        }
    }
}

/// A media list input's replicated list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaListStatus {
    pub input: WireInputId,
    /// Items in authored order.
    pub items: Vec<MediaListItemSpec>,
    pub options: MediaListOptionsSpec,
    /// The id of the item the input plays.
    pub active: u32,
    /// How many times the list has wrapped.
    pub cycle: u64,
    /// Item ids in the current cycle's play order.
    pub order: Vec<u32>,
    /// The active item's zero-based position in `order`.
    pub position: u32,
}

impl MediaListStatus {
    /// The active item.
    #[must_use]
    pub fn active_item(&self) -> Option<&MediaListItemSpec> {
        self.items.iter().find(|item| item.id == self.active)
    }
}

/// One `media_list_state` record after a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaListStateMessage {
    /// The snapshot's revision.
    pub revision: u64,
    pub status: MediaListStatus,
}
