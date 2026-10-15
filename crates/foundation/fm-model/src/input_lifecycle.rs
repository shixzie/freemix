//! Input lifecycle and organisation (schema 37, IN-026).
//!
//! Every input may carry a category from the project's category list and a
//! colour label from a fixed palette, and may be *paused* (it holds its last
//! frame and its media transport or capture is suspended) or *closed* (its
//! source is released while its definition, labels, strip, and references
//! are kept; reopening restores it). Whether a source is *offline* (its
//! device, file, or stream is unavailable) is runtime state and is never
//! persisted.
//!
//! Inputs without category, colour, pause, or close keep no entry, so a
//! project's lifecycle map holds only inputs that differ from the default.
//!
//! The module also defines [`InputSourceSpec`], the input sources that can
//! be named on the wire and replaced live, and [`plan_batch_import`], the
//! per-item validation of a batch media import.

use core::{fmt, num::NonZeroU32};
use std::collections::BTreeSet;

use fm_types::{InputId, MAX_INPUT_NAME_BYTES};

use crate::{InputKind, MediaPlaybackSettings, SimulatedAudio, SimulatedInput, SimulatedVideo};

/// Most categories one project defines.
pub const MAX_INPUT_CATEGORIES: usize = 32;
/// Longest category name, in UTF-8 bytes.
pub const MAX_INPUT_CATEGORY_NAME_BYTES: usize = 48;
/// Most items one batch import carries.
pub const MAX_BATCH_IMPORT_ITEMS: usize = 64;
/// Longest source locator (asset URI, device key, network endpoint), in
/// UTF-8 bytes. Equals the project asset URI bound.
pub const MAX_INPUT_SOURCE_BYTES: usize = 1024;
/// Prefix of every project asset URI.
pub const MEDIA_ASSET_URI_PREFIX: &str = "asset://";
/// File extensions (lower case) a batch import accepts as media.
pub const MEDIA_FILE_EXTENSIONS: [&str; 26] = [
    "mp4", "m4v", "mov", "mkv", "mxf", "ts", "m2ts", "mts", "webm", "avi", "nut", "mpg", "mpeg",
    "wav", "mp3", "aac", "m4a", "flac", "ogg", "png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff",
];

/// A project-local category identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InputCategoryId(NonZeroU32);

impl InputCategoryId {
    /// The identifier `value`, or `None` for zero.
    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for InputCategoryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// One entry of the project's category list.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct InputCategory {
    pub id: InputCategoryId,
    pub name: String,
}

impl InputCategory {
    #[must_use]
    pub fn new(id: InputCategoryId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
        }
    }
}

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

    /// The stable token of the label in `project.json` and on the wire.
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

    /// The label named by `token`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|label| label.token() == token)
    }

    /// The label's display colour as sRGB.
    #[must_use]
    pub const fn rgb(self) -> [u8; 3] {
        match self {
            Self::Red => [0xE5, 0x48, 0x4D],
            Self::Orange => [0xF7, 0x6B, 0x15],
            Self::Yellow => [0xF5, 0xD9, 0x0A],
            Self::Green => [0x30, 0xA4, 0x6C],
            Self::Teal => [0x12, 0xA5, 0x94],
            Self::Blue => [0x00, 0x90, 0xFF],
            Self::Purple => [0x8E, 0x4E, 0xC6],
            Self::Pink => [0xD6, 0x40, 0x9F],
        }
    }
}

/// One input's persisted lifecycle and organisation.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct InputLifecycle {
    pub category: Option<InputCategoryId>,
    pub color: Option<InputColorLabel>,
    /// The input holds its last frame; its transport or capture is
    /// suspended.
    pub paused: bool,
    /// The input's source is released; reopening restores it.
    pub closed: bool,
}

impl InputLifecycle {
    /// Whether the input carries nothing beyond the default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// A category list or lifecycle assignment that breaks a bound or a
/// reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputLifecycleError {
    UnknownInput(InputId),
    UnknownCategory(InputCategoryId),
    TooManyCategories {
        count: usize,
    },
    DuplicateCategoryId(InputCategoryId),
    DuplicateCategoryName(String),
    EmptyCategoryName,
    CategoryNameTooLong {
        bytes: usize,
    },
    CategoryNameControlCharacter,
    /// A category removed from the list is still assigned to an input.
    CategoryInUse {
        category: InputCategoryId,
        input: InputId,
    },
}

impl InputLifecycleError {
    /// The stable code of the error on the wire.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownInput(_) => "unknown_input",
            Self::UnknownCategory(_) => "unknown_category",
            Self::TooManyCategories { .. } => "too_many_categories",
            Self::DuplicateCategoryId(_) => "duplicate_category",
            Self::DuplicateCategoryName(_) => "duplicate_category_name",
            Self::EmptyCategoryName
            | Self::CategoryNameTooLong { .. }
            | Self::CategoryNameControlCharacter => "invalid_category_name",
            Self::CategoryInUse { .. } => "category_in_use",
        }
    }
}

impl fmt::Display for InputLifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "input {input} does not exist"),
            Self::UnknownCategory(category) => {
                write!(formatter, "category {category} does not exist")
            }
            Self::TooManyCategories { count } => write!(
                formatter,
                "{count} categories exceed the limit of {MAX_INPUT_CATEGORIES}"
            ),
            Self::DuplicateCategoryId(category) => {
                write!(formatter, "category {category} is listed twice")
            }
            Self::DuplicateCategoryName(name) => {
                write!(formatter, "category name `{name}` is listed twice")
            }
            Self::EmptyCategoryName => formatter.write_str("category name must not be empty"),
            Self::CategoryNameTooLong { bytes } => write!(
                formatter,
                "category name has {bytes} bytes; the limit is {MAX_INPUT_CATEGORY_NAME_BYTES}"
            ),
            Self::CategoryNameControlCharacter => {
                formatter.write_str("category name must not contain control characters")
            }
            Self::CategoryInUse { category, input } => write!(
                formatter,
                "category {category} is still assigned to input {input}"
            ),
        }
    }
}

impl std::error::Error for InputLifecycleError {}

/// Validates one category name.
///
/// # Errors
///
/// Returns an empty, too long, or control-character name error.
pub fn validate_input_category_name(name: &str) -> Result<(), InputLifecycleError> {
    if name.trim().is_empty() {
        Err(InputLifecycleError::EmptyCategoryName)
    } else if name.len() > MAX_INPUT_CATEGORY_NAME_BYTES {
        Err(InputLifecycleError::CategoryNameTooLong { bytes: name.len() })
    } else if name.chars().any(char::is_control) {
        Err(InputLifecycleError::CategoryNameControlCharacter)
    } else {
        Ok(())
    }
}

/// Validates a category list: its bound, each name, and unique ids and
/// names (names compare case-insensitively).
///
/// # Errors
///
/// Returns the first violation found, in list order.
pub fn validate_input_categories(categories: &[InputCategory]) -> Result<(), InputLifecycleError> {
    if categories.len() > MAX_INPUT_CATEGORIES {
        return Err(InputLifecycleError::TooManyCategories {
            count: categories.len(),
        });
    }
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for category in categories {
        validate_input_category_name(&category.name)?;
        if !ids.insert(category.id) {
            return Err(InputLifecycleError::DuplicateCategoryId(category.id));
        }
        if !names.insert(category.name.to_lowercase()) {
            return Err(InputLifecycleError::DuplicateCategoryName(
                category.name.clone(),
            ));
        }
    }
    Ok(())
}

/// The video of a generator source.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GeneratorVideo {
    Solid { rgba: [u8; 4] },
    Bars,
}

/// An input source that can be named on the wire and swapped live: a
/// generator, a media file, a capture device, or a network stream.
///
/// Scene, alias, title, mix-feed, and plain colour inputs have no
/// replaceable source.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum InputSourceSpec {
    /// A simulated generator with a silent or sine-tone audio track.
    Generator {
        video: GeneratorVideo,
        tone_hz: Option<u32>,
    },
    /// A media file in the project's assets (`asset://<key>`).
    Media { asset_uri: String },
    /// A capture device by its adapter-qualified stable key.
    Device { stable_key: String },
    /// A network source by its endpoint.
    Network { endpoint: String },
}

/// Why a source specification is unusable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InputSourceError {
    /// The locator is empty, too long, or holds control characters.
    InvalidLocator,
    /// A media source is not a strict `asset://` URI.
    InvalidAssetUri,
    /// A media source's extension is not a supported media type.
    UnsupportedMediaType,
    /// A sine tone outside the simulated frequency range.
    InvalidTone,
}

impl InputSourceError {
    /// The stable code of the error on the wire.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLocator => "invalid_source",
            Self::InvalidAssetUri => "invalid_uri",
            Self::UnsupportedMediaType => "unsupported_type",
            Self::InvalidTone => "invalid_tone",
        }
    }
}

impl fmt::Display for InputSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLocator => {
                "the source locator is empty, too long, or contains control characters"
            }
            Self::InvalidAssetUri => "a media source must be a project asset:// URI",
            Self::UnsupportedMediaType => "the file type is not a supported media type",
            Self::InvalidTone => "the tone frequency is outside the simulated range",
        })
    }
}

impl std::error::Error for InputSourceError {}

/// Validates the strict syntax of a project `asset://` URI: a nonempty
/// relative key of nonempty segments, without `.`/`..` segments, a leading
/// slash, backslashes, `?`, `#`, `%`, `:`, or control characters.
///
/// # Errors
///
/// Returns [`InputSourceError::InvalidAssetUri`].
pub fn validate_media_asset_uri(uri: &str) -> Result<(), InputSourceError> {
    if uri.len() > MAX_INPUT_SOURCE_BYTES {
        return Err(InputSourceError::InvalidAssetUri);
    }
    let Some(key) = uri.strip_prefix(MEDIA_ASSET_URI_PREFIX) else {
        return Err(InputSourceError::InvalidAssetUri);
    };
    if key.is_empty()
        || key.starts_with('/')
        || key.contains(['\\', '\0', '?', '#', '%', ':'])
        || key.chars().any(char::is_control)
        || key
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(InputSourceError::InvalidAssetUri);
    }
    Ok(())
}

/// Whether `path` (a URI, key, or host path) names a supported media type
/// by its extension.
#[must_use]
pub fn is_media_file_name(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty()
            && MEDIA_FILE_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
    })
}

/// The input name a media file suggests: its file name without the
/// extension, trimmed and bounded, or `Media`.
#[must_use]
pub fn media_input_name(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem).trim();
    let stem: String = stem.chars().filter(|c| !c.is_control()).collect();
    if stem.trim().is_empty() {
        return "Media".to_owned();
    }
    truncate_to(stem.trim(), MAX_INPUT_NAME_BYTES)
}

fn truncate_to(text: &str, maximum: usize) -> String {
    if text.len() <= maximum {
        return text.to_owned();
    }
    let mut end = maximum;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].trim_end().to_owned()
}

/// The first of `base`, `base 2`, `base 3`, ... that `taken` does not hold,
/// each bounded to the input name limit.
#[must_use]
pub fn unique_input_name(base: &str, taken: impl Fn(&str) -> bool) -> Option<String> {
    let base = truncate_to(base, MAX_INPUT_NAME_BYTES);
    if !taken(&base) && !base.trim().is_empty() {
        return Some(base);
    }
    (2..=999_u32).find_map(|suffix| {
        let suffix = format!(" {suffix}");
        let candidate = format!(
            "{}{suffix}",
            truncate_to(&base, MAX_INPUT_NAME_BYTES - suffix.len())
        );
        (!taken(&candidate)).then_some(candidate)
    })
}

fn validate_locator(locator: &str) -> Result<(), InputSourceError> {
    if locator.trim().is_empty()
        || locator.len() > MAX_INPUT_SOURCE_BYTES
        || locator.chars().any(char::is_control)
    {
        Err(InputSourceError::InvalidLocator)
    } else {
        Ok(())
    }
}

impl InputSourceSpec {
    /// Validates the locator, asset URI, media type, and tone.
    ///
    /// # Errors
    ///
    /// Returns the [`InputSourceError`] that makes the source unusable.
    pub fn validate(&self) -> Result<(), InputSourceError> {
        match self {
            Self::Generator { tone_hz, .. } => match tone_hz {
                Some(hz)
                    if !(SimulatedAudio::MIN_SINE_FREQUENCY_HZ
                        ..=SimulatedAudio::MAX_SINE_FREQUENCY_HZ)
                        .contains(hz) =>
                {
                    Err(InputSourceError::InvalidTone)
                }
                _ => Ok(()),
            },
            Self::Media { asset_uri } => {
                validate_media_asset_uri(asset_uri)?;
                if is_media_file_name(asset_uri) {
                    Ok(())
                } else {
                    Err(InputSourceError::UnsupportedMediaType)
                }
            }
            Self::Device { stable_key } => validate_locator(stable_key),
            Self::Network { endpoint } => validate_locator(endpoint),
        }
    }

    /// The source of `kind`, or `None` for an input kind without a
    /// replaceable source.
    #[must_use]
    pub fn from_kind(kind: &InputKind) -> Option<Self> {
        match kind {
            InputKind::Simulated(simulated) => Some(Self::Generator {
                video: match simulated.video {
                    SimulatedVideo::Solid(color) => GeneratorVideo::Solid {
                        rgba: [color.red, color.green, color.blue, color.alpha],
                    },
                    SimulatedVideo::Bars => GeneratorVideo::Bars,
                },
                tone_hz: match simulated.audio {
                    SimulatedAudio::Silence => None,
                    SimulatedAudio::Sine { frequency_hz } => Some(frequency_hz),
                },
            }),
            InputKind::Media { asset_uri, .. } => Some(Self::Media {
                asset_uri: asset_uri.clone(),
            }),
            InputKind::Device { stable_key } => Some(Self::Device {
                stable_key: stable_key.clone(),
            }),
            InputKind::Network { endpoint } => Some(Self::Network {
                endpoint: endpoint.clone(),
            }),
            InputKind::Color
            | InputKind::Scene { .. }
            | InputKind::Alias { .. }
            | InputKind::MixFeed(_)
            | InputKind::Title(_) => None,
        }
    }

    /// The input kind of this source. A media source plays with `playback`.
    #[must_use]
    pub fn to_kind(&self, playback: MediaPlaybackSettings) -> InputKind {
        match self {
            Self::Generator { video, tone_hz } => InputKind::Simulated(SimulatedInput::new(
                match *video {
                    GeneratorVideo::Solid {
                        rgba: [red, green, blue, alpha],
                    } => SimulatedVideo::Solid(crate::SolidColor::new(red, green, blue, alpha)),
                    GeneratorVideo::Bars => SimulatedVideo::Bars,
                },
                tone_hz.map_or(SimulatedAudio::Silence, |frequency_hz| {
                    SimulatedAudio::Sine { frequency_hz }
                }),
            )),
            Self::Media { asset_uri } => InputKind::Media {
                asset_uri: asset_uri.clone(),
                playback,
            },
            Self::Device { stable_key } => InputKind::Device {
                stable_key: stable_key.clone(),
            },
            Self::Network { endpoint } => InputKind::Network {
                endpoint: endpoint.clone(),
            },
        }
    }

    /// Whether this is a media-file source.
    #[must_use]
    pub const fn is_media(&self) -> bool {
        matches!(self, Self::Media { .. })
    }
}

/// One requested item of a batch media import.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct BatchImportRequest {
    pub input: InputId,
    /// The input name, or `None` to derive it from the file name.
    pub name: Option<String>,
    /// The media source: an `asset://` URI. Anything else is a host path
    /// the host could not bring into the project, and is refused as
    /// [`BatchImportRefusal::Unavailable`].
    pub asset_uri: String,
}

/// One accepted item of a batch import, ready to add.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct BatchImportItem {
    pub input: InputId,
    pub name: String,
    pub asset_uri: String,
}

/// Why one item of a batch import was refused.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BatchImportRefusal {
    /// The source is not a strict `asset://` URI.
    InvalidUri,
    /// The file type is not a supported media type.
    UnsupportedType,
    /// The host could not bring the file into the project.
    Unavailable,
    /// An earlier item of the batch names the same source.
    DuplicateSource,
    /// The input id is already used by the show or an earlier item.
    DuplicateInput,
    /// The name is blank or too long.
    InvalidName,
    /// The name is already used by an input or an earlier item.
    DuplicateName,
    /// The show would hold more inputs than its limit.
    TooManyInputs,
}

impl BatchImportRefusal {
    /// Every refusal, in token order.
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

    /// The stable token of the refusal on the wire.
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

    /// The refusal named by `token`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|refusal| refusal.token() == token)
    }
}

impl fmt::Display for BatchImportRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUri => "not a project asset:// URI",
            Self::UnsupportedType => "not a supported media type",
            Self::Unavailable => "the file could not be brought into the project",
            Self::DuplicateSource => "the same file is listed earlier in the batch",
            Self::DuplicateInput => "the input id is already in use",
            Self::InvalidName => "the input name is blank or too long",
            Self::DuplicateName => "the input name is already in use",
            Self::TooManyInputs => "the show cannot hold more inputs",
        })
    }
}

/// The per-item outcome of a batch import plan.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BatchImportPlan {
    /// Accepted items in request order, with their final names.
    pub accepted: Vec<BatchImportItem>,
    /// Refused items: their request index and why.
    pub refused: Vec<(usize, BatchImportRefusal)>,
}

/// Plans a batch media import against the existing inputs: each item is
/// validated in order (an unimported host path, URI syntax, media type, duplicate
/// source, id, name, capacity), a missing name is derived from the file
/// name and made unique with a numeric suffix, and accepted items reserve
/// their id and name for the items after them.
#[must_use]
pub fn plan_batch_import(
    existing: &[(InputId, &str)],
    capacity: usize,
    requests: &[BatchImportRequest],
) -> BatchImportPlan {
    let mut ids: BTreeSet<InputId> = existing.iter().map(|(input, _)| *input).collect();
    let mut names: BTreeSet<String> = existing
        .iter()
        .map(|(_, name)| (*name).to_owned())
        .collect();
    let mut sources = BTreeSet::new();
    let mut plan = BatchImportPlan::default();
    let mut count = existing.len();
    for (index, request) in requests.iter().enumerate() {
        let refusal = if !request.asset_uri.starts_with(MEDIA_ASSET_URI_PREFIX) {
            Some(BatchImportRefusal::Unavailable)
        } else if validate_media_asset_uri(&request.asset_uri).is_err() {
            Some(BatchImportRefusal::InvalidUri)
        } else if !is_media_file_name(&request.asset_uri) {
            Some(BatchImportRefusal::UnsupportedType)
        } else if sources.contains(&request.asset_uri) {
            Some(BatchImportRefusal::DuplicateSource)
        } else if ids.contains(&request.input) {
            Some(BatchImportRefusal::DuplicateInput)
        } else if count >= capacity {
            Some(BatchImportRefusal::TooManyInputs)
        } else {
            None
        };
        if let Some(refusal) = refusal {
            plan.refused.push((index, refusal));
            continue;
        }
        let name = match &request.name {
            Some(name) if fm_types::validate_input_name(name).is_err() => {
                plan.refused.push((index, BatchImportRefusal::InvalidName));
                continue;
            }
            Some(name) if names.contains(name) => {
                plan.refused
                    .push((index, BatchImportRefusal::DuplicateName));
                continue;
            }
            Some(name) => name.clone(),
            None => {
                let Some(name) = unique_input_name(&media_input_name(&request.asset_uri), |name| {
                    names.contains(name)
                }) else {
                    plan.refused
                        .push((index, BatchImportRefusal::DuplicateName));
                    continue;
                };
                name
            }
        };
        ids.insert(request.input);
        names.insert(name.clone());
        sources.insert(request.asset_uri.clone());
        count += 1;
        plan.accepted.push(BatchImportItem {
            input: request.input,
            name,
            asset_uri: request.asset_uri.clone(),
        });
    }
    plan
}
