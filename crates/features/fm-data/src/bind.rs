//! Data-to-title bindings covering text, number, color, image URI,
//! visibility, and formatted number/date fields, plus list-row selection.
//!
//! Like [`crate::TitleMapper`] this does not depend on the titles crate; a
//! composition layer converts [`BoundValue`] into renderer field updates.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use crate::{
    CoercionError, CoercionPolicy, DataPath, DataValue, DateTimeFormat, Decimal, FormatError,
    NumberFormat, PathError, RowCursor, RowSelectionError, TitleColor, TitleFieldKind,
    TitleFieldValue, Transform, TransformError, ValueType, coerce_title_value,
    format::numeric_value,
};

// ---------------------------------------------------------------- visibility

/// Numeric comparison operator for visibility rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CompareOp {
    const fn holds(self, ordering: Ordering) -> bool {
        match self {
            Self::Eq => matches!(ordering, Ordering::Equal),
            Self::Ne => !matches!(ordering, Ordering::Equal),
            Self::Lt => matches!(ordering, Ordering::Less),
            Self::Le => !matches!(ordering, Ordering::Greater),
            Self::Gt => matches!(ordering, Ordering::Greater),
            Self::Ge => !matches!(ordering, Ordering::Less),
        }
    }
}

/// How a data value decides visibility.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TruthyRule {
    /// Booleans; integers `0`/`1`; strings `true/false`, `yes/no`, `on/off`,
    /// `1/0` (ASCII case-insensitive, trimmed). Anything else is an error.
    /// A missing value follows the binding's fallback/required rules.
    Explicit,
    /// Visible when present and non-empty: non-blank strings, non-empty
    /// lists/objects, and any boolean or number. Missing, `Null`, and blank
    /// strings are hidden (never an error).
    NonEmpty,
    /// Visible when the exact numeric value compares true against `operand`.
    /// Non-numeric values are an error; a missing value follows the
    /// binding's fallback/required rules.
    Compare { op: CompareOp, operand: Decimal },
}

/// A visibility binding rule with optional inversion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisibilityRule {
    pub rule: TruthyRule,
    /// Inverts the evaluated result (not the fallback).
    pub invert: bool,
}

impl VisibilityRule {
    #[must_use]
    pub const fn new(rule: TruthyRule) -> Self {
        Self {
            rule,
            invert: false,
        }
    }

    #[must_use]
    pub const fn inverted(mut self) -> Self {
        self.invert = !self.invert;
        self
    }

    /// Evaluates a present, non-null value.
    ///
    /// # Errors
    ///
    /// Returns a [`VisibilityError`] when the value does not fit the rule.
    pub fn evaluate(&self, value: &DataValue) -> Result<bool, VisibilityError> {
        let visible = match &self.rule {
            TruthyRule::Explicit => explicit_truth(value)?,
            TruthyRule::NonEmpty => match value {
                DataValue::Null => false,
                DataValue::String(text) => !text.trim().is_empty(),
                DataValue::List(items) => !items.is_empty(),
                DataValue::Object(fields) => !fields.is_empty(),
                DataValue::Bool(_) | DataValue::Integer(_) | DataValue::Decimal(_) => true,
            },
            TruthyRule::Compare { op, operand } => {
                let number = numeric_value(value).map_err(|_| VisibilityError::NotNumeric {
                    actual: value.value_type(),
                })?;
                op.holds(number.cmp(operand))
            }
        };
        Ok(visible != self.invert)
    }
}

fn explicit_truth(value: &DataValue) -> Result<bool, VisibilityError> {
    match value {
        DataValue::Bool(flag) => Ok(*flag),
        DataValue::Integer(0) => Ok(false),
        DataValue::Integer(1) => Ok(true),
        DataValue::String(text) => {
            let word = text.trim().to_ascii_lowercase();
            match word.as_str() {
                "true" | "yes" | "on" | "1" => Ok(true),
                "false" | "no" | "off" | "0" => Ok(false),
                _ => Err(VisibilityError::NotBoolean(text.clone())),
            }
        }
        other => Err(VisibilityError::NotBoolean(
            other
                .scalar_text()
                .unwrap_or_else(|| format!("{:?}", other.value_type())),
        )),
    }
}

/// A value does not satisfy a visibility rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VisibilityError {
    NotBoolean(String),
    NotNumeric { actual: ValueType },
}

impl fmt::Display for VisibilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBoolean(text) => write!(formatter, "`{text}` is not an explicit boolean"),
            Self::NotNumeric { actual } => {
                write!(
                    formatter,
                    "visibility comparison needs a number, found {actual:?}"
                )
            }
        }
    }
}

impl std::error::Error for VisibilityError {}

// ---------------------------------------------------------------- image URI

/// Validation rules for image URI bindings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageUriPolicy {
    /// Lowercase schemes accepted (e.g. `https`).
    pub allowed_schemes: Vec<String>,
    /// Accept scheme-less references (relative paths, `C:\...` drive paths,
    /// or absolute paths) for the media layer to resolve.
    pub allow_paths: bool,
    pub max_bytes: usize,
}

impl Default for ImageUriPolicy {
    fn default() -> Self {
        Self {
            allowed_schemes: vec!["https".to_owned(), "http".to_owned(), "file".to_owned()],
            allow_paths: false,
            max_bytes: 2048,
        }
    }
}

impl ImageUriPolicy {
    pub const MAX_SCHEMES: usize = 16;
    pub const MAX_BYTES_LIMIT: usize = 8192;

    fn validate(&self) -> Result<(), ImageUriError> {
        if self.max_bytes == 0 || self.max_bytes > Self::MAX_BYTES_LIMIT {
            return Err(ImageUriError::InvalidPolicy);
        }
        if self.allowed_schemes.len() > Self::MAX_SCHEMES {
            return Err(ImageUriError::InvalidPolicy);
        }
        for scheme in &self.allowed_schemes {
            if !valid_scheme(scheme) || scheme.bytes().any(|b| b.is_ascii_uppercase()) {
                return Err(ImageUriError::InvalidPolicy);
            }
        }
        Ok(())
    }

    /// Validates `text` and returns the trimmed reference with a lowercased
    /// scheme. `http`/`https` require a non-empty authority.
    ///
    /// # Errors
    ///
    /// Returns an [`ImageUriError`] describing the first violated rule.
    pub fn check(&self, text: &str) -> Result<String, ImageUriError> {
        self.validate()?;
        let reference = text.trim();
        if reference.is_empty() {
            return Err(ImageUriError::Empty);
        }
        if reference.len() > self.max_bytes {
            return Err(ImageUriError::TooLong {
                limit: self.max_bytes,
                actual: reference.len(),
            });
        }
        if reference
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(ImageUriError::InvalidCharacter);
        }
        let scheme = reference
            .split_once(':')
            .map(|(scheme, _)| scheme)
            // A single letter before `:` is a Windows drive, not a scheme.
            .filter(|scheme| scheme.len() > 1 && valid_scheme(scheme));
        let Some(scheme) = scheme else {
            return if self.allow_paths {
                Ok(reference.to_owned())
            } else {
                Err(ImageUriError::MissingScheme)
            };
        };
        let lower = scheme.to_ascii_lowercase();
        if !self.allowed_schemes.contains(&lower) {
            return Err(ImageUriError::SchemeNotAllowed(lower));
        }
        let rest = &reference[scheme.len() + 1..];
        if matches!(lower.as_str(), "http" | "https") {
            let authority = rest
                .strip_prefix("//")
                .map(|after| after.split(['/', '?', '#']).next().unwrap_or_default());
            if authority.is_none_or(str::is_empty) {
                return Err(ImageUriError::MissingAuthority);
            }
        }
        Ok(format!("{lower}:{rest}"))
    }
}

fn valid_scheme(scheme: &str) -> bool {
    let mut bytes = scheme.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// An image URI failed validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImageUriError {
    InvalidPolicy,
    Unsupported { actual: ValueType },
    Empty,
    TooLong { limit: usize, actual: usize },
    InvalidCharacter,
    MissingScheme,
    SchemeNotAllowed(String),
    MissingAuthority,
}

impl fmt::Display for ImageUriError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicy => formatter.write_str("image URI policy is invalid"),
            Self::Unsupported { actual } => {
                write!(formatter, "image URI must be a string, found {actual:?}")
            }
            Self::Empty => formatter.write_str("image URI is empty"),
            Self::TooLong { limit, actual } => {
                write!(formatter, "image URI is {actual} bytes; limit is {limit}")
            }
            Self::InvalidCharacter => {
                formatter.write_str("image URI contains whitespace or a control character")
            }
            Self::MissingScheme => formatter.write_str("image URI has no scheme"),
            Self::SchemeNotAllowed(scheme) => write!(formatter, "scheme `{scheme}` is not allowed"),
            Self::MissingAuthority => formatter.write_str("image URI has no host"),
        }
    }
}

impl std::error::Error for ImageUriError {}

// ---------------------------------------------------------------- bindings

/// What a binding produces and how.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingKind {
    Text,
    /// Integer field via [`CoercionPolicy::rounding`].
    Number,
    Color,
    Image(ImageUriPolicy),
    Visibility(VisibilityRule),
    /// Text rendered by a [`NumberFormat`].
    FormattedNumber(NumberFormat),
    /// Text rendered by a [`DateTimeFormat`].
    FormattedDateTime(DateTimeFormat),
}

/// The output shape of a [`BindingKind`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundValueKind {
    Text,
    Number,
    Color,
    Image,
    Visibility,
}

impl BindingKind {
    #[must_use]
    pub const fn output(&self) -> BoundValueKind {
        match self {
            Self::Text | Self::FormattedNumber(_) | Self::FormattedDateTime(_) => {
                BoundValueKind::Text
            }
            Self::Number => BoundValueKind::Number,
            Self::Color => BoundValueKind::Color,
            Self::Image(_) => BoundValueKind::Image,
            Self::Visibility(_) => BoundValueKind::Visibility,
        }
    }
}

/// A title-ready value produced by a binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundValue {
    Text(String),
    Number(i64),
    Color(TitleColor),
    Image(String),
    Visible(bool),
}

impl BoundValue {
    #[must_use]
    pub const fn kind(&self) -> BoundValueKind {
        match self {
            Self::Text(_) => BoundValueKind::Text,
            Self::Number(_) => BoundValueKind::Number,
            Self::Color(_) => BoundValueKind::Color,
            Self::Image(_) => BoundValueKind::Image,
            Self::Visible(_) => BoundValueKind::Visibility,
        }
    }
}

impl From<TitleFieldValue> for BoundValue {
    fn from(value: TitleFieldValue) -> Self {
        match value {
            TitleFieldValue::Text(text) => Self::Text(text),
            TitleFieldValue::Number(number) => Self::Number(number),
            TitleFieldValue::Color(color) => Self::Color(color),
            TitleFieldValue::Image(reference) => Self::Image(reference),
        }
    }
}

/// Binds one data path onto one title field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataBinding {
    pub field: String,
    pub source: DataPath,
    pub kind: BindingKind,
    pub transforms: Vec<Transform>,
    pub required: bool,
    pub fallback: Option<BoundValue>,
}

impl DataBinding {
    #[must_use]
    pub fn new(field: impl Into<String>, source: DataPath, kind: BindingKind) -> Self {
        Self {
            field: field.into(),
            source,
            kind,
            transforms: Vec::new(),
            required: true,
            fallback: None,
        }
    }

    #[must_use]
    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    #[must_use]
    pub fn with_fallback(mut self, fallback: BoundValue) -> Self {
        self.fallback = Some(fallback);
        self
    }

    #[must_use]
    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transforms.push(transform);
        self
    }
}

/// A binding set is invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingConfigError {
    TooManyBindings {
        limit: usize,
        actual: usize,
    },
    TooManyTransforms {
        field: String,
        limit: usize,
    },
    EmptyFieldName,
    DuplicateField(String),
    Format {
        field: String,
        error: FormatError,
    },
    Image {
        field: String,
        error: ImageUriError,
    },
    FallbackKindMismatch {
        field: String,
        expected: BoundValueKind,
        actual: BoundValueKind,
    },
    InvalidFallback {
        field: String,
        error: BindingError,
    },
}

impl fmt::Display for BindingConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyBindings { limit, actual } => {
                write!(formatter, "{actual} bindings exceed the limit of {limit}")
            }
            Self::TooManyTransforms { field, limit } => {
                write!(formatter, "field `{field}` exceeds {limit} transforms")
            }
            Self::EmptyFieldName => formatter.write_str("title field name is empty"),
            Self::DuplicateField(field) => write!(formatter, "field `{field}` is bound twice"),
            Self::Format { field, error } => write!(formatter, "field `{field}`: {error}"),
            Self::Image { field, error } => write!(formatter, "field `{field}`: {error}"),
            Self::FallbackKindMismatch {
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "fallback for `{field}` is {actual:?}; expected {expected:?}"
            ),
            Self::InvalidFallback { field, error } => {
                write!(formatter, "fallback for `{field}` is invalid: {error}")
            }
        }
    }
}

impl std::error::Error for BindingConfigError {}

/// Why one binding failed for one row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingError {
    Path(PathError),
    Transform(TransformError),
    Coercion(CoercionError),
    Format(FormatError),
    Visibility(VisibilityError),
    Image(ImageUriError),
}

impl fmt::Display for BindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(error) => error.fmt(formatter),
            Self::Transform(error) => error.fmt(formatter),
            Self::Coercion(error) => error.fmt(formatter),
            Self::Format(error) => error.fmt(formatter),
            Self::Visibility(error) => error.fmt(formatter),
            Self::Image(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for BindingError {}

/// The outcome for one binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingStatus {
    Mapped,
    Fallback,
    Omitted,
    Missing,
    Failed(BindingError),
}

impl BindingStatus {
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(self, Self::Missing | Self::Failed(_))
    }
}

/// Report entry for one binding, in declaration order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingFieldReport {
    pub field: String,
    pub status: BindingStatus,
}

/// Bound values plus complete per-field diagnostics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BindingReport {
    pub values: BTreeMap<String, BoundValue>,
    pub fields: Vec<BindingFieldReport>,
}

impl BindingReport {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.fields.iter().all(|field| !field.status.is_error())
    }

    pub fn errors(&self) -> impl Iterator<Item = &BindingFieldReport> {
        self.fields.iter().filter(|field| field.status.is_error())
    }
}

/// Selecting the list row to bind failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowBindingError {
    NoSelection,
    Selection(RowSelectionError),
}

impl fmt::Display for RowBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSelection => formatter.write_str("no row is selected"),
            Self::Selection(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RowBindingError {}

/// A validated, bounded set of [`DataBinding`]s.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingSet {
    bindings: Vec<DataBinding>,
    policy: CoercionPolicy,
}

impl BindingSet {
    pub const MAX_BINDINGS: usize = 256;
    pub const MAX_TRANSFORMS_PER_BINDING: usize = 16;

    /// Validates count, unique non-empty names, formatter and image policies,
    /// and fallbacks (matching output kind and passing the binding's rules).
    ///
    /// # Errors
    ///
    /// Returns the first [`BindingConfigError`] in declaration order.
    pub fn new(
        bindings: Vec<DataBinding>,
        policy: CoercionPolicy,
    ) -> Result<Self, BindingConfigError> {
        if bindings.len() > Self::MAX_BINDINGS {
            return Err(BindingConfigError::TooManyBindings {
                limit: Self::MAX_BINDINGS,
                actual: bindings.len(),
            });
        }
        let mut seen = BTreeSet::new();
        for binding in &bindings {
            let field = || binding.field.clone();
            if binding.field.is_empty() {
                return Err(BindingConfigError::EmptyFieldName);
            }
            if !seen.insert(binding.field.as_str()) {
                return Err(BindingConfigError::DuplicateField(field()));
            }
            if binding.transforms.len() > Self::MAX_TRANSFORMS_PER_BINDING {
                return Err(BindingConfigError::TooManyTransforms {
                    field: field(),
                    limit: Self::MAX_TRANSFORMS_PER_BINDING,
                });
            }
            match &binding.kind {
                BindingKind::FormattedNumber(format) => {
                    format
                        .validate()
                        .map_err(|error| BindingConfigError::Format {
                            field: field(),
                            error,
                        })?;
                }
                BindingKind::Image(image) => {
                    image
                        .validate()
                        .map_err(|error| BindingConfigError::Image {
                            field: field(),
                            error,
                        })?;
                }
                _ => {}
            }
            if let Some(fallback) = &binding.fallback {
                let expected = binding.kind.output();
                if fallback.kind() != expected {
                    return Err(BindingConfigError::FallbackKindMismatch {
                        field: field(),
                        expected,
                        actual: fallback.kind(),
                    });
                }
                validate_fallback(fallback, &binding.kind, &policy).map_err(|error| {
                    BindingConfigError::InvalidFallback {
                        field: field(),
                        error,
                    }
                })?;
            }
        }
        Ok(Self { bindings, policy })
    }

    #[must_use]
    pub fn bindings(&self) -> &[DataBinding] {
        &self.bindings
    }

    #[must_use]
    pub const fn policy(&self) -> &CoercionPolicy {
        &self.policy
    }

    /// Maps one data row.
    #[must_use]
    pub fn map_row(&self, row: &DataValue) -> BindingReport {
        let mut report = BindingReport {
            values: BTreeMap::new(),
            fields: Vec::with_capacity(self.bindings.len()),
        };
        for binding in &self.bindings {
            let status = match self.resolve(binding, row) {
                Ok(Some(value)) => {
                    report.values.insert(binding.field.clone(), value);
                    BindingStatus::Mapped
                }
                Ok(None) => match &binding.fallback {
                    Some(fallback) => {
                        report
                            .values
                            .insert(binding.field.clone(), fallback.clone());
                        BindingStatus::Fallback
                    }
                    None if binding.required => BindingStatus::Missing,
                    None => BindingStatus::Omitted,
                },
                Err(error) => BindingStatus::Failed(error),
            };
            report.fields.push(BindingFieldReport {
                field: binding.field.clone(),
                status,
            });
        }
        report
    }

    /// Maps the row selected by `cursor` from a list.
    ///
    /// # Errors
    ///
    /// Returns [`RowBindingError::NoSelection`] or a length mismatch.
    pub fn map_selected(
        &self,
        rows: &[DataValue],
        cursor: &RowCursor,
    ) -> Result<BindingReport, RowBindingError> {
        if rows.len() != cursor.len() {
            return Err(RowBindingError::Selection(
                RowSelectionError::LengthMismatch {
                    cursor: cursor.len(),
                    rows: rows.len(),
                },
            ));
        }
        let row = cursor.current(rows).ok_or(RowBindingError::NoSelection)?;
        Ok(self.map_row(row))
    }

    /// `Ok(None)` means missing (fallback/required rules apply).
    fn resolve(
        &self,
        binding: &DataBinding,
        row: &DataValue,
    ) -> Result<Option<BoundValue>, BindingError> {
        let hidden_when_missing = matches!(
            &binding.kind,
            BindingKind::Visibility(VisibilityRule {
                rule: TruthyRule::NonEmpty,
                ..
            })
        );
        let missing = |kind: &BindingKind| match kind {
            BindingKind::Visibility(rule) if hidden_when_missing => {
                Ok(Some(BoundValue::Visible(rule.invert)))
            }
            _ => Ok(None),
        };
        let mut value = match binding.source.extract(row) {
            Ok(value) => value.clone(),
            Err(PathError::MissingField { .. } | PathError::MissingIndex { .. }) => {
                return missing(&binding.kind);
            }
            Err(error) => return Err(BindingError::Path(error)),
        };
        if !value.is_null() {
            for transform in &binding.transforms {
                value = transform
                    .apply(value, row)
                    .map_err(BindingError::Transform)?;
            }
        }
        if value.is_null() {
            return missing(&binding.kind);
        }
        if binding.kind.output() != BoundValueKind::Text
            && self.policy.empty_string_is_missing
            && matches!(&value, DataValue::String(text) if text.trim().is_empty())
        {
            return missing(&binding.kind);
        }
        convert(&value, &binding.kind, &self.policy).map(Some)
    }
}

fn convert(
    value: &DataValue,
    kind: &BindingKind,
    policy: &CoercionPolicy,
) -> Result<BoundValue, BindingError> {
    let title = |kind| {
        coerce_title_value(value, kind, policy)
            .map(BoundValue::from)
            .map_err(BindingError::Coercion)
    };
    let text = |text: String| {
        if text.len() > policy.max_text_bytes {
            return Err(BindingError::Coercion(CoercionError::TextTooLong {
                limit: policy.max_text_bytes,
                actual: text.len(),
            }));
        }
        Ok(BoundValue::Text(text))
    };
    match kind {
        BindingKind::Text => title(TitleFieldKind::Text),
        BindingKind::Number => title(TitleFieldKind::Number),
        BindingKind::Color => title(TitleFieldKind::Color),
        BindingKind::Image(image) => match value {
            DataValue::String(reference) => image
                .check(reference)
                .map(BoundValue::Image)
                .map_err(BindingError::Image),
            other => Err(BindingError::Image(ImageUriError::Unsupported {
                actual: other.value_type(),
            })),
        },
        BindingKind::Visibility(rule) => rule
            .evaluate(value)
            .map(BoundValue::Visible)
            .map_err(BindingError::Visibility),
        BindingKind::FormattedNumber(format) => {
            text(format.format_value(value).map_err(BindingError::Format)?)
        }
        BindingKind::FormattedDateTime(format) => {
            text(format.format_value(value).map_err(BindingError::Format)?)
        }
    }
}

fn validate_fallback(
    fallback: &BoundValue,
    kind: &BindingKind,
    policy: &CoercionPolicy,
) -> Result<(), BindingError> {
    match (fallback, kind) {
        (BoundValue::Text(text), _) if text.len() > policy.max_text_bytes => {
            Err(BindingError::Coercion(CoercionError::TextTooLong {
                limit: policy.max_text_bytes,
                actual: text.len(),
            }))
        }
        (BoundValue::Image(reference), BindingKind::Image(image)) => image
            .check(reference)
            .map(|_| ())
            .map_err(BindingError::Image),
        _ => Ok(()),
    }
}
