//! Input lifecycle and organisation (Protocol 2.38, IN-026).
//!
//! Every snapshot input carries its [`InputLifecycleStatus`] (category,
//! colour label, paused, closed, and the runtime offline reason) and every
//! snapshot the project's category list. The `input_*` lifecycle commands
//! ([`InputLifecycleOperation`]) are project edits; each accepted one is
//! announced by `event=input_lifecycle_changed` with the inputs it appended
//! and the import items it refused. Whether a source is offline is runtime
//! state, announced by the `input_availability` runtime event.

use crate::WireInputId;

/// Most categories one project defines.
pub const MAX_INPUT_CATEGORIES: usize = 32;
/// Longest category name, in UTF-8 bytes.
pub const MAX_INPUT_CATEGORY_NAME_BYTES: usize = 48;
/// Most items one batch import carries.
pub const MAX_BATCH_IMPORT_ITEMS: usize = 64;
/// Longest source locator (asset URI, device key, endpoint), in UTF-8
/// bytes.
pub const MAX_INPUT_SOURCE_BYTES: usize = 1024;

/// The fixed colour-label palette.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum InputColorLabel {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
}

impl InputColorLabel {
    /// Every label, in palette order.
    pub const ALL: [Self; 8] = [
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Teal,
        Self::Blue,
        Self::Purple,
        Self::Pink,
    ];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Teal => "teal",
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Pink => "pink",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|label| label.token() == token)
    }
}

/// Why an input's source is unavailable (runtime state).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InputOfflineReason {
    /// The media file or asset is missing.
    SourceMissing,
    /// The media file was truncated, replaced, or rewritten under the input.
    SourceChanged,
    /// The source's format cannot be played.
    Unsupported,
    /// Decoding the source failed.
    DecodeFailed,
    /// A required media tool is unavailable.
    ToolUnavailable,
    /// The capture device is disconnected or busy.
    DeviceUnavailable,
    /// The network source is unreachable or stopped sending.
    NetworkUnavailable,
}

impl InputOfflineReason {
    pub const ALL: [Self; 7] = [
        Self::SourceMissing,
        Self::SourceChanged,
        Self::Unsupported,
        Self::DecodeFailed,
        Self::ToolUnavailable,
        Self::DeviceUnavailable,
        Self::NetworkUnavailable,
    ];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::SourceMissing => "source_missing",
            Self::SourceChanged => "source_changed",
            Self::Unsupported => "unsupported",
            Self::DecodeFailed => "decode_failed",
            Self::ToolUnavailable => "tool_unavailable",
            Self::DeviceUnavailable => "device_unavailable",
            Self::NetworkUnavailable => "network_unavailable",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.token() == token)
    }
}

/// One input's lifecycle as clients see it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct InputLifecycleStatus {
    /// The input's category id, from the snapshot's category list.
    pub category: Option<u32>,
    pub color: Option<InputColorLabel>,
    /// The input holds its last frame; its transport or capture is
    /// suspended.
    pub paused: bool,
    /// The input's source is released until it is reopened.
    pub closed: bool,
    /// Why the source is unavailable, while it is (runtime state).
    pub offline: Option<InputOfflineReason>,
}

/// One entry of the project's category list.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct InputCategoryStatus {
    /// Nonzero category id.
    pub id: u32,
    pub name: String,
}

/// The video of a generator source.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WireGeneratorVideo {
    Solid { rgba: [u8; 4] },
    Bars,
}

/// A replaceable input source.
///
/// Wire form: `solid:<r>:<g>:<b>:<a>[:tone:<hz>]`, `bars[:tone:<hz>]`,
/// `media:<uri>`, `device:<key>`, or `network:<endpoint>`, where text is
/// percent-escaped.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum WireInputSource {
    Generator {
        video: WireGeneratorVideo,
        tone_hz: Option<u32>,
    },
    /// A project asset URI (`asset://<key>`); the server may also accept a
    /// host path it imports into the project first.
    Media {
        uri: String,
    },
    Device {
        stable_key: String,
    },
    Network {
        endpoint: String,
    },
    /// A system audio capture source (Protocol 2.55, AU-011).
    SystemAudio(crate::SystemAudioConfig),
}

/// One requested item of a batch import.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ImportItemSpec {
    pub input: WireInputId,
    /// The input name; `None` derives it from the file name.
    pub name: Option<String>,
    /// A project asset URI, or a host path the server imports first.
    pub uri: String,
}

/// Why one item of a batch import was refused.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ImportRefusal {
    InvalidUri,
    UnsupportedType,
    Unavailable,
    DuplicateSource,
    DuplicateInput,
    InvalidName,
    DuplicateName,
    TooManyInputs,
}

impl ImportRefusal {
    pub const ALL: [Self; 8] = [
        Self::InvalidUri,
        Self::UnsupportedType,
        Self::Unavailable,
        Self::DuplicateSource,
        Self::DuplicateInput,
        Self::InvalidName,
        Self::DuplicateName,
        Self::TooManyInputs,
    ];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::InvalidUri => "invalid_uri",
            Self::UnsupportedType => "unsupported_type",
            Self::Unavailable => "unavailable",
            Self::DuplicateSource => "duplicate_source",
            Self::DuplicateInput => "duplicate_input",
            Self::InvalidName => "invalid_name",
            Self::DuplicateName => "duplicate_name",
            Self::TooManyInputs => "too_many_inputs",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|refusal| refusal.token() == token)
    }
}

/// One input lifecycle command (Protocol 2.38). Wire payloads:
///
/// | Operation | Payload | Fields |
/// |---|---|---|
/// | [`Self::Duplicate`] | `input_duplicate` | `source`, `input`, optional `name` |
/// | [`Self::ReplaceSource`] | `input_replace_source` | `input`, `source` |
/// | [`Self::SetCategory`] | `input_category_set` | `input`, `category` (`<id>` or `-`) |
/// | [`Self::SetColor`] | `input_color_set` | `input`, `color` (token or `-`) |
/// | [`Self::SetPaused`] | `input_pause_set` | `input`, `paused` |
/// | [`Self::SetClosed`] | `input_close_set` | `input`, `closed` |
/// | [`Self::SetCategories`] | `input_categories_set` | `categories` |
/// | [`Self::Import`] | `input_import_batch` | `items` |
/// | [`Self::RemoveInputs`] | `inputs_remove` | `inputs` |
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum InputLifecycleOperation {
    Duplicate {
        source: WireInputId,
        input: WireInputId,
        name: Option<String>,
    },
    ReplaceSource {
        input: WireInputId,
        source: WireInputSource,
    },
    SetCategory {
        input: WireInputId,
        category: Option<u32>,
    },
    SetColor {
        input: WireInputId,
        color: Option<InputColorLabel>,
    },
    SetPaused {
        input: WireInputId,
        paused: bool,
    },
    SetClosed {
        input: WireInputId,
        closed: bool,
    },
    SetCategories {
        categories: Vec<InputCategoryStatus>,
    },
    Import {
        items: Vec<ImportItemSpec>,
    },
    RemoveInputs {
        inputs: Vec<WireInputId>,
    },
}

impl InputLifecycleOperation {
    /// The wire payload name.
    #[must_use]
    pub const fn payload(&self) -> &'static str {
        match self {
            Self::Duplicate { .. } => "input_duplicate",
            Self::ReplaceSource { .. } => "input_replace_source",
            Self::SetCategory { .. } => "input_category_set",
            Self::SetColor { .. } => "input_color_set",
            Self::SetPaused { .. } => "input_pause_set",
            Self::SetClosed { .. } => "input_close_set",
            Self::SetCategories { .. } => "input_categories_set",
            Self::Import { .. } => "input_import_batch",
            Self::RemoveInputs { .. } => "inputs_remove",
        }
    }
}

/// An appended input: its id and name.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AddedInput {
    pub input: WireInputId,
    pub name: String,
}
