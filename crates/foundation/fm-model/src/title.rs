//! Title inputs: a persisted reference to a stored title template plus the
//! operator's typed field values.
//!
//! The template itself (layout, fonts, animation) lives in the project bundle
//! as an asset and is owned by the persistence layer; the model keeps only
//! what the operator edits live. Field ids are the template's field ids, kept
//! strictly ascending so a project has one canonical encoding.

use core::{fmt, num::NonZeroU128};

use crate::Rgba8;

/// Most fields one title input may carry.
pub const MAX_TITLE_FIELDS: usize = 128;
/// Longest text field value, in Unicode scalar values.
pub const MAX_TITLE_TEXT_CHARS: usize = 4_096;
/// Largest total of text values plus image asset URIs, in UTF-8 bytes.
pub const MAX_TITLE_CONTENT_BYTES: usize = 12_288;

/// The stable id of a field in a title template.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TitleFieldId(NonZeroU128);

impl TitleFieldId {
    #[must_use]
    pub const fn new(value: NonZeroU128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> NonZeroU128 {
        self.0
    }
}

impl From<NonZeroU128> for TitleFieldId {
    fn from(value: NonZeroU128) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for TitleFieldId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One typed title field value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleFieldValue {
    Text(String),
    Number(i64),
    /// A straight (not premultiplied) RGBA color.
    Color(Rgba8),
    /// An image held in the project bundle.
    Image {
        asset_uri: String,
    },
}

impl TitleFieldValue {
    /// UTF-8 bytes this value contributes to [`MAX_TITLE_CONTENT_BYTES`].
    #[must_use]
    pub fn content_bytes(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Image { asset_uri } => asset_uri.len(),
            Self::Number(_) | Self::Color(_) => 0,
        }
    }
}

/// One field id and its value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleFieldEntry {
    pub id: TitleFieldId,
    pub value: TitleFieldValue,
}

/// A title input: the stored template it instantiates and its live fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleInput {
    /// `asset://` URI of the stored `.fmtitle` template.
    pub template_uri: String,
    /// Optimistic-concurrency revision of `fields`; it never decreases.
    pub field_revision: u64,
    /// Field values sorted by strictly ascending id.
    pub fields: Vec<TitleFieldEntry>,
}

/// Why a title field list is refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleFieldsError {
    /// More than [`MAX_TITLE_FIELDS`] fields.
    TooManyFields { count: usize },
    /// The same id appears twice.
    DuplicateField(TitleFieldId),
    /// Ids are not strictly ascending.
    OutOfOrder {
        previous: TitleFieldId,
        next: TitleFieldId,
    },
    /// A text value exceeds [`MAX_TITLE_TEXT_CHARS`].
    TextTooLong { field: TitleFieldId, chars: usize },
    /// An image value names a blank asset URI.
    BlankImageUri(TitleFieldId),
    /// Text and image URIs together exceed [`MAX_TITLE_CONTENT_BYTES`].
    ContentTooLarge { bytes: usize },
}

impl fmt::Display for TitleFieldsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyFields { count } => write!(
                formatter,
                "title has {count} fields, maximum is {MAX_TITLE_FIELDS}"
            ),
            Self::DuplicateField(field) => write!(formatter, "title field {field} repeats"),
            Self::OutOfOrder { previous, next } => write!(
                formatter,
                "title field {next} follows {previous}; ids must be strictly ascending"
            ),
            Self::TextTooLong { field, chars } => write!(
                formatter,
                "title field {field} has {chars} characters, maximum is {MAX_TITLE_TEXT_CHARS}"
            ),
            Self::BlankImageUri(field) => {
                write!(formatter, "title field {field} names a blank image asset")
            }
            Self::ContentTooLarge { bytes } => write!(
                formatter,
                "title content is {bytes} bytes, maximum is {MAX_TITLE_CONTENT_BYTES}"
            ),
        }
    }
}

impl std::error::Error for TitleFieldsError {}

/// Checks the bounds, ordering, and image URIs of a title field list.
///
/// # Errors
///
/// Returns the first [`TitleFieldsError`] found.
pub fn validate_title_fields(fields: &[TitleFieldEntry]) -> Result<(), TitleFieldsError> {
    if fields.len() > MAX_TITLE_FIELDS {
        return Err(TitleFieldsError::TooManyFields {
            count: fields.len(),
        });
    }
    for pair in fields.windows(2) {
        let (previous, next) = (pair[0].id, pair[1].id);
        if previous == next {
            return Err(TitleFieldsError::DuplicateField(next));
        }
        if previous > next {
            return Err(TitleFieldsError::OutOfOrder { previous, next });
        }
    }
    let mut bytes = 0_usize;
    for entry in fields {
        match &entry.value {
            TitleFieldValue::Text(text) => {
                let chars = text.chars().count();
                if chars > MAX_TITLE_TEXT_CHARS {
                    return Err(TitleFieldsError::TextTooLong {
                        field: entry.id,
                        chars,
                    });
                }
            }
            TitleFieldValue::Image { asset_uri } if asset_uri.trim().is_empty() => {
                return Err(TitleFieldsError::BlankImageUri(entry.id));
            }
            TitleFieldValue::Image { .. }
            | TitleFieldValue::Number(_)
            | TitleFieldValue::Color(_) => {}
        }
        bytes = bytes.saturating_add(entry.value.content_bytes());
    }
    if bytes > MAX_TITLE_CONTENT_BYTES {
        return Err(TitleFieldsError::ContentTooLarge { bytes });
    }
    Ok(())
}

/// Refusal for [`crate::Project::set_title_fields`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetTitleFieldsError {
    UnknownInput(fm_types::InputId),
    NotTitleInput(fm_types::InputId),
    /// The requested revision is older than the stored one.
    RevisionRegressed {
        current: u64,
        requested: u64,
    },
    InvalidFields(TitleFieldsError),
}

impl fmt::Display for SetTitleFieldsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "input {input} does not exist"),
            Self::NotTitleInput(input) => write!(formatter, "input {input} is not a title input"),
            Self::RevisionRegressed { current, requested } => write!(
                formatter,
                "title field revision {requested} is older than the stored revision {current}"
            ),
            Self::InvalidFields(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SetTitleFieldsError {}

/// Where a title input is shown, for its timeline (GX-001, schema 65).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TitleShownOn {
    /// On air: on Program (either side of a transition) or on an overlay
    /// channel.
    Air,
    /// On Preview and not on air.
    Preview,
}

/// The frame a shown title's timeline started on (GX-001, schema 65).
///
/// A title's tickers, scene clocks, and since-take clocks count from the
/// engine frame it was taken to Program, an overlay channel, or Preview,
/// not from the start of the show. The engine owns this runtime state; a
/// project carries it so a restored show (a checkpoint followed by journal
/// replay) continues every timeline exactly where it was.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TitleTimeline {
    pub on: TitleShownOn,
    /// The engine frame the title was taken to `on`.
    pub since_frame: u64,
}
