//! Typed mapping from data rows onto title template field values.
//!
//! The kinds mirror the title template field contract (text, number, color,
//! image reference) without depending on the titles crate; a composition
//! layer converts [`TitleFieldValue`] into the renderer's own field type.

use std::{collections::BTreeMap, fmt, str::FromStr};

use crate::{DataPath, DataValue, Decimal, PathError, Transform, TransformError, ValueType};

/// The declared type of a title template field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleFieldKind {
    Text,
    Number,
    Color,
    Image,
}

/// A straight-alpha RGBA8 color.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TitleColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// A coerced, title-ready field value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleFieldValue {
    Text(String),
    Number(i64),
    Color(TitleColor),
    /// An asset reference resolved by the titles/media layer.
    Image(String),
}

impl TitleFieldValue {
    #[must_use]
    pub const fn kind(&self) -> TitleFieldKind {
        match self {
            Self::Text(_) => TitleFieldKind::Text,
            Self::Number(_) => TitleFieldKind::Number,
            Self::Color(_) => TitleFieldKind::Color,
            Self::Image(_) => TitleFieldKind::Image,
        }
    }
}

/// How a fractional decimal is converted into an integer `Number` field.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NumberRounding {
    /// Fractional values are a coercion error.
    #[default]
    Reject,
    /// Discards the fraction (rounds toward zero).
    Truncate,
    /// Rounds to the nearest integer, ties away from zero.
    HalfAwayFromZero,
}

/// Explicit coercion rules. Nothing is coerced unless listed here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoercionPolicy {
    pub rounding: NumberRounding,
    /// Treat empty or all-whitespace strings as missing for `Number`,
    /// `Color`, and `Image` fields. `Text` fields always keep empty strings.
    pub empty_string_is_missing: bool,
    /// Maximum UTF-8 bytes of a `Text` or `Image` value; longer values fail.
    pub max_text_bytes: usize,
}

impl CoercionPolicy {
    pub const DEFAULT_MAX_TEXT_BYTES: usize = 4096;
}

impl Default for CoercionPolicy {
    fn default() -> Self {
        Self {
            rounding: NumberRounding::Reject,
            empty_string_is_missing: true,
            max_text_bytes: Self::DEFAULT_MAX_TEXT_BYTES,
        }
    }
}

/// A value could not be coerced into the requested title field kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoercionError {
    Unsupported {
        kind: TitleFieldKind,
        actual: ValueType,
    },
    InvalidNumber(String),
    FractionalNumber(Decimal),
    NumberOutOfRange,
    InvalidColor(String),
    EmptyImageReference,
    ControlCharacter,
    TextTooLong {
        limit: usize,
        actual: usize,
    },
}

impl fmt::Display for CoercionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { kind, actual } => {
                write!(formatter, "cannot coerce {actual:?} into a {kind:?} field")
            }
            Self::InvalidNumber(text) => write!(formatter, "`{text}` is not a number"),
            Self::FractionalNumber(value) => {
                write!(formatter, "{value} has a fraction and rounding is disabled")
            }
            Self::NumberOutOfRange => formatter.write_str("number is out of range"),
            Self::InvalidColor(text) => {
                write!(formatter, "`{text}` is not #RGB, #RRGGBB, or #RRGGBBAA")
            }
            Self::EmptyImageReference => formatter.write_str("image reference is empty"),
            Self::ControlCharacter => {
                formatter.write_str("image reference contains a control character")
            }
            Self::TextTooLong { limit, actual } => {
                write!(formatter, "text is {actual} bytes; limit is {limit}")
            }
        }
    }
}

impl std::error::Error for CoercionError {}

/// Coerces one present value into a title field value.
///
/// `Null` is never coerced; callers decide whether a missing value is a
/// fallback, an omission, or an error. Rules:
/// - `Text`: strings as-is; booleans, integers, and decimals by canonical text.
/// - `Number`: integers as-is; decimals and numeric strings (optional sign,
///   surrounding ASCII whitespace) via [`NumberRounding`].
/// - `Color`: `#RGB`, `#RRGGBB`, or `#RRGGBBAA` hexadecimal strings.
/// - `Image`: non-empty strings without control characters (trimmed).
///
/// # Errors
///
/// Returns a [`CoercionError`] for any unlisted conversion or malformed input.
pub fn coerce_title_value(
    value: &DataValue,
    kind: TitleFieldKind,
    policy: &CoercionPolicy,
) -> Result<TitleFieldValue, CoercionError> {
    let unsupported = || CoercionError::Unsupported {
        kind,
        actual: value.value_type(),
    };
    match kind {
        TitleFieldKind::Text => {
            let text = match value {
                DataValue::String(text) => text.clone(),
                DataValue::Bool(_) | DataValue::Integer(_) | DataValue::Decimal(_) => {
                    value.scalar_text().ok_or_else(unsupported)?
                }
                DataValue::Null | DataValue::List(_) | DataValue::Object(_) => {
                    return Err(unsupported());
                }
            };
            check_length(&text, policy)?;
            Ok(TitleFieldValue::Text(text))
        }
        TitleFieldKind::Number => match value {
            DataValue::Integer(number) => Ok(TitleFieldValue::Number(*number)),
            DataValue::Decimal(decimal) => {
                round_decimal(*decimal, policy.rounding).map(TitleFieldValue::Number)
            }
            DataValue::String(text) => {
                let trimmed = text.trim_ascii();
                let invalid = || CoercionError::InvalidNumber(text.clone());
                let unsigned = match trimmed.strip_prefix('+') {
                    Some(rest) if rest.starts_with('-') => return Err(invalid()),
                    Some(rest) => rest,
                    None => trimmed,
                };
                let decimal = Decimal::from_str(unsigned).map_err(|_| invalid())?;
                round_decimal(decimal, policy.rounding).map(TitleFieldValue::Number)
            }
            _ => Err(unsupported()),
        },
        TitleFieldKind::Color => match value {
            DataValue::String(text) => parse_color(text.trim_ascii())
                .map(TitleFieldValue::Color)
                .ok_or_else(|| CoercionError::InvalidColor(text.clone())),
            _ => Err(unsupported()),
        },
        TitleFieldKind::Image => match value {
            DataValue::String(text) => {
                let reference = text.trim();
                if reference.is_empty() {
                    return Err(CoercionError::EmptyImageReference);
                }
                if reference.chars().any(char::is_control) {
                    return Err(CoercionError::ControlCharacter);
                }
                check_length(reference, policy)?;
                Ok(TitleFieldValue::Image(reference.to_owned()))
            }
            _ => Err(unsupported()),
        },
    }
}

fn check_length(text: &str, policy: &CoercionPolicy) -> Result<(), CoercionError> {
    if text.len() > policy.max_text_bytes {
        return Err(CoercionError::TextTooLong {
            limit: policy.max_text_bytes,
            actual: text.len(),
        });
    }
    Ok(())
}

fn round_decimal(decimal: Decimal, rounding: NumberRounding) -> Result<i64, CoercionError> {
    if decimal.scale() == 0 {
        return Ok(decimal.coefficient());
    }
    let divisor = 10_i128.pow(decimal.scale());
    let coefficient = i128::from(decimal.coefficient());
    let truncated = coefficient / divisor;
    let remainder = coefficient % divisor;
    let rounded = match rounding {
        NumberRounding::Reject => return Err(CoercionError::FractionalNumber(decimal)),
        NumberRounding::Truncate => truncated,
        NumberRounding::HalfAwayFromZero => {
            if remainder.abs() * 2 >= divisor {
                truncated + coefficient.signum()
            } else {
                truncated
            }
        }
    };
    i64::try_from(rounded).map_err(|_| CoercionError::NumberOutOfRange)
}

fn parse_color(text: &str) -> Option<TitleColor> {
    let hex = text.strip_prefix('#')?;
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |index: usize, width: usize| -> Option<u8> {
        let digits = hex.get(index * width..(index + 1) * width)?;
        let value = u8::from_str_radix(digits, 16).ok()?;
        Some(if width == 1 { value * 17 } else { value })
    };
    match hex.len() {
        3 => Some(TitleColor {
            r: channel(0, 1)?,
            g: channel(1, 1)?,
            b: channel(2, 1)?,
            a: 255,
        }),
        6 | 8 => Some(TitleColor {
            r: channel(0, 2)?,
            g: channel(1, 2)?,
            b: channel(2, 2)?,
            a: if hex.len() == 8 { channel(3, 2)? } else { 255 },
        }),
        _ => None,
    }
}

/// Binds one data path onto one title template field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleFieldBinding {
    pub field: String,
    pub source: DataPath,
    pub kind: TitleFieldKind,
    /// Applied in order to a present value before coercion.
    pub transforms: Vec<Transform>,
    /// When `false`, a missing value without fallback is omitted, not an error.
    pub required: bool,
    /// Used when the source value is missing, `Null`, or an empty string
    /// treated as missing. Its kind must equal `kind`.
    pub fallback: Option<TitleFieldValue>,
}

impl TitleFieldBinding {
    #[must_use]
    pub fn new(field: impl Into<String>, source: DataPath, kind: TitleFieldKind) -> Self {
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
    pub fn with_fallback(mut self, fallback: TitleFieldValue) -> Self {
        self.fallback = Some(fallback);
        self
    }

    #[must_use]
    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transforms.push(transform);
        self
    }
}

/// A title mapper configuration is invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleMapperError {
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
    FallbackKindMismatch {
        field: String,
        expected: TitleFieldKind,
        actual: TitleFieldKind,
    },
    InvalidFallback {
        field: String,
        error: CoercionError,
    },
}

impl fmt::Display for TitleMapperError {
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

impl std::error::Error for TitleMapperError {}

/// The outcome for one title field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleFieldStatus {
    Mapped,
    /// The source was missing and the binding's fallback was used.
    Fallback,
    /// The source was missing on an optional binding without fallback.
    Omitted,
    /// The source was missing on a required binding without fallback.
    Missing,
    Path(PathError),
    Transform(TransformError),
    Coercion(CoercionError),
}

impl TitleFieldStatus {
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(
            self,
            Self::Missing | Self::Path(_) | Self::Transform(_) | Self::Coercion(_)
        )
    }
}

/// Report entry for one binding, in declaration order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleFieldReport {
    pub field: String,
    pub status: TitleFieldStatus,
}

/// Coerced title values plus complete per-field diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleMappingReport {
    pub values: BTreeMap<String, TitleFieldValue>,
    pub fields: Vec<TitleFieldReport>,
}

impl TitleMappingReport {
    /// `true` when no binding produced an error status.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.fields.iter().all(|field| !field.status.is_error())
    }

    pub fn errors(&self) -> impl Iterator<Item = &TitleFieldReport> {
        self.fields.iter().filter(|field| field.status.is_error())
    }
}

/// Validated, bounded set of title field bindings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleMapper {
    bindings: Vec<TitleFieldBinding>,
    policy: CoercionPolicy,
}

impl TitleMapper {
    pub const MAX_BINDINGS: usize = 256;
    pub const MAX_TRANSFORMS_PER_BINDING: usize = 16;

    /// Validates bindings: bounded count, unique non-empty field names, and
    /// fallbacks whose kind matches and which satisfy the policy.
    ///
    /// # Errors
    ///
    /// Returns the first [`TitleMapperError`] in declaration order.
    pub fn new(
        bindings: Vec<TitleFieldBinding>,
        policy: CoercionPolicy,
    ) -> Result<Self, TitleMapperError> {
        if bindings.len() > Self::MAX_BINDINGS {
            return Err(TitleMapperError::TooManyBindings {
                limit: Self::MAX_BINDINGS,
                actual: bindings.len(),
            });
        }
        let mut seen = std::collections::BTreeSet::new();
        for binding in &bindings {
            if binding.field.is_empty() {
                return Err(TitleMapperError::EmptyFieldName);
            }
            if !seen.insert(binding.field.as_str()) {
                return Err(TitleMapperError::DuplicateField(binding.field.clone()));
            }
            if binding.transforms.len() > Self::MAX_TRANSFORMS_PER_BINDING {
                return Err(TitleMapperError::TooManyTransforms {
                    field: binding.field.clone(),
                    limit: Self::MAX_TRANSFORMS_PER_BINDING,
                });
            }
            if let Some(fallback) = &binding.fallback {
                if fallback.kind() != binding.kind {
                    return Err(TitleMapperError::FallbackKindMismatch {
                        field: binding.field.clone(),
                        expected: binding.kind,
                        actual: fallback.kind(),
                    });
                }
                validate_fallback(fallback, &policy).map_err(|error| {
                    TitleMapperError::InvalidFallback {
                        field: binding.field.clone(),
                        error,
                    }
                })?;
            }
        }
        Ok(Self { bindings, policy })
    }

    #[must_use]
    pub fn bindings(&self) -> &[TitleFieldBinding] {
        &self.bindings
    }

    #[must_use]
    pub const fn policy(&self) -> &CoercionPolicy {
        &self.policy
    }

    /// Maps one data row (any value addressed by the binding paths).
    #[must_use]
    pub fn map_row(&self, row: &DataValue) -> TitleMappingReport {
        let mut values = BTreeMap::new();
        let mut fields = Vec::with_capacity(self.bindings.len());
        for binding in &self.bindings {
            let status = match self.map_binding(binding, row) {
                Ok(Resolved::Value(value)) => {
                    values.insert(binding.field.clone(), value);
                    TitleFieldStatus::Mapped
                }
                Ok(Resolved::Missing) => match &binding.fallback {
                    Some(fallback) => {
                        values.insert(binding.field.clone(), fallback.clone());
                        TitleFieldStatus::Fallback
                    }
                    None if binding.required => TitleFieldStatus::Missing,
                    None => TitleFieldStatus::Omitted,
                },
                Err(status) => status,
            };
            fields.push(TitleFieldReport {
                field: binding.field.clone(),
                status,
            });
        }
        TitleMappingReport { values, fields }
    }

    fn map_binding(
        &self,
        binding: &TitleFieldBinding,
        row: &DataValue,
    ) -> Result<Resolved, TitleFieldStatus> {
        let mut value = match binding.source.extract(row) {
            Ok(value) => value.clone(),
            Err(PathError::MissingField { .. } | PathError::MissingIndex { .. }) => {
                return Ok(Resolved::Missing);
            }
            Err(error) => return Err(TitleFieldStatus::Path(error)),
        };
        if value.is_null() {
            return Ok(Resolved::Missing);
        }
        for transform in &binding.transforms {
            value = transform
                .apply(value, row)
                .map_err(TitleFieldStatus::Transform)?;
        }
        if value.is_null() {
            return Ok(Resolved::Missing);
        }
        if binding.kind != TitleFieldKind::Text
            && self.policy.empty_string_is_missing
            && matches!(&value, DataValue::String(text) if text.trim().is_empty())
        {
            return Ok(Resolved::Missing);
        }
        coerce_title_value(&value, binding.kind, &self.policy)
            .map(Resolved::Value)
            .map_err(TitleFieldStatus::Coercion)
    }
}

enum Resolved {
    Value(TitleFieldValue),
    Missing,
}

fn validate_fallback(
    fallback: &TitleFieldValue,
    policy: &CoercionPolicy,
) -> Result<(), CoercionError> {
    match fallback {
        TitleFieldValue::Text(text) => check_length(text, policy),
        TitleFieldValue::Image(reference) => coerce_title_value(
            &DataValue::String(reference.clone()),
            TitleFieldKind::Image,
            policy,
        )
        .map(|_| ()),
        TitleFieldValue::Number(_) | TitleFieldValue::Color(_) => Ok(()),
    }
}
