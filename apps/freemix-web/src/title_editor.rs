//! Browser title editor (`GX-006` slice).
//!
//! The editor core lives in [`fm_ui_model::title_editor`] and is keyed on
//! protocol types, so the browser and Studio share one model and one live
//! protocol bridge. This module adds the browser's session gating and the
//! conversions between `fm_titles` scenes and editor snapshots that
//! in-process authorities use.

use fm_client::{ConnectionState, Session};
use fm_protocol::{TitleFieldValue, WireTitleFieldId};
use fm_titles::{Color, FieldId, FieldValue, ImageSource, TitleScene, TitleTemplate};
use fm_types::InputId;
use fm_ui_model::title_editor::{ExposedField, FieldType, TitleAccess, TitleSnapshot};

/// Derives title access from the client connection and granted session.
/// Permissions survive a disconnect so typing is not lost, but nothing can
/// be sent until the connection is ready again.
#[must_use]
pub fn title_access(state: &ConnectionState, session: Option<&Session>) -> TitleAccess {
    let permissions = session.map_or(&[][..], |session| session.permissions.as_slice());
    TitleAccess::from_permissions(
        matches!(state, ConnectionState::Ready) && session.is_some(),
        permissions,
    )
}

/// Builds the snapshot of the title hosted as `title` from a live scene,
/// naming fields from its template in template order. Every template field
/// is exposed.
#[must_use]
pub fn scene_snapshot(
    title: InputId,
    name: impl Into<String>,
    template: &TitleTemplate,
    scene: &TitleScene,
    on_air: bool,
) -> TitleSnapshot {
    let fields = template
        .fields
        .iter()
        .filter_map(|definition| {
            scene.field_value(definition.id).map(|value| ExposedField {
                id: wire_field_id(definition.id),
                name: definition.name.clone(),
                value: wire_field_value(value),
                revision: scene.field_set().field_revision(definition.id).unwrap_or(0),
            })
        })
        .collect();
    TitleSnapshot {
        title,
        name: name.into(),
        revision: scene.revision(),
        on_air,
        fields,
    }
}

#[must_use]
pub const fn wire_field_id(field: FieldId) -> WireTitleFieldId {
    WireTitleFieldId::new(field.get())
}

#[must_use]
pub const fn domain_field_id(field: WireTitleFieldId) -> FieldId {
    FieldId::new(field.get())
}

#[must_use]
pub const fn wire_field_type(field_type: fm_titles::FieldType) -> FieldType {
    match field_type {
        fm_titles::FieldType::Text => FieldType::Text,
        fm_titles::FieldType::Number => FieldType::Number,
        fm_titles::FieldType::Color => FieldType::Color,
        fm_titles::FieldType::Image => FieldType::Image,
    }
}

#[must_use]
pub fn wire_field_value(value: &FieldValue) -> TitleFieldValue {
    match value {
        FieldValue::Text(text) => TitleFieldValue::Text(text.clone()),
        FieldValue::Number(number) => TitleFieldValue::Number(*number),
        FieldValue::Color(color) => TitleFieldValue::Color {
            red: color.r,
            green: color.g,
            blue: color.b,
            alpha: color.a,
        },
        FieldValue::Image(image) => TitleFieldValue::Image {
            asset_uri: image.asset.clone(),
        },
    }
}

#[must_use]
pub fn domain_field_value(value: &TitleFieldValue) -> FieldValue {
    match value {
        TitleFieldValue::Text(text) => FieldValue::Text(text.clone()),
        TitleFieldValue::Number(number) => FieldValue::Number(*number),
        TitleFieldValue::Color {
            red,
            green,
            blue,
            alpha,
        } => FieldValue::Color(Color::new(*red, *green, *blue, *alpha)),
        TitleFieldValue::Image { asset_uri } => FieldValue::Image(ImageSource::new(asset_uri)),
    }
}
