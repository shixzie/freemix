use crate::{
    ClockSpec, ElementId, FieldId, ProductionClockSpec, TemplateId, TickerSpec, TitleAnimation,
    TitleId, validation::validate_template_structure,
};
use core::fmt;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);
    pub const BLACK: Self = Self::new(0, 0, 0, 255);

    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HorizontalAlignment {
    #[default]
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VerticalAlignment {
    #[default]
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Alignment {
    pub horizontal: HorizontalAlignment,
    pub vertical: VerticalAlignment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FontStyle {
    pub family: String,
    pub size_px: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Style {
    pub fill: Color,
    pub background: Option<Color>,
    pub opacity: u8,
    pub font: Option<FontStyle>,
    pub alignment: Alignment,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fill: Color::new(255, 255, 255, 255),
            background: None,
            opacity: 255,
            font: None,
            alignment: Alignment::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageSource {
    pub asset: String,
}

impl ImageSource {
    #[must_use]
    pub fn new(asset: impl Into<String>) -> Self {
        Self {
            asset: asset.into(),
        }
    }
}

/// How a decoded image is sized inside its element bounds.
///
/// Every mode except [`Self::Stretch`] keeps the source aspect ratio. The
/// placed image is positioned by the element's `style.alignment`; whatever
/// falls outside the element bounds is clipped, and element area the image
/// does not cover is left untouched (no background is painted under decoded
/// pixels). Placed sizes are integers: the scaled axis is
/// `round(other_source * bound / source)` with halves rounding up and a
/// minimum of one pixel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ImageFit {
    /// Scale each axis independently to exactly fill the bounds.
    #[default]
    Stretch,
    /// Scale uniformly so the whole image fits inside the bounds
    /// (letterbox/pillarbox). Also known as "fit" or "contain".
    Contain,
    /// Scale uniformly so the image covers the bounds, cropping the overflow.
    /// Also known as "fill" or "cover".
    Cover,
    /// Draw at the source's native pixel size, cropping any overflow.
    None,
}

/// How destination pixels sample a scaled decoded image.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ImageFilter {
    /// One source pixel per destination pixel, chosen by
    /// `floor(offset * source / placed)`.
    #[default]
    Nearest,
    /// Pixel-center bilinear interpolation of premultiplied channels with
    /// 8-bit fixed-point weights and clamp-to-edge addressing.
    Bilinear,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldType {
    Text,
    Number,
    Image,
    Color,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldValue {
    Text(String),
    Number(i64),
    Image(ImageSource),
    Color(Color),
}

impl FieldValue {
    #[must_use]
    pub const fn field_type(&self) -> FieldType {
        match self {
            Self::Text(_) => FieldType::Text,
            Self::Number(_) => FieldType::Number,
            Self::Image(_) => FieldType::Image,
            Self::Color(_) => FieldType::Color,
        }
    }

    /// Borrows the text a renderer would draw. Borrowed, not cloned: copying a
    /// field would allocate once per frame in proportion to its length.
    pub(crate) fn display_text(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::Text(value) => Some(Cow::Borrowed(value)),
            Self::Number(value) => Some(Cow::Owned(value.to_string())),
            Self::Image(_) | Self::Color(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldDefinition {
    pub id: FieldId,
    pub name: String,
    pub default: FieldValue,
    /// Whether browser clients (the web title editor, GX-006) may see and
    /// edit the field. A hidden field's name and value are never sent to a
    /// Web session; Studio and raw clients see every field.
    pub exposed: bool,
}

impl FieldDefinition {
    #[must_use]
    pub const fn field_type(&self) -> FieldType {
        self.default.field_type()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ElementKind {
    Text {
        field: FieldId,
    },
    Rectangle,
    /// Draws an image field. When the referenced asset has decoded pixels in
    /// the renderer's [`crate::AssetCatalog`] they are placed inside the
    /// element bounds by `fit` (aligned by `style.alignment`) and resampled
    /// with `filter`; otherwise the element draws the image placeholder.
    Image {
        field: FieldId,
        fit: ImageFit,
        filter: ImageFilter,
    },
    /// A scene-time clock that starts with the title.
    Clock(ClockSpec),
    /// A shared production clock read from the frame's
    /// [`crate::ClockSource`]; see [`crate::production_clock`].
    ProductionClock(ProductionClockSpec),
    Ticker(TickerSpec),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Element {
    pub id: ElementId,
    pub name: String,
    pub bounds: Bounds,
    pub z_index: i32,
    pub visible: bool,
    pub style: Style,
    /// Optional color field overriding `style.fill`.
    pub color_field: Option<FieldId>,
    pub kind: ElementKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleTemplate {
    pub id: TemplateId,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub background: Color,
    pub fields: Vec<FieldDefinition>,
    pub elements: Vec<Element>,
    /// Keyframe animation of the elements; see [`crate::TitleAnimation`].
    pub animation: TitleAnimation,
}

impl TitleTemplate {
    /// Creates a live scene while preserving template, element, and field IDs.
    ///
    /// # Errors
    ///
    /// Returns all structural validation errors. Missing external assets are
    /// reported separately by [`crate::validate_template`] and rendering.
    pub fn instantiate(&self, id: TitleId) -> Result<TitleScene, InstantiationError> {
        let errors = validate_template_structure(self);
        if !errors.is_empty() {
            return Err(InstantiationError { errors });
        }
        Ok(TitleScene {
            id,
            template_id: self.id,
            width: self.width,
            height: self.height,
            background: self.background,
            fields: TitleFieldSet::from_definitions(&self.fields),
            elements: self.elements.clone(),
            animation: self.animation.clone(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstantiationError {
    pub errors: Vec<crate::ValidationError>,
}

impl fmt::Display for InstantiationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "template has {} validation error(s)",
            self.errors.len()
        )
    }
}

impl std::error::Error for InstantiationError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleScene {
    id: TitleId,
    template_id: TemplateId,
    width: u32,
    height: u32,
    background: Color,
    fields: TitleFieldSet,
    elements: Vec<Element>,
    animation: TitleAnimation,
}

impl TitleScene {
    #[must_use]
    pub const fn id(&self) -> TitleId {
        self.id
    }

    #[must_use]
    pub const fn template_id(&self) -> TemplateId {
        self.template_id
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub const fn background(&self) -> Color {
        self.background
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.fields.revision()
    }

    #[must_use]
    pub fn field_value(&self, field: FieldId) -> Option<&FieldValue> {
        self.fields.value(field)
    }

    #[must_use]
    pub const fn fields(&self) -> &BTreeMap<FieldId, FieldValue> {
        self.fields.values()
    }

    /// Returns the typed field set this scene draws from.
    #[must_use]
    pub const fn field_set(&self) -> &TitleFieldSet {
        &self.fields
    }

    #[must_use]
    pub fn elements(&self) -> &[Element] {
        &self.elements
    }

    /// The template animation this scene samples.
    #[must_use]
    pub const fn animation(&self) -> &TitleAnimation {
        &self.animation
    }

    /// Atomically applies typed field values if `expected_revision` is current.
    /// An empty update is a no-op and does not advance the revision.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`TitleFieldSet::update`] without changing any
    /// field.
    pub fn update_fields(
        &mut self,
        expected_revision: u64,
        updates: &[(FieldId, FieldValue)],
    ) -> Result<u64, UpdateError> {
        self.fields.update(expected_revision, updates)
    }

    /// Applies one optimistic live-field update.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::update_fields`].
    pub fn update_field(
        &mut self,
        expected_revision: u64,
        field: FieldId,
        value: FieldValue,
    ) -> Result<u64, UpdateError> {
        self.update_fields(expected_revision, &[(field, value)])
    }
}

/// Most fields one title may define.
pub const MAX_TITLE_FIELDS: usize = 128;
/// Longest text field value, in Unicode scalar values.
pub const MAX_TITLE_TEXT_CHARS: usize = 4_096;
/// Largest total of text values plus image asset names, in UTF-8 bytes.
pub const MAX_TITLE_CONTENT_BYTES: usize = 12_288;

/// UTF-8 bytes `value` contributes to [`MAX_TITLE_CONTENT_BYTES`].
fn content_bytes(value: &FieldValue) -> usize {
    match value {
        FieldValue::Text(text) => text.len(),
        FieldValue::Image(image) => image.asset.len(),
        FieldValue::Number(_) | FieldValue::Color(_) => 0,
    }
}

/// The typed, revisioned field values of one title, independent of layout.
///
/// Every field the template defines has exactly one value of the defined
/// type, and the values stay within [`MAX_TITLE_FIELDS`],
/// [`MAX_TITLE_TEXT_CHARS`], and [`MAX_TITLE_CONTENT_BYTES`]. The revision is
/// an optimistic-concurrency counter that advances once per non-empty
/// [`Self::update`]. Each field also carries its own revision: the title
/// revision of the update that last changed its value (GX-006), never above
/// the title revision. An update is checked per field, so editors changing
/// different fields of one title never conflict.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleFieldSet {
    definitions: Vec<FieldDefinition>,
    field_types: BTreeMap<FieldId, FieldType>,
    values: BTreeMap<FieldId, FieldValue>,
    field_revisions: BTreeMap<FieldId, u64>,
    revision: u64,
}

impl TitleFieldSet {
    /// Creates the field set at revision zero holding the template defaults.
    ///
    /// # Errors
    ///
    /// Returns a [`FieldSetError`] when the template repeats a field id,
    /// defines too many fields, or a default breaks a value bound.
    pub fn from_template(template: &TitleTemplate) -> Result<Self, FieldSetError> {
        Self::restore(
            template,
            0,
            template
                .fields
                .iter()
                .map(|field| (field.id, field.default.clone())),
        )
    }

    /// Restores persisted values at `revision` against `template`, every
    /// field at that revision (as if each had just changed).
    ///
    /// `values` must cover the template fields exactly, each with the
    /// defined type, and stay within the value bounds.
    ///
    /// # Errors
    ///
    /// Returns a [`FieldSetError`] naming the first offending field.
    pub fn restore(
        template: &TitleTemplate,
        revision: u64,
        values: impl IntoIterator<Item = (FieldId, FieldValue)>,
    ) -> Result<Self, FieldSetError> {
        Self::restore_revisioned(
            template,
            revision,
            values
                .into_iter()
                .map(|(field, value)| (field, value, revision)),
        )
    }

    /// Restores persisted values and their per-field revisions at
    /// `revision` against `template`.
    ///
    /// `values` must cover the template fields exactly, each with the
    /// defined type and a field revision no newer than `revision`, and stay
    /// within the value bounds.
    ///
    /// # Errors
    ///
    /// Returns a [`FieldSetError`] naming the first offending field.
    pub fn restore_revisioned(
        template: &TitleTemplate,
        revision: u64,
        values: impl IntoIterator<Item = (FieldId, FieldValue, u64)>,
    ) -> Result<Self, FieldSetError> {
        if template.fields.len() > MAX_TITLE_FIELDS {
            return Err(FieldSetError::TooManyFields {
                count: template.fields.len(),
            });
        }
        let mut field_types = BTreeMap::new();
        for field in &template.fields {
            if field_types.insert(field.id, field.field_type()).is_some() {
                return Err(FieldSetError::DuplicateDefinition(field.id));
            }
        }
        let mut restored = BTreeMap::new();
        let mut field_revisions = BTreeMap::new();
        let mut bytes = 0_usize;
        for (field, value, field_revision) in values {
            if restored.contains_key(&field) {
                return Err(FieldSetError::DuplicateField(field));
            }
            let Some(expected) = field_types.get(&field).copied() else {
                return Err(FieldSetError::UnknownField(field));
            };
            check_value(field, expected, &value)
                .map_err(|error| FieldSetError::from_value(&error))?;
            if field_revision > revision {
                return Err(FieldSetError::FieldRevisionAhead {
                    field,
                    field_revision,
                    revision,
                });
            }
            bytes = bytes.saturating_add(content_bytes(&value));
            restored.insert(field, value);
            field_revisions.insert(field, field_revision);
        }
        if let Some(missing) = field_types.keys().find(|id| !restored.contains_key(id)) {
            return Err(FieldSetError::MissingField(*missing));
        }
        if bytes > MAX_TITLE_CONTENT_BYTES {
            return Err(FieldSetError::ContentTooLarge { bytes });
        }
        Ok(Self {
            definitions: template.fields.clone(),
            field_types,
            values: restored,
            field_revisions,
            revision,
        })
    }

    /// Builds a revision-zero set from definitions already checked for
    /// structure, without applying the value bounds (scene instantiation
    /// predates them).
    pub(crate) fn from_definitions(definitions: &[FieldDefinition]) -> Self {
        Self {
            definitions: definitions.to_vec(),
            field_types: definitions
                .iter()
                .map(|field| (field.id, field.field_type()))
                .collect(),
            values: definitions
                .iter()
                .map(|field| (field.id, field.default.clone()))
                .collect(),
            field_revisions: definitions.iter().map(|field| (field.id, 0)).collect(),
            revision: 0,
        }
    }

    /// Returns the template's field definitions in template order.
    #[must_use]
    pub fn definitions(&self) -> &[FieldDefinition] {
        &self.definitions
    }

    /// Marks `field` exposed to (or hidden from) the browser title editor,
    /// following its template. Values and the revision do not change.
    /// Returns `false` when the set has no such field.
    pub fn set_exposed(&mut self, field: FieldId, exposed: bool) -> bool {
        match self
            .definitions
            .iter_mut()
            .find(|definition| definition.id == field)
        {
            Some(definition) => {
                definition.exposed = exposed;
                true
            }
            None => false,
        }
    }

    #[must_use]
    pub fn field_type(&self, field: FieldId) -> Option<FieldType> {
        self.field_types.get(&field).copied()
    }

    #[must_use]
    pub fn value(&self, field: FieldId) -> Option<&FieldValue> {
        self.values.get(&field)
    }

    #[must_use]
    pub const fn values(&self) -> &BTreeMap<FieldId, FieldValue> {
        &self.values
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The title revision that last changed `field`'s value (zero for a
    /// template default never changed), or `None` for an unknown field.
    #[must_use]
    pub fn field_revision(&self, field: FieldId) -> Option<u64> {
        self.field_revisions.get(&field).copied()
    }

    /// Returns the text plus image-asset UTF-8 bytes the values hold.
    #[must_use]
    pub fn content_bytes(&self) -> usize {
        self.values
            .values()
            .map(content_bytes)
            .fold(0, usize::saturating_add)
    }

    /// Atomically applies typed field values made against
    /// `expected_revision`, the title revision the editor last saw.
    ///
    /// The update is accepted when `expected_revision` is not ahead of the
    /// title and no updated field changed after it, so edits to different
    /// fields made against the same revision all apply, while a stale edit
    /// of a field someone else changed is refused. An accepted non-empty
    /// update advances the title revision by one and moves each field whose
    /// value it changes to that revision. An empty update is a no-op and
    /// does not advance the revision.
    ///
    /// # Errors
    ///
    /// Returns a conflict, stale field, unknown/duplicate field, type
    /// mismatch, blank image asset, over-long text, oversized total content,
    /// or revision exhaustion without changing any field.
    pub fn update(
        &mut self,
        expected_revision: u64,
        updates: &[(FieldId, FieldValue)],
    ) -> Result<u64, UpdateError> {
        if expected_revision > self.revision {
            return Err(UpdateError::RevisionConflict {
                expected: expected_revision,
                actual: self.revision,
            });
        }
        if updates.is_empty() {
            return Ok(self.revision);
        }

        let mut seen = BTreeSet::new();
        let mut bytes = self.content_bytes();
        for (field, value) in updates {
            if !seen.insert(*field) {
                return Err(UpdateError::DuplicateField(*field));
            }
            let Some(expected) = self.field_types.get(field).copied() else {
                return Err(UpdateError::UnknownField(*field));
            };
            check_value(*field, expected, value)?;
            let changed = self.field_revisions.get(field).copied().unwrap_or(0);
            if changed > expected_revision {
                return Err(UpdateError::StaleField {
                    field: *field,
                    expected: expected_revision,
                    changed,
                });
            }
            let previous = self.values.get(field).map_or(0, content_bytes);
            bytes = bytes
                .saturating_sub(previous)
                .saturating_add(content_bytes(value));
        }
        if bytes > MAX_TITLE_CONTENT_BYTES {
            return Err(UpdateError::ContentTooLarge { bytes });
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(UpdateError::RevisionExhausted)?;
        for (field, value) in updates {
            if self.values.get(field) != Some(value) {
                self.values.insert(*field, value.clone());
                self.field_revisions.insert(*field, revision);
            }
        }
        self.revision = revision;
        Ok(revision)
    }
}

/// Checks one value against its defined type and the per-value bounds.
fn check_value(field: FieldId, expected: FieldType, value: &FieldValue) -> Result<(), UpdateError> {
    let actual = value.field_type();
    if actual != expected {
        return Err(UpdateError::TypeMismatch {
            field,
            expected,
            actual,
        });
    }
    match value {
        FieldValue::Image(image) if image.asset.trim().is_empty() => {
            Err(UpdateError::EmptyImageAsset(field))
        }
        FieldValue::Text(text) => {
            let chars = text.chars().count();
            if chars > MAX_TITLE_TEXT_CHARS {
                Err(UpdateError::TextTooLong { field, chars })
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

/// Why a [`TitleFieldSet`] cannot be built or restored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldSetError {
    /// The template defines more than [`MAX_TITLE_FIELDS`] fields.
    TooManyFields { count: usize },
    /// The template defines the same field id twice.
    DuplicateDefinition(FieldId),
    /// A value names a field the template does not define.
    UnknownField(FieldId),
    /// A value is supplied twice for the same field.
    DuplicateField(FieldId),
    /// A template field has no value.
    MissingField(FieldId),
    TypeMismatch {
        field: FieldId,
        expected: FieldType,
        actual: FieldType,
    },
    /// An image value names a blank asset.
    EmptyImageAsset(FieldId),
    /// A text value exceeds [`MAX_TITLE_TEXT_CHARS`].
    TextTooLong { field: FieldId, chars: usize },
    /// Text and image assets together exceed [`MAX_TITLE_CONTENT_BYTES`].
    ContentTooLarge { bytes: usize },
    /// A field's revision is newer than the title's.
    FieldRevisionAhead {
        field: FieldId,
        field_revision: u64,
        revision: u64,
    },
}

impl FieldSetError {
    fn from_value(error: &UpdateError) -> Self {
        match *error {
            UpdateError::TypeMismatch {
                field,
                expected,
                actual,
            } => Self::TypeMismatch {
                field,
                expected,
                actual,
            },
            UpdateError::EmptyImageAsset(field) => Self::EmptyImageAsset(field),
            UpdateError::TextTooLong { field, chars } => Self::TextTooLong { field, chars },
            _ => unreachable!("value checks report only per-value errors"),
        }
    }
}

impl fmt::Display for FieldSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyFields { count } => write!(
                formatter,
                "title defines {count} fields, maximum is {MAX_TITLE_FIELDS}"
            ),
            Self::DuplicateDefinition(field) => {
                write!(formatter, "title defines field {field} twice")
            }
            Self::UnknownField(field) => write!(formatter, "unknown field {field}"),
            Self::DuplicateField(field) => write!(formatter, "field {field} has two values"),
            Self::MissingField(field) => write!(formatter, "field {field} has no value"),
            Self::TypeMismatch {
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "field {field} expects {expected:?}, received {actual:?}"
            ),
            Self::EmptyImageAsset(field) => {
                write!(formatter, "field {field} names an empty image asset")
            }
            Self::TextTooLong { field, chars } => write!(
                formatter,
                "field {field} has {chars} characters, maximum is {MAX_TITLE_TEXT_CHARS}"
            ),
            Self::ContentTooLarge { bytes } => write!(
                formatter,
                "title content is {bytes} bytes, maximum is {MAX_TITLE_CONTENT_BYTES}"
            ),
            Self::FieldRevisionAhead {
                field,
                field_revision,
                revision,
            } => write!(
                formatter,
                "field {field} is at revision {field_revision}, after the title's {revision}"
            ),
        }
    }
}

impl std::error::Error for FieldSetError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateError {
    /// The update names a title revision newer than the title's.
    RevisionConflict {
        expected: u64,
        actual: u64,
    },
    /// The update was made against revision `expected`, but `field` changed
    /// at the later revision `changed`.
    StaleField {
        field: FieldId,
        expected: u64,
        changed: u64,
    },
    UnknownField(FieldId),
    DuplicateField(FieldId),
    TypeMismatch {
        field: FieldId,
        expected: FieldType,
        actual: FieldType,
    },
    /// An image value names a blank asset, which template validation rejects.
    EmptyImageAsset(FieldId),
    /// A text value exceeds [`MAX_TITLE_TEXT_CHARS`].
    TextTooLong {
        field: FieldId,
        chars: usize,
    },
    /// The updated text and image assets together would exceed
    /// [`MAX_TITLE_CONTENT_BYTES`].
    ContentTooLarge {
        bytes: usize,
    },
    RevisionExhausted,
}

impl fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionConflict { expected, actual } => {
                write!(
                    formatter,
                    "expected revision {expected}, current revision is {actual}"
                )
            }
            Self::StaleField {
                field,
                expected,
                changed,
            } => write!(
                formatter,
                "field {field} changed at revision {changed}, after the edit's revision {expected}"
            ),
            Self::UnknownField(field) => write!(formatter, "unknown field {field}"),
            Self::DuplicateField(field) => write!(formatter, "field {field} is updated twice"),
            Self::TypeMismatch {
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "field {field} expects {expected:?}, received {actual:?}"
            ),
            Self::EmptyImageAsset(field) => {
                write!(formatter, "field {field} names an empty image asset")
            }
            Self::TextTooLong { field, chars } => write!(
                formatter,
                "field {field} has {chars} characters, maximum is {MAX_TITLE_TEXT_CHARS}"
            ),
            Self::ContentTooLarge { bytes } => write!(
                formatter,
                "title content would be {bytes} bytes, maximum is {MAX_TITLE_CONTENT_BYTES}"
            ),
            Self::RevisionExhausted => formatter.write_str("title revision is exhausted"),
        }
    }
}

impl std::error::Error for UpdateError {}
