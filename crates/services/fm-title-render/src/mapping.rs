//! Applies `fm-data` mapped values onto a title scene (GX-004, GX-005).
//!
//! Mapping stays outside `fm-titles`: [`apply_title_mapping`] resolves each
//! report field name against a template and writes every changed value in one
//! revision-checked scene update, so a title never shows a half-applied row.

use core::fmt;

use fm_data::{TitleFieldValue, TitleMappingReport};
use fm_titles::{
    Color, FieldValue, ImageSource, TitleFieldSet, TitleScene, TitleTemplate, UpdateError,
};

/// Why a mapped row could not be applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleMappingError {
    /// The report names a field the template does not define.
    UnknownField { field: String },
    /// An image value is not a project `asset://` reference.
    InvalidAsset { field: String, asset: String },
    /// The scene refused the update (a stale revision, a duplicate field, or
    /// a value of the wrong type).
    Update(UpdateError),
}

impl fmt::Display for TitleMappingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownField { field } => {
                write!(formatter, "the title template has no field named {field:?}")
            }
            Self::InvalidAsset { field, asset } => write!(
                formatter,
                "title field {field:?} names {asset:?}, which is not a project asset URI"
            ),
            Self::Update(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TitleMappingError {}

impl From<UpdateError> for TitleMappingError {
    fn from(error: UpdateError) -> Self {
        Self::Update(error)
    }
}

/// Writes every value in `report` onto the matching field of `scene`.
///
/// # Errors
///
/// Returns [`TitleMappingError::UnknownField`] for a report field the
/// template does not define, or the scene's [`UpdateError`] without changing
/// any field.
pub fn apply_title_mapping(
    scene: &mut TitleScene,
    template: &TitleTemplate,
    expected_revision: u64,
    report: &TitleMappingReport,
) -> Result<u64, TitleMappingError> {
    let updates = mapping_updates(template, report)?;
    scene
        .update_fields(expected_revision, &updates)
        .map_err(Into::into)
}

/// Writes every value in `report` onto the matching field of a standalone
/// field set.
///
/// This is [`apply_title_mapping`] without a scene: the project keeps a
/// title's typed values and per-field revisions in a [`TitleFieldSet`] (see
/// `freemix-cli title-apply-row`), so mapped rows reach the project schema
/// through the same field resolution and validation.
///
/// # Errors
///
/// Returns the same errors as [`apply_title_mapping`].
pub fn apply_title_mapping_to_fields(
    fields: &mut TitleFieldSet,
    template: &TitleTemplate,
    expected_revision: u64,
    report: &TitleMappingReport,
) -> Result<u64, TitleMappingError> {
    let updates = mapping_updates(template, report)?;
    fields.update(expected_revision, &updates).map_err(Into::into)
}

fn mapping_updates(
    template: &TitleTemplate,
    report: &TitleMappingReport,
) -> Result<Vec<(fm_titles::FieldId, FieldValue)>, TitleMappingError> {
    let mut updates = Vec::with_capacity(report.values.len());
    for (name, value) in &report.values {
        let field = template
            .fields
            .iter()
            .find(|field| field.name == *name)
            .ok_or_else(|| TitleMappingError::UnknownField {
                field: name.clone(),
            })?;
        if let TitleFieldValue::Image(asset) = value {
            fm_persistence::validate_asset_uri(asset).map_err(|_| {
                TitleMappingError::InvalidAsset {
                    field: name.clone(),
                    asset: asset.clone(),
                }
            })?;
        }
        updates.push((field.id, field_value(value)));
    }
    Ok(updates)
}

fn field_value(value: &TitleFieldValue) -> FieldValue {
    match value {
        TitleFieldValue::Text(text) => FieldValue::Text(text.clone()),
        TitleFieldValue::Number(number) => FieldValue::Number(*number),
        TitleFieldValue::Color(color) => {
            FieldValue::Color(Color::new(color.r, color.g, color.b, color.a))
        }
        TitleFieldValue::Image(asset) => FieldValue::Image(ImageSource::new(asset.clone())),
    }
}
