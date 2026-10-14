//! Stored title templates (`assets/titles/<key>.fmtitle`) and their fonts.
//!
//! A title input in `project.json` names a template asset by URI and carries
//! only the operator's field values. The template document holds the
//! [`fm_titles::TitleTemplate`] layout plus a font table mapping every font
//! family the template draws with to a font file in the same bundle:
//!
//! ```json
//! {"schema_version": 29,
//!  "template": {"id": 1, "name": "Lower third", "width": 1920, "height": 1080,
//!               "background": "#00000000", "fields": [...], "elements": [...]},
//!  "fonts": [{"family": "Inter Bold", "asset_uri": "asset://fonts/Inter-Bold.ttf"}],
//!  "clocks": [{"name": "Local", "kind": "time_of_day",
//!              "zone": "EST5EDT,M3.2.0,M11.1.0",
//!              "format": {"type": "time_of_day", "preset": "hours_24"}}]}
//! ```
//!
//! The codec is as strict as the manifest codec: unknown or missing keys,
//! unknown enum values, and any `schema_version` other than the current one
//! are refused, and documents are capped at [`MAX_TITLE_TEMPLATE_BYTES`].
//!
//! A production clock element (`{"type": "production_clock", "clock": name,
//! "format": preset or null}`) names a clock in the document's `clocks`
//! table, which, like the font table, holds exactly the clocks the elements
//! show, sorted by name. Each clock is a [`TitleClock`]: `time_of_day` in a
//! canonical POSIX `TZ` zone (DST rules included), `countdown_to` a whole
//! second UTC `target_utc` (`YYYY-MM-DDTHH:MM:SSZ`) with an `overrun` policy,
//! or `since_take`. Display presets are `{"type": "duration" | "time_of_day",
//! "preset": name}`; the frames preset adds a `frame_rate`. A host reads the
//! clocks through [`fm_titles::title_clock_registry`].
//!
//! [`ProjectStore::import_title_template`] ingests a source document in the
//! same format whose `fonts` array is empty: fonts are supplied separately as
//! `(family, path)` pairs, parsed with [`FontFace::from_bytes`], and copied
//! into `assets/fonts/` with the same dedupe rules as media import.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    ffi::OsStr,
    fmt::{self, Write as _},
    fs::File,
    io::{self, Read, Write as _},
    path::{Path, PathBuf},
};

use fm_clock::production::{
    ClockName, DisplayFormat, DurationFormat, OverrunPolicy, TimeOfDayFormat, TimeZone, UtcInstant,
};
use fm_model::{
    InputKind, MAX_TITLE_FIELDS, Project, Rgba8, TitleFieldEntry, TitleFieldId, TitleFieldValue,
    TitleFieldsError, validate_title_fields,
};
use fm_titles::{
    Alignment, AnimatedProperty, AnimationTrack, Bounds, ClockDirection, ClockFormat, ClockSpec,
    Color, Element, ElementId, ElementKind, FieldDefinition, FieldId, FieldType, FieldValue,
    FontError, FontFace, FontStyle, HorizontalAlignment, ImageFilter, ImageFit, ImageSource,
    Interpolation, Keyframe, MAX_FONT_BYTES, ProductionClockSpec, Style, TemplateId,
    TickerDirection, TickerSpec, TitleClock, TitleClockError, TitleClockKind, TitleTemplate,
    VerticalAlignment,
};
use fm_types::{FrameRate, InputId};

use crate::{
    AssetAuditIssue, AssetAuditReason, AssetDisposition, AssetResolveError, BundleError,
    CURRENT_SCHEMA_VERSION, ProjectStore, StoredProject,
    asset::validate_asset_uri,
    fsutil,
    json::{
        self, DecodeError, Object, Value, escape_string, parse_array, parse_hex_color,
        parse_optional, syntax, unknown_enum, write_hex_color,
    },
    media::{asset_uri, imported_key, place_file},
};

/// Largest accepted stored or source title template document, in bytes.
pub const MAX_TITLE_TEMPLATE_BYTES: u64 = 1024 * 1024;
/// File extension of a stored title template.
pub const TITLE_TEMPLATE_EXTENSION: &str = "fmtitle";
const TITLES_DIRECTORY: &str = "titles";
const FONTS_DIRECTORY: &str = "fonts";

/// One font family a template draws with and the bundle font file behind it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleFontAsset {
    pub family: String,
    pub asset_uri: String,
}

/// A decoded `.fmtitle` document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredTitleTemplate {
    pub template: TitleTemplate,
    /// Font table sorted by family.
    pub fonts: Vec<TitleFontAsset>,
    /// The clocks production clock elements show, sorted by name.
    pub clocks: Vec<TitleClock>,
}

impl StoredTitleTemplate {
    /// Checks everything the codec enforces: template structure, field
    /// defaults within project title bounds, valid image default URIs, a
    /// font table that covers exactly the families the template uses, and a
    /// clock table that covers exactly the clocks it shows, in a form the
    /// document can hold.
    ///
    /// # Errors
    ///
    /// Returns the first [`TitleTemplateError`] found.
    pub fn validate(&self) -> Result<(), TitleTemplateError> {
        validate_template_body(&self.template)?;
        check_font_table(
            &self.template,
            self.fonts.iter().map(|font| font.family.as_str()),
        )?;
        for font in &self.fonts {
            if validate_asset_uri(&font.asset_uri).is_err() {
                return Err(TitleTemplateError::InvalidFontUri {
                    family: font.family.clone(),
                });
            }
        }
        check_clock_table(&self.template, &self.clocks)
    }

    /// Every bundle asset URI the template references: fonts and image
    /// field defaults.
    #[must_use]
    pub fn asset_uris(&self) -> BTreeSet<String> {
        let mut uris = self
            .fonts
            .iter()
            .map(|font| font.asset_uri.clone())
            .collect::<BTreeSet<_>>();
        for field in &self.template.fields {
            if let FieldValue::Image(image) = &field.default {
                uris.insert(image.asset.clone());
            }
        }
        uris
    }
}

/// A template document that cannot be read, decoded, or stored.
#[derive(Debug)]
pub enum TitleTemplateError {
    Io(io::Error),
    /// The template URI does not resolve to a bundle file.
    Asset(AssetResolveError),
    TooLarge {
        bytes: u64,
        maximum: u64,
    },
    NotUtf8,
    Malformed {
        offset: usize,
        message: String,
    },
    UnsupportedSchema {
        found: u32,
        supported: u32,
    },
    /// The clock table does not match the production clock elements.
    InvalidClocks(TitleClockError),
    /// A clock the document cannot hold exactly: a zone whose canonical
    /// POSIX form does not parse back to it, or a countdown target with a
    /// sub-second part.
    UnencodableClock {
        clock: ClockName,
        reason: &'static str,
    },
    /// The template fails structural validation.
    InvalidTemplate(Vec<fm_titles::ValidationError>),
    /// The template has more fields than a title input may carry.
    TooManyFields {
        count: usize,
    },
    /// The field defaults break a project title bound.
    InvalidFieldDefaults(TitleFieldsError),
    /// An image field default is not a valid `asset://` URI.
    InvalidImageUri {
        field: FieldId,
    },
    BlankFontFamily,
    DuplicateFontFamily(String),
    /// A family the template draws with has no font.
    MissingFontFamily(String),
    /// A font is declared that no element uses.
    UnusedFontFamily(String),
    /// A font table URI is not a valid `asset://` URI.
    InvalidFontUri {
        family: String,
    },
}

impl fmt::Display for TitleTemplateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "title template I/O failed: {error}"),
            Self::Asset(error) => write!(formatter, "title template asset: {error}"),
            Self::TooLarge { bytes, maximum } => write!(
                formatter,
                "title template is {bytes} bytes, maximum is {maximum}"
            ),
            Self::NotUtf8 => formatter.write_str("title template is not UTF-8"),
            Self::Malformed { offset, message } => {
                write!(formatter, "malformed title template at {offset}: {message}")
            }
            Self::UnsupportedSchema { found, supported } => write!(
                formatter,
                "title template schema {found} is unsupported; expected {supported}"
            ),
            Self::InvalidClocks(error) => write!(formatter, "title template clocks: {error}"),
            Self::UnencodableClock { clock, reason } => {
                write!(formatter, "clock {clock} cannot be stored: {reason}")
            }
            Self::InvalidTemplate(errors) => write!(
                formatter,
                "title template has {} validation error(s): {errors:?}",
                errors.len()
            ),
            Self::TooManyFields { count } => write!(
                formatter,
                "title template has {count} fields, maximum is {MAX_TITLE_FIELDS}"
            ),
            Self::InvalidFieldDefaults(error) => {
                write!(formatter, "title template defaults: {error}")
            }
            Self::InvalidImageUri { field } => write!(
                formatter,
                "image field {field} default is not a valid asset:// URI"
            ),
            Self::BlankFontFamily => formatter.write_str("font family must not be blank"),
            Self::DuplicateFontFamily(family) => {
                write!(formatter, "font family `{family}` is declared twice")
            }
            Self::MissingFontFamily(family) => {
                write!(formatter, "font family `{family}` is used but not supplied")
            }
            Self::UnusedFontFamily(family) => {
                write!(
                    formatter,
                    "font family `{family}` is supplied but never used"
                )
            }
            Self::InvalidFontUri { family } => write!(
                formatter,
                "font `{family}` does not name a valid asset:// URI"
            ),
        }
    }
}

impl Error for TitleTemplateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Asset(error) => Some(error),
            Self::InvalidFieldDefaults(error) => Some(error),
            Self::InvalidClocks(error) => Some(error),
            _ => None,
        }
    }
}

impl From<DecodeError> for TitleTemplateError {
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::Syntax { offset, message } => Self::Malformed { offset, message },
            DecodeError::Validation(error) => Self::Malformed {
                offset: 0,
                message: error.to_string(),
            },
        }
    }
}

/// A title template import that was refused.
#[derive(Debug)]
pub enum TitleImportError {
    /// The source document or the resulting font table is invalid.
    Template(TitleTemplateError),
    /// The source document declares fonts; fonts are supplied separately.
    SourceDeclaresFonts,
    /// A supplied font file could not be read.
    FontUnreadable {
        family: String,
        error: io::Error,
    },
    /// A supplied font file is not a usable font.
    UnparsableFont {
        family: String,
        error: FontError,
    },
    /// An image field default does not resolve inside the bundle.
    UnresolvedImage {
        field: FieldId,
        asset_uri: String,
    },
    Bundle(BundleError),
}

impl fmt::Display for TitleImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Template(error) => error.fmt(formatter),
            Self::SourceDeclaresFonts => formatter.write_str(
                "the source template must declare `\"fonts\": []`; supply fonts with --font",
            ),
            Self::FontUnreadable { family, error } => {
                write!(formatter, "font `{family}` could not be read: {error}")
            }
            Self::UnparsableFont { family, error } => {
                write!(formatter, "font `{family}` is unusable: {error}")
            }
            Self::UnresolvedImage { field, asset_uri } => write!(
                formatter,
                "image field {field} default `{asset_uri}` does not resolve in the bundle"
            ),
            Self::Bundle(error) => error.fmt(formatter),
        }
    }
}

impl Error for TitleImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Template(error) => Some(error),
            Self::FontUnreadable { error, .. } => Some(error),
            Self::UnparsableFont { error, .. } => Some(error),
            Self::Bundle(error) => Some(error),
            Self::SourceDeclaresFonts | Self::UnresolvedImage { .. } => None,
        }
    }
}

impl From<TitleTemplateError> for TitleImportError {
    fn from(error: TitleTemplateError) -> Self {
        Self::Template(error)
    }
}

impl From<BundleError> for TitleImportError {
    fn from(error: BundleError) -> Self {
        Self::Bundle(error)
    }
}

impl From<io::Error> for TitleImportError {
    fn from(error: io::Error) -> Self {
        Self::Bundle(BundleError::Io(error))
    }
}

/// One font placed in the bundle by an import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedTitleFont {
    pub family: String,
    pub asset_uri: String,
    pub disposition: AssetDisposition,
}

/// The outcome of [`ProjectStore::import_title_template`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedTitleTemplate {
    pub template_uri: String,
    pub disposition: AssetDisposition,
    /// Fonts sorted by family.
    pub fonts: Vec<ImportedTitleFont>,
    pub stored: StoredTitleTemplate,
}

/// How a project title input disagrees with its stored template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleProblem {
    /// The template does not load; the input cannot be checked further.
    UnreadableTemplate(String),
    /// A template field has no project value.
    MissingField(TitleFieldId),
    /// A project value names no template field.
    UnknownField(TitleFieldId),
    TypeMismatch {
        field: TitleFieldId,
        expected: FieldType,
        actual: FieldType,
    },
}

/// One title input problem found by [`ProjectStore::validate_titles`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleIssue {
    pub input_id: InputId,
    pub problem: TitleProblem,
}

impl ProjectStore {
    /// Reads and strictly decodes the stored template at `uri`.
    ///
    /// # Errors
    ///
    /// Returns [`TitleTemplateError::Asset`] when the URI does not resolve to
    /// a bundle file, and every read and decode error.
    pub fn load_title_template(
        &self,
        uri: &str,
    ) -> Result<StoredTitleTemplate, TitleTemplateError> {
        let path = self
            .resolve_asset_uri(uri)
            .map_err(TitleTemplateError::Asset)?;
        decode_title_template(&read_template_file(&path)?)
    }

    /// Imports a source template document and its fonts into this bundle.
    ///
    /// `template` is a `.fmtitle` document in the stored format whose `fonts`
    /// array is empty. `fonts` pairs every font family the template draws
    /// with to a font file; each file must parse with [`FontFace::from_bytes`]
    /// and is copied to `assets/fonts/` (an identical file already at the key
    /// is reused, a different one gets a `-2`, `-3`, ... key). Image field
    /// defaults must already resolve inside the bundle. The stored template is
    /// written to `assets/titles/<source stem>.fmtitle` with the same dedupe
    /// rules. `project.json` is not touched.
    ///
    /// # Errors
    ///
    /// Returns [`TitleImportError`]; validation happens before anything is
    /// copied. Fonts copied before a later I/O failure stay in place.
    pub fn import_title_template(
        &self,
        template: &Path,
        fonts: &[(String, PathBuf)],
    ) -> Result<ImportedTitleTemplate, TitleImportError> {
        let source = read_template_file(template)?;
        let (parsed, declared_fonts, clocks) = decode_document(&source)?;
        if !declared_fonts.is_empty() {
            return Err(TitleImportError::SourceDeclaresFonts);
        }
        validate_template_body(&parsed)?;
        check_font_table(&parsed, fonts.iter().map(|(family, _)| family.as_str()))?;
        check_clock_table(&parsed, &clocks)?;
        // The checked bytes are what gets stored, so a source file changing
        // after it was checked cannot place an unparsable or oversized font.
        let mut font_bytes = Vec::with_capacity(fonts.len());
        for (family, path) in fonts {
            let bytes = read_font_file(path).map_err(|error| TitleImportError::FontUnreadable {
                family: family.clone(),
                error,
            })?;
            FontFace::from_bytes(bytes.clone()).map_err(|error| {
                TitleImportError::UnparsableFont {
                    family: family.clone(),
                    error,
                }
            })?;
            font_bytes.push(bytes);
        }
        for field in &parsed.fields {
            if let FieldValue::Image(image) = &field.default
                && self.resolve_asset_uri(&image.asset).is_err()
            {
                return Err(TitleImportError::UnresolvedImage {
                    field: field.id,
                    asset_uri: image.asset.clone(),
                });
            }
        }

        let assets_root = self.assets_root();
        let mut imported_fonts = Vec::with_capacity(fonts.len());
        for ((family, path), bytes) in fonts.iter().zip(&font_bytes) {
            let base = format!(
                "{FONTS_DIRECTORY}/{}",
                imported_key(path.file_name().unwrap_or_else(|| OsStr::new("font")))
            );
            let (key, disposition) = place_bytes(&assets_root, &base, bytes)?;
            imported_fonts.push(ImportedTitleFont {
                family: family.clone(),
                asset_uri: asset_uri(&key),
                disposition,
            });
        }
        imported_fonts.sort_by(|left, right| left.family.cmp(&right.family));
        let stored = StoredTitleTemplate {
            template: parsed,
            fonts: imported_fonts
                .iter()
                .map(|font| TitleFontAsset {
                    family: font.family.clone(),
                    asset_uri: font.asset_uri.clone(),
                })
                .collect(),
            clocks,
        };
        let document = encode_title_template(&stored)?;

        let stem = template
            .file_stem()
            .map_or_else(|| "title".to_owned(), imported_key);
        let base = format!("{TITLES_DIRECTORY}/{stem}.{TITLE_TEMPLATE_EXTENSION}");
        let (key, disposition) = place_bytes(&assets_root, &base, document.as_bytes())?;
        Ok(ImportedTitleTemplate {
            template_uri: asset_uri(&key),
            disposition,
            fonts: imported_fonts,
            stored,
        })
    }

    /// Checks that every title input's field ids and types exactly cover its
    /// stored template's fields. Issues are sorted by input id.
    #[must_use]
    pub fn validate_titles(&self, project: &StoredProject) -> Vec<TitleIssue> {
        let mut issues = Vec::new();
        for input in project.project().inputs() {
            let InputKind::Title(title) = &input.kind else {
                continue;
            };
            let template = match self.load_title_template(&title.template_uri) {
                Ok(template) => template,
                Err(error) => {
                    issues.push(TitleIssue {
                        input_id: input.id,
                        problem: TitleProblem::UnreadableTemplate(error.to_string()),
                    });
                    continue;
                }
            };
            let expected = template
                .template
                .fields
                .iter()
                .map(|field| (field.id.get(), field.field_type()))
                .collect::<BTreeMap<_, _>>();
            let actual = title
                .fields
                .iter()
                .map(|entry| (entry.id.get(), field_type(&entry.value)))
                .collect::<BTreeMap<_, _>>();
            for (id, expected_type) in &expected {
                match actual.get(id) {
                    None => issues.push(TitleIssue {
                        input_id: input.id,
                        problem: TitleProblem::MissingField(TitleFieldId::new(*id)),
                    }),
                    Some(actual_type) if actual_type != expected_type => {
                        issues.push(TitleIssue {
                            input_id: input.id,
                            problem: TitleProblem::TypeMismatch {
                                field: TitleFieldId::new(*id),
                                expected: *expected_type,
                                actual: *actual_type,
                            },
                        });
                    }
                    Some(_) => {}
                }
            }
            for id in actual.keys().filter(|id| !expected.contains_key(id)) {
                issues.push(TitleIssue {
                    input_id: input.id,
                    problem: TitleProblem::UnknownField(TitleFieldId::new(*id)),
                });
            }
        }
        issues.sort_by_key(|issue| issue.input_id);
        issues
    }

    /// Audits the assets of every title input: the template itself, then (when
    /// it loads) its fonts and image defaults, and the input's image values.
    pub(crate) fn audit_title_assets(&self, project: &Project) -> Vec<(InputId, AssetAuditReason)> {
        let mut issues = Vec::new();
        for input in project.inputs() {
            let InputKind::Title(title) = &input.kind else {
                continue;
            };
            let mut uris = title_value_uris(&title.fields);
            match self.load_title_template(&title.template_uri) {
                Ok(template) => uris.extend(template.asset_uris()),
                Err(TitleTemplateError::Asset(error)) => {
                    issues.push((input.id, audit_reason(&error)));
                }
                Err(_) => issues.push((input.id, AssetAuditReason::UnreadableTitleTemplate)),
            }
            for uri in uris {
                if let Err(error) = self.resolve_asset_uri(&uri) {
                    issues.push((input.id, audit_reason(&error)));
                }
            }
        }
        issues
    }

    /// Every asset URI title inputs reference, including the template's fonts
    /// and image defaults.
    ///
    /// # Errors
    ///
    /// Returns the first template that does not load.
    pub(crate) fn title_asset_uris(
        &self,
        project: &Project,
    ) -> Result<BTreeSet<String>, (InputId, TitleTemplateError)> {
        let mut uris = BTreeSet::new();
        for input in project.inputs() {
            let InputKind::Title(title) = &input.kind else {
                continue;
            };
            let template = self
                .load_title_template(&title.template_uri)
                .map_err(|error| (input.id, error))?;
            uris.insert(title.template_uri.clone());
            uris.extend(template.asset_uris());
            uris.extend(title_value_uris(&title.fields));
        }
        Ok(uris)
    }
}

fn title_value_uris(fields: &[TitleFieldEntry]) -> BTreeSet<String> {
    fields
        .iter()
        .filter_map(|entry| match &entry.value {
            TitleFieldValue::Image { asset_uri } => Some(asset_uri.clone()),
            _ => None,
        })
        .collect()
}

pub(crate) const fn audit_reason(error: &AssetResolveError) -> AssetAuditReason {
    match error {
        AssetResolveError::InvalidUri => AssetAuditReason::InvalidUri,
        AssetResolveError::AssetsRootUnavailable(_) | AssetResolveError::AssetUnavailable(_) => {
            AssetAuditReason::MissingAsset
        }
        AssetResolveError::OutsideAssetsRoot => AssetAuditReason::OutsideAssetsRoot,
        AssetResolveError::NotRegularFile => AssetAuditReason::NotRegularFile,
    }
}

/// The project field type of a title value.
#[must_use]
pub const fn field_type(value: &TitleFieldValue) -> FieldType {
    match value {
        TitleFieldValue::Text(_) => FieldType::Text,
        TitleFieldValue::Number(_) => FieldType::Number,
        TitleFieldValue::Color(_) => FieldType::Color,
        TitleFieldValue::Image { .. } => FieldType::Image,
    }
}

/// Converts a template field value into a project title value.
#[must_use]
pub fn title_field_value(value: &FieldValue) -> TitleFieldValue {
    match value {
        FieldValue::Text(text) => TitleFieldValue::Text(text.clone()),
        FieldValue::Number(number) => TitleFieldValue::Number(*number),
        FieldValue::Color(color) => {
            TitleFieldValue::Color(Rgba8::new(color.r, color.g, color.b, color.a))
        }
        FieldValue::Image(image) => TitleFieldValue::Image {
            asset_uri: image.asset.clone(),
        },
    }
}

/// The template's field defaults as project title fields, sorted by id.
#[must_use]
pub fn title_field_defaults(template: &TitleTemplate) -> Vec<TitleFieldEntry> {
    let mut fields = template
        .fields
        .iter()
        .map(|field| TitleFieldEntry {
            id: TitleFieldId::new(field.id.get()),
            value: title_field_value(&field.default),
        })
        .collect::<Vec<_>>();
    fields.sort_by_key(|entry| entry.id);
    fields
}

fn validate_template_body(template: &TitleTemplate) -> Result<(), TitleTemplateError> {
    let report = fm_titles::validate_template(template, &fm_titles::AssetCatalog::new());
    if !report.errors.is_empty() {
        return Err(TitleTemplateError::InvalidTemplate(report.errors));
    }
    if template.fields.len() > MAX_TITLE_FIELDS {
        return Err(TitleTemplateError::TooManyFields {
            count: template.fields.len(),
        });
    }
    for field in &template.fields {
        if let FieldValue::Image(image) = &field.default
            && validate_asset_uri(&image.asset).is_err()
        {
            return Err(TitleTemplateError::InvalidImageUri { field: field.id });
        }
    }
    validate_title_fields(&title_field_defaults(template))
        .map_err(TitleTemplateError::InvalidFieldDefaults)
}

/// Requires `families` to be non-blank, distinct, and exactly the families
/// the template's elements draw with.
fn check_font_table<'a>(
    template: &TitleTemplate,
    families: impl Iterator<Item = &'a str>,
) -> Result<(), TitleTemplateError> {
    let mut supplied = BTreeSet::new();
    for family in families {
        if family.trim().is_empty() {
            return Err(TitleTemplateError::BlankFontFamily);
        }
        if !supplied.insert(family) {
            return Err(TitleTemplateError::DuplicateFontFamily(family.to_owned()));
        }
    }
    let used = template
        .elements
        .iter()
        .filter_map(|element| element.style.font.as_ref())
        .map(|font| font.family.as_str())
        .collect::<BTreeSet<_>>();
    if let Some(missing) = used.difference(&supplied).next() {
        return Err(TitleTemplateError::MissingFontFamily((*missing).to_owned()));
    }
    if let Some(unused) = supplied.difference(&used).next() {
        return Err(TitleTemplateError::UnusedFontFamily((*unused).to_owned()));
    }
    Ok(())
}

/// Requires `clocks` to be exactly the clocks `template` shows (see
/// [`fm_titles::validate_title_clocks`]) and each to survive the document
/// form unchanged.
fn check_clock_table(
    template: &TitleTemplate,
    clocks: &[TitleClock],
) -> Result<(), TitleTemplateError> {
    fm_titles::validate_title_clocks(template, clocks)
        .map_err(TitleTemplateError::InvalidClocks)?;
    for clock in clocks {
        let unencodable = |reason| TitleTemplateError::UnencodableClock {
            clock: clock.name.clone(),
            reason,
        };
        match &clock.kind {
            TitleClockKind::TimeOfDay { zone, .. } => {
                if TimeZone::parse_posix(&zone.to_string()).as_ref() != Ok(zone) {
                    return Err(unencodable(
                        "the zone has no POSIX TZ form that parses back to it",
                    ));
                }
            }
            TitleClockKind::CountdownTo { target, .. } => {
                if target.subsec_nanos() != 0 {
                    return Err(unencodable("countdown targets are whole UTC seconds"));
                }
            }
            TitleClockKind::SinceTake { .. } => {}
        }
    }
    Ok(())
}

fn read_capped(path: &Path, maximum: u64) -> io::Result<Result<Vec<u8>, u64>> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    if metadata.len() > maximum {
        return Ok(Err(metadata.len()));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or_default());
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if length > maximum {
        return Ok(Err(length));
    }
    Ok(Ok(bytes))
}

fn read_template_file(path: &Path) -> Result<String, TitleTemplateError> {
    let bytes = read_capped(path, MAX_TITLE_TEMPLATE_BYTES)
        .map_err(TitleTemplateError::Io)?
        .map_err(|bytes| TitleTemplateError::TooLarge {
            bytes,
            maximum: MAX_TITLE_TEMPLATE_BYTES,
        })?;
    String::from_utf8(bytes).map_err(|_| TitleTemplateError::NotUtf8)
}

fn read_font_file(path: &Path) -> io::Result<Vec<u8>> {
    let maximum = u64::try_from(MAX_FONT_BYTES).unwrap_or(u64::MAX);
    read_capped(path, maximum)?.map_err(|bytes| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("font file is {bytes} bytes, maximum is {maximum}"),
        )
    })
}

/// Writes `bytes` to the first free key in `base`'s suffix family, reusing an
/// identical occupant. Never overwrites an existing asset.
fn place_bytes(
    assets_root: &Path,
    base: &str,
    bytes: &[u8],
) -> Result<(String, AssetDisposition), BundleError> {
    let staged = fsutil::create_parents_within(assets_root, base)?;
    let (temp, mut file, _guard) = fsutil::create_temp_sibling(&staged, "import")?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    place_file(assets_root, &temp, base, |_| false)
}

/// Strictly decodes a stored `.fmtitle` document.
///
/// # Errors
///
/// Returns a [`TitleTemplateError`] for oversized, malformed, wrong-schema,
/// or invalid documents.
pub fn decode_title_template(source: &str) -> Result<StoredTitleTemplate, TitleTemplateError> {
    let (template, fonts, clocks) = decode_document(source)?;
    let stored = StoredTitleTemplate {
        template,
        fonts,
        clocks,
    };
    stored.validate()?;
    if stored
        .fonts
        .windows(2)
        .any(|pair| pair[0].family >= pair[1].family)
    {
        return Err(TitleTemplateError::Malformed {
            offset: 0,
            message: "fonts must be sorted by family".to_owned(),
        });
    }
    if stored
        .clocks
        .windows(2)
        .any(|pair| pair[0].name >= pair[1].name)
    {
        return Err(TitleTemplateError::Malformed {
            offset: 0,
            message: "clocks must be sorted by name".to_owned(),
        });
    }
    Ok(stored)
}

/// Encodes a stored `.fmtitle` document. Fonts are written sorted by family
/// and clocks sorted by name.
///
/// # Errors
///
/// Returns a [`TitleTemplateError`] when the template would not decode, for
/// example because its font or clock table is wrong, or the encoded document
/// exceeds [`MAX_TITLE_TEMPLATE_BYTES`].
pub fn encode_title_template(stored: &StoredTitleTemplate) -> Result<String, TitleTemplateError> {
    stored.validate()?;
    let mut fonts = stored.fonts.iter().collect::<Vec<_>>();
    fonts.sort_by(|left, right| left.family.cmp(&right.family));
    let template = &stored.template;
    let mut output = String::new();
    write!(
        output,
        "{{\n  \"schema_version\": {CURRENT_SCHEMA_VERSION},\n  \"template\": {{\n    \"id\": {},\n    \"name\": ",
        template.id
    )
    .expect("writing to a string cannot fail");
    write_string(&mut output, &template.name);
    write!(
        output,
        ",\n    \"width\": {},\n    \"height\": {},\n    \"background\": ",
        template.width, template.height
    )
    .expect("writing to a string cannot fail");
    write_color(&mut output, template.background);
    output.push_str(",\n    \"fields\": [");
    for (index, field) in template.fields.iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(output, "{{\"id\": {}, \"name\": ", field.id)
            .expect("writing to a string cannot fail");
        write_string(&mut output, &field.name);
        output.push_str(", \"default\": ");
        write_field_value(&mut output, &field.default);
        output.push('}');
    }
    output.push_str(if template.fields.is_empty() {
        "],\n"
    } else {
        "\n    ],\n"
    });
    output.push_str("    \"elements\": [");
    for (index, element) in template.elements.iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write_element(&mut output, element);
    }
    output.push_str(if template.elements.is_empty() {
        "]\n  },\n"
    } else {
        "\n    ]\n  },\n"
    });
    output.push_str("  \"fonts\": [");
    for (index, font) in fonts.iter().enumerate() {
        output.push_str(if index == 0 { "\n    " } else { ",\n    " });
        output.push_str("{\"family\": ");
        write_string(&mut output, &font.family);
        output.push_str(", \"asset_uri\": ");
        write_string(&mut output, &font.asset_uri);
        output.push('}');
    }
    output.push_str(if fonts.is_empty() { "],\n" } else { "\n  ],\n" });
    let mut clocks = stored.clocks.iter().collect::<Vec<_>>();
    clocks.sort_by(|left, right| left.name.cmp(&right.name));
    output.push_str("  \"clocks\": [");
    for (index, clock) in clocks.iter().enumerate() {
        output.push_str(if index == 0 { "\n    " } else { ",\n    " });
        write_clock(&mut output, clock);
    }
    output.push_str(if clocks.is_empty() {
        "]\n}\n"
    } else {
        "\n  ]\n}\n"
    });
    // The canonical layout can be larger than a compact source document, and
    // a stored document over the cap would never load again.
    let bytes = u64::try_from(output.len()).unwrap_or(u64::MAX);
    if bytes > MAX_TITLE_TEMPLATE_BYTES {
        return Err(TitleTemplateError::TooLarge {
            bytes,
            maximum: MAX_TITLE_TEMPLATE_BYTES,
        });
    }
    Ok(output)
}

fn write_string(output: &mut String, value: &str) {
    output.push('"');
    escape_string(output, value);
    output.push('"');
}

fn write_color(output: &mut String, color: Color) {
    output.push('"');
    write_hex_color(output, Rgba8::new(color.r, color.g, color.b, color.a));
    output.push('"');
}

fn write_field_value(output: &mut String, value: &FieldValue) {
    match value {
        FieldValue::Text(text) => {
            output.push_str("{\"type\": \"text\", \"value\": ");
            write_string(output, text);
        }
        FieldValue::Number(number) => {
            write!(output, "{{\"type\": \"number\", \"value\": {number}")
                .expect("writing to a string cannot fail");
        }
        FieldValue::Color(color) => {
            output.push_str("{\"type\": \"color\", \"value\": ");
            write_color(output, *color);
        }
        FieldValue::Image(image) => {
            output.push_str("{\"type\": \"image\", \"asset_uri\": ");
            write_string(output, &image.asset);
        }
    }
    output.push('}');
}

fn write_element(output: &mut String, element: &Element) {
    write!(output, "{{\"id\": {}, \"name\": ", element.id)
        .expect("writing to a string cannot fail");
    write_string(output, &element.name);
    let bounds = element.bounds;
    write!(
        output,
        ", \"bounds\": {{\"x\": {}, \"y\": {}, \"width\": {}, \"height\": {}}}, \"z_index\": {}, \"visible\": {}, \"style\": {{\"fill\": ",
        bounds.x, bounds.y, bounds.width, bounds.height, element.z_index, element.visible
    )
    .expect("writing to a string cannot fail");
    let style = &element.style;
    write_color(output, style.fill);
    output.push_str(", \"background\": ");
    match style.background {
        Some(color) => write_color(output, color),
        None => output.push_str("null"),
    }
    write!(output, ", \"opacity\": {}, \"font\": ", style.opacity)
        .expect("writing to a string cannot fail");
    match &style.font {
        Some(font) => {
            output.push_str("{\"family\": ");
            write_string(output, &font.family);
            write!(output, ", \"size_px\": {}}}", font.size_px)
                .expect("writing to a string cannot fail");
        }
        None => output.push_str("null"),
    }
    write!(
        output,
        ", \"alignment\": {{\"horizontal\": \"{}\", \"vertical\": \"{}\"}}}}, \"color_field\": ",
        horizontal_name(style.alignment.horizontal),
        vertical_name(style.alignment.vertical)
    )
    .expect("writing to a string cannot fail");
    match element.color_field {
        Some(field) => write!(output, "{field}").expect("writing to a string cannot fail"),
        None => output.push_str("null"),
    }
    output.push_str(", \"kind\": ");
    write_element_kind(output, element);
    output.push_str(", \"animations\": [");
    for (index, track) in element.animations.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(
            output,
            "{{\"property\": \"{}\", \"keyframes\": [",
            property_name(track.property)
        )
        .expect("writing to a string cannot fail");
        for (index, keyframe) in track.keyframes.iter().enumerate() {
            if index != 0 {
                output.push_str(", ");
            }
            write!(
                output,
                "{{\"at_ms\": {}, \"value\": {}, \"interpolation\": \"{}\"}}",
                keyframe.at_ms,
                keyframe.value,
                match keyframe.interpolation {
                    Interpolation::Hold => "hold",
                    Interpolation::Linear => "linear",
                }
            )
            .expect("writing to a string cannot fail");
        }
        output.push_str("]}");
    }
    output.push_str("]}");
}

fn write_clock(output: &mut String, clock: &TitleClock) {
    output.push_str("{\"name\": ");
    write_string(output, clock.name.as_str());
    match &clock.kind {
        TitleClockKind::TimeOfDay { zone, .. } => {
            output.push_str(", \"kind\": \"time_of_day\", \"zone\": ");
            write_string(output, &zone.to_string());
        }
        TitleClockKind::CountdownTo {
            target, overrun, ..
        } => {
            let civil = target.to_civil(0);
            write!(
                output,
                ", \"kind\": \"countdown_to\", \"target_utc\": \"{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z\", \"overrun\": \"{}\"",
                civil.year,
                civil.month,
                civil.day,
                civil.hour,
                civil.minute,
                civil.second,
                match overrun {
                    OverrunPolicy::StopAtTarget => "stop_at_target",
                    OverrunPolicy::Continue => "continue",
                }
            )
            .expect("writing to a string cannot fail");
        }
        TitleClockKind::SinceTake { .. } => output.push_str(", \"kind\": \"since_take\""),
    }
    output.push_str(", \"format\": ");
    write_display_format(output, clock.kind.display_format());
    output.push('}');
}

fn write_display_format(output: &mut String, format: DisplayFormat) {
    match format {
        DisplayFormat::Duration(DurationFormat::HoursMinutesSecondsFrames(rate)) => write!(
            output,
            "{{\"type\": \"duration\", \"preset\": \"hours_minutes_seconds_frames\", \"frame_rate\": {{\"numerator\": {}, \"denominator\": {}}}}}",
            rate.numerator(),
            rate.denominator()
        )
        .expect("writing to a string cannot fail"),
        DisplayFormat::Duration(format) => write!(
            output,
            "{{\"type\": \"duration\", \"preset\": \"{}\"}}",
            duration_preset_name(format)
        )
        .expect("writing to a string cannot fail"),
        DisplayFormat::TimeOfDay(format) => write!(
            output,
            "{{\"type\": \"time_of_day\", \"preset\": \"{}\"}}",
            time_of_day_preset_name(format)
        )
        .expect("writing to a string cannot fail"),
    }
}

const fn duration_preset_name(format: DurationFormat) -> &'static str {
    match format {
        DurationFormat::HoursMinutesSeconds => "hours_minutes_seconds",
        DurationFormat::MinutesSeconds => "minutes_seconds",
        DurationFormat::Seconds => "seconds",
        DurationFormat::HoursMinutesSecondsTenths => "hours_minutes_seconds_tenths",
        DurationFormat::MinutesSecondsTenths => "minutes_seconds_tenths",
        DurationFormat::HoursMinutesSecondsMillis => "hours_minutes_seconds_millis",
        DurationFormat::HoursMinutesSecondsFrames(_) => "hours_minutes_seconds_frames",
        DurationFormat::Compact => "compact",
    }
}

const fn time_of_day_preset_name(format: TimeOfDayFormat) -> &'static str {
    match format {
        TimeOfDayFormat::Hours24 => "hours_24",
        TimeOfDayFormat::Hours24Minutes => "hours_24_minutes",
        TimeOfDayFormat::Hours12 => "hours_12",
        TimeOfDayFormat::Hours12Minutes => "hours_12_minutes",
        TimeOfDayFormat::Date => "date",
        TimeOfDayFormat::Rfc3339 => "rfc3339",
    }
}

fn write_element_kind(output: &mut String, element: &Element) {
    match &element.kind {
        ElementKind::Text { field } => {
            write!(output, "{{\"type\": \"text\", \"field\": {field}}}")
                .expect("writing to a string cannot fail");
        }
        ElementKind::Rectangle => output.push_str("{\"type\": \"rectangle\"}"),
        ElementKind::Image { field, fit, filter } => write!(
            output,
            "{{\"type\": \"image\", \"field\": {field}, \"fit\": \"{}\", \"filter\": \"{}\"}}",
            fit_name(*fit),
            filter_name(*filter)
        )
        .expect("writing to a string cannot fail"),
        ElementKind::Clock(spec) => write!(
            output,
            "{{\"type\": \"clock\", \"direction\": \"{}\", \"start_value_ms\": {}, \"starts_at_ms\": {}, \"format\": \"{}\"}}",
            match spec.direction {
                ClockDirection::CountUp => "count_up",
                ClockDirection::CountDown => "count_down",
            },
            spec.start_value_ms,
            spec.starts_at_ms,
            match spec.format {
                ClockFormat::MinutesSeconds => "minutes_seconds",
                ClockFormat::HoursMinutesSeconds => "hours_minutes_seconds",
            }
        )
        .expect("writing to a string cannot fail"),
        ElementKind::Ticker(spec) => write!(
            output,
            "{{\"type\": \"ticker\", \"field\": {}, \"direction\": \"{}\", \"pixels_per_second\": {}, \"gap_px\": {}, \"starts_at_ms\": {}}}",
            spec.field,
            match spec.direction {
                TickerDirection::Left => "left",
                TickerDirection::Right => "right",
            },
            spec.pixels_per_second,
            spec.gap_px,
            spec.starts_at_ms
        )
        .expect("writing to a string cannot fail"),
        ElementKind::ProductionClock(spec) => {
            output.push_str("{\"type\": \"production_clock\", \"clock\": ");
            write_string(output, spec.clock.as_str());
            output.push_str(", \"format\": ");
            match spec.format {
                Some(format) => write_display_format(output, format),
                None => output.push_str("null"),
            }
            output.push('}');
        }
    }
}

const fn horizontal_name(alignment: HorizontalAlignment) -> &'static str {
    match alignment {
        HorizontalAlignment::Start => "start",
        HorizontalAlignment::Center => "center",
        HorizontalAlignment::End => "end",
    }
}

const fn vertical_name(alignment: VerticalAlignment) -> &'static str {
    match alignment {
        VerticalAlignment::Start => "start",
        VerticalAlignment::Center => "center",
        VerticalAlignment::End => "end",
    }
}

const fn fit_name(fit: ImageFit) -> &'static str {
    match fit {
        ImageFit::Stretch => "stretch",
        ImageFit::Contain => "contain",
        ImageFit::Cover => "cover",
        ImageFit::None => "none",
    }
}

const fn filter_name(filter: ImageFilter) -> &'static str {
    match filter {
        ImageFilter::Nearest => "nearest",
        ImageFilter::Bilinear => "bilinear",
    }
}

const fn property_name(property: AnimatedProperty) -> &'static str {
    match property {
        AnimatedProperty::X => "x",
        AnimatedProperty::Y => "y",
        AnimatedProperty::Width => "width",
        AnimatedProperty::Height => "height",
        AnimatedProperty::Opacity => "opacity",
    }
}

/// Parses the document shape without the font-table checks, which differ
/// between stored documents and import sources.
fn decode_document(
    source: &str,
) -> Result<(TitleTemplate, Vec<TitleFontAsset>, Vec<TitleClock>), TitleTemplateError> {
    let length = u64::try_from(source.len()).unwrap_or(u64::MAX);
    if length > MAX_TITLE_TEMPLATE_BYTES {
        return Err(TitleTemplateError::TooLarge {
            bytes: length,
            maximum: MAX_TITLE_TEMPLATE_BYTES,
        });
    }
    let mut root = Object::new(
        json::Reader::new(source).document()?,
        "title template document",
    )?;
    let schema = root.u32("schema_version")?;
    if schema != CURRENT_SCHEMA_VERSION {
        return Err(TitleTemplateError::UnsupportedSchema {
            found: schema,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    let template = parse_template(root.take("template")?)?;
    let fonts = parse_array(root.take("fonts")?, "fonts", parse_font)?;
    let clocks = parse_array(root.take("clocks")?, "clocks", parse_clock)?;
    root.finish()?;
    Ok((template, fonts, clocks))
}

fn parse_clock(value: Value) -> Result<TitleClock, DecodeError> {
    let mut object = Object::new(value, "clock")?;
    let name = parse_clock_name(&object.string("name")?)?;
    let kind = match object.string("kind")?.as_str() {
        "time_of_day" => {
            let text = object.string("zone")?;
            let zone = TimeZone::parse_posix(&text).map_err(|error| syntax(error.to_string()))?;
            if zone.to_string() != text {
                return Err(syntax(format!(
                    "zone `{text}` is not in its canonical POSIX form `{zone}`"
                )));
            }
            let DisplayFormat::TimeOfDay(format) = parse_display_format(object.take("format")?)?
            else {
                return Err(syntax("a time_of_day clock needs a time_of_day format"));
            };
            TitleClockKind::TimeOfDay { zone, format }
        }
        "countdown_to" => {
            let target = parse_utc_seconds(&object.string("target_utc")?)?;
            let overrun = match object.string("overrun")?.as_str() {
                "stop_at_target" => OverrunPolicy::StopAtTarget,
                "continue" => OverrunPolicy::Continue,
                value => return Err(unknown_enum("clock overrun", value)),
            };
            TitleClockKind::CountdownTo {
                target,
                overrun,
                format: parse_duration_format(object.take("format")?)?,
            }
        }
        "since_take" => TitleClockKind::SinceTake {
            format: parse_duration_format(object.take("format")?)?,
        },
        value => return Err(unknown_enum("clock kind", value)),
    };
    object.finish()?;
    Ok(TitleClock { name, kind })
}

fn parse_clock_name(text: &str) -> Result<ClockName, DecodeError> {
    ClockName::new(text).map_err(|error| syntax(format!("clock `{text}`: {error}")))
}

/// Parses exactly `YYYY-MM-DDTHH:MM:SSZ`.
fn parse_utc_seconds(text: &str) -> Result<UtcInstant, DecodeError> {
    let invalid = || syntax(format!("`{text}` is not a YYYY-MM-DDTHH:MM:SSZ UTC time"));
    let bytes = text.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return Err(invalid());
    }
    let number = |range: core::ops::Range<usize>| -> Result<u32, DecodeError> {
        let digits = &text[range];
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        digits.parse().map_err(|_| invalid())
    };
    let narrow = |value: u32| u8::try_from(value).map_err(|_| invalid());
    UtcInstant::from_utc_civil(
        i32::try_from(number(0..4)?).map_err(|_| invalid())?,
        narrow(number(5..7)?)?,
        narrow(number(8..10)?)?,
        narrow(number(11..13)?)?,
        narrow(number(14..16)?)?,
        narrow(number(17..19)?)?,
        0,
    )
    .map_err(|_| invalid())
}

fn parse_duration_format(value: Value) -> Result<DurationFormat, DecodeError> {
    match parse_display_format(value)? {
        DisplayFormat::Duration(format) => Ok(format),
        DisplayFormat::TimeOfDay(_) => Err(syntax("this clock needs a duration format")),
    }
}

fn parse_display_format(value: Value) -> Result<DisplayFormat, DecodeError> {
    let mut object = Object::new(value, "clock format")?;
    let kind = object.string("type")?;
    let preset = object.string("preset")?;
    let format = match kind.as_str() {
        "duration" => DisplayFormat::Duration(match preset.as_str() {
            "hours_minutes_seconds" => DurationFormat::HoursMinutesSeconds,
            "minutes_seconds" => DurationFormat::MinutesSeconds,
            "seconds" => DurationFormat::Seconds,
            "hours_minutes_seconds_tenths" => DurationFormat::HoursMinutesSecondsTenths,
            "minutes_seconds_tenths" => DurationFormat::MinutesSecondsTenths,
            "hours_minutes_seconds_millis" => DurationFormat::HoursMinutesSecondsMillis,
            "hours_minutes_seconds_frames" => DurationFormat::HoursMinutesSecondsFrames(
                parse_frame_rate(object.take("frame_rate")?)?,
            ),
            "compact" => DurationFormat::Compact,
            value => return Err(unknown_enum("duration preset", value)),
        }),
        "time_of_day" => DisplayFormat::TimeOfDay(match preset.as_str() {
            "hours_24" => TimeOfDayFormat::Hours24,
            "hours_24_minutes" => TimeOfDayFormat::Hours24Minutes,
            "hours_12" => TimeOfDayFormat::Hours12,
            "hours_12_minutes" => TimeOfDayFormat::Hours12Minutes,
            "date" => TimeOfDayFormat::Date,
            "rfc3339" => TimeOfDayFormat::Rfc3339,
            value => return Err(unknown_enum("time of day preset", value)),
        }),
        value => return Err(unknown_enum("clock format type", value)),
    };
    object.finish()?;
    Ok(format)
}

fn parse_frame_rate(value: Value) -> Result<FrameRate, DecodeError> {
    let mut object = Object::new(value, "frame_rate")?;
    let numerator = object.u32("numerator")?;
    let denominator = object.u32("denominator")?;
    object.finish()?;
    FrameRate::new(numerator, denominator).map_err(|error| syntax(error.to_string()))
}

fn parse_font(value: Value) -> Result<TitleFontAsset, DecodeError> {
    let mut object = Object::new(value, "font")?;
    let font = TitleFontAsset {
        family: object.string("family")?,
        asset_uri: object.string("asset_uri")?,
    };
    object.finish()?;
    Ok(font)
}

fn parse_template(value: Value) -> Result<TitleTemplate, TitleTemplateError> {
    let mut object = Object::new(value, "title template")?;
    let id = TemplateId::new(object.nonzero_u128("id")?);
    let name = object.string("name")?;
    let width = object.u32("width")?;
    let height = object.u32("height")?;
    let background = parse_color(&object.string("background")?)?;
    let fields = parse_array(object.take("fields")?, "title template fields", parse_field)?;
    let mut elements = Vec::new();
    let Value::Array(values) = object.take("elements")? else {
        return Err(syntax("title template elements must be an array").into());
    };
    for value in values {
        elements.push(parse_element(value)?);
    }
    object.finish()?;
    Ok(TitleTemplate {
        id,
        name,
        width,
        height,
        background,
        fields,
        elements,
    })
}

fn parse_color(value: &str) -> Result<Color, DecodeError> {
    let color = parse_hex_color(value)?;
    Ok(Color::new(color.red, color.green, color.blue, color.alpha))
}

fn parse_field(value: Value) -> Result<FieldDefinition, DecodeError> {
    let mut object = Object::new(value, "title template field")?;
    let field = FieldDefinition {
        id: FieldId::new(object.nonzero_u128("id")?),
        name: object.string("name")?,
        default: parse_field_value(object.take("default")?)?,
    };
    object.finish()?;
    Ok(field)
}

fn parse_field_value(value: Value) -> Result<FieldValue, DecodeError> {
    let mut object = Object::new(value, "title template field default")?;
    let value = match object.string("type")?.as_str() {
        "text" => FieldValue::Text(object.string("value")?),
        "number" => FieldValue::Number(object.i64("value")?),
        "color" => FieldValue::Color(parse_color(&object.string("value")?)?),
        "image" => FieldValue::Image(ImageSource::new(object.string("asset_uri")?)),
        value => return Err(unknown_enum("title field type", value)),
    };
    object.finish()?;
    Ok(value)
}

fn parse_element(value: Value) -> Result<Element, TitleTemplateError> {
    let mut object = Object::new(value, "title element")?;
    let id = ElementId::new(object.nonzero_u128("id")?);
    let name = object.string("name")?;
    let bounds = parse_bounds(object.take("bounds")?)?;
    let z_index = object.i32("z_index")?;
    let visible = object.boolean("visible")?;
    let style = parse_style(object.take("style")?)?;
    let color_field = parse_optional(object.take("color_field")?, |value| {
        parse_field_id(&value, "color_field")
    })?;
    let kind = parse_kind(object.take("kind")?)?;
    let animations = parse_array(object.take("animations")?, "animations", parse_track)?;
    object.finish()?;
    Ok(Element {
        id,
        name,
        bounds,
        z_index,
        visible,
        style,
        color_field,
        kind,
        animations,
    })
}

fn parse_field_id(value: &Value, context: &str) -> Result<FieldId, DecodeError> {
    let Value::Number(number) = value else {
        return Err(syntax(format!("{context} must be an unsigned integer")));
    };
    core::num::NonZeroU128::new(*number)
        .map(FieldId::new)
        .ok_or_else(|| syntax(format!("{context} must be nonzero")))
}

fn parse_bounds(value: Value) -> Result<Bounds, DecodeError> {
    let mut object = Object::new(value, "title element bounds")?;
    let bounds = Bounds {
        x: object.i32("x")?,
        y: object.i32("y")?,
        width: object.u32("width")?,
        height: object.u32("height")?,
    };
    object.finish()?;
    Ok(bounds)
}

fn parse_style(value: Value) -> Result<Style, DecodeError> {
    let mut object = Object::new(value, "title element style")?;
    let fill = parse_color(&object.string("fill")?)?;
    let background = match object.take("background")? {
        Value::Null => None,
        Value::String(value) => Some(parse_color(&value)?),
        _ => return Err(syntax("field `background` must be a color or null")),
    };
    let opacity = object.u8("opacity")?;
    let font = parse_optional(object.take("font")?, parse_font_style)?;
    let alignment = parse_alignment(object.take("alignment")?)?;
    object.finish()?;
    Ok(Style {
        fill,
        background,
        opacity,
        font,
        alignment,
    })
}

fn parse_font_style(value: Value) -> Result<FontStyle, DecodeError> {
    let mut object = Object::new(value, "font style")?;
    let font = FontStyle {
        family: object.string("family")?,
        size_px: object.u32("size_px")?,
    };
    object.finish()?;
    Ok(font)
}

fn parse_alignment(value: Value) -> Result<Alignment, DecodeError> {
    let mut object = Object::new(value, "alignment")?;
    let horizontal = match object.string("horizontal")?.as_str() {
        "start" => HorizontalAlignment::Start,
        "center" => HorizontalAlignment::Center,
        "end" => HorizontalAlignment::End,
        value => return Err(unknown_enum("horizontal alignment", value)),
    };
    let vertical = match object.string("vertical")?.as_str() {
        "start" => VerticalAlignment::Start,
        "center" => VerticalAlignment::Center,
        "end" => VerticalAlignment::End,
        value => return Err(unknown_enum("vertical alignment", value)),
    };
    object.finish()?;
    Ok(Alignment {
        horizontal,
        vertical,
    })
}

fn parse_kind(value: Value) -> Result<ElementKind, DecodeError> {
    let mut object = Object::new(value, "title element kind")?;
    let kind = match object.string("type")?.as_str() {
        "text" => ElementKind::Text {
            field: FieldId::new(object.nonzero_u128("field")?),
        },
        "rectangle" => ElementKind::Rectangle,
        "image" => ElementKind::Image {
            field: FieldId::new(object.nonzero_u128("field")?),
            fit: match object.string("fit")?.as_str() {
                "stretch" => ImageFit::Stretch,
                "contain" => ImageFit::Contain,
                "cover" => ImageFit::Cover,
                "none" => ImageFit::None,
                value => return Err(unknown_enum("image fit", value)),
            },
            filter: match object.string("filter")?.as_str() {
                "nearest" => ImageFilter::Nearest,
                "bilinear" => ImageFilter::Bilinear,
                value => return Err(unknown_enum("image filter", value)),
            },
        },
        "clock" => ElementKind::Clock(ClockSpec {
            direction: match object.string("direction")?.as_str() {
                "count_up" => ClockDirection::CountUp,
                "count_down" => ClockDirection::CountDown,
                value => return Err(unknown_enum("clock direction", value)),
            },
            start_value_ms: object.u64("start_value_ms")?,
            starts_at_ms: object.u64("starts_at_ms")?,
            format: match object.string("format")?.as_str() {
                "minutes_seconds" => ClockFormat::MinutesSeconds,
                "hours_minutes_seconds" => ClockFormat::HoursMinutesSeconds,
                value => return Err(unknown_enum("clock format", value)),
            },
        }),
        "ticker" => ElementKind::Ticker(TickerSpec {
            field: FieldId::new(object.nonzero_u128("field")?),
            direction: match object.string("direction")?.as_str() {
                "left" => TickerDirection::Left,
                "right" => TickerDirection::Right,
                value => return Err(unknown_enum("ticker direction", value)),
            },
            pixels_per_second: object.u32("pixels_per_second")?,
            gap_px: object.u32("gap_px")?,
            starts_at_ms: object.u64("starts_at_ms")?,
        }),
        "production_clock" => ElementKind::ProductionClock(ProductionClockSpec {
            clock: parse_clock_name(&object.string("clock")?)?,
            format: parse_optional(object.take("format")?, parse_display_format)?,
        }),
        value => return Err(unknown_enum("title element kind", value)),
    };
    object.finish()?;
    Ok(kind)
}

fn parse_track(value: Value) -> Result<AnimationTrack, DecodeError> {
    let mut object = Object::new(value, "animation track")?;
    let property = match object.string("property")?.as_str() {
        "x" => AnimatedProperty::X,
        "y" => AnimatedProperty::Y,
        "width" => AnimatedProperty::Width,
        "height" => AnimatedProperty::Height,
        "opacity" => AnimatedProperty::Opacity,
        value => return Err(unknown_enum("animated property", value)),
    };
    let keyframes = parse_array(object.take("keyframes")?, "keyframes", parse_keyframe)?;
    object.finish()?;
    Ok(AnimationTrack {
        property,
        keyframes,
    })
}

fn parse_keyframe(value: Value) -> Result<Keyframe, DecodeError> {
    let mut object = Object::new(value, "keyframe")?;
    let keyframe = Keyframe {
        at_ms: object.u64("at_ms")?,
        value: object.i64("value")?,
        interpolation: match object.string("interpolation")?.as_str() {
            "hold" => Interpolation::Hold,
            "linear" => Interpolation::Linear,
            value => return Err(unknown_enum("interpolation", value)),
        },
    };
    object.finish()?;
    Ok(keyframe)
}

/// The audit issue for a title template that does not load.
pub(crate) fn template_issue(input_id: InputId, error: &TitleTemplateError) -> AssetAuditIssue {
    AssetAuditIssue {
        input_id,
        reason: match error {
            TitleTemplateError::Asset(error) => audit_reason(error),
            _ => AssetAuditReason::UnreadableTitleTemplate,
        },
    }
}
