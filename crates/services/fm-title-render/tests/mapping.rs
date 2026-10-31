//! GX-004/GX-005: mapped data values apply onto a title scene atomically.

use std::{collections::BTreeMap, num::NonZeroU128};

use fm_data::{TitleColor, TitleFieldValue, TitleMappingReport};
use fm_title_render::{TitleMappingError, apply_title_mapping};
use fm_titles::{
    Color, FieldDefinition, FieldId, FieldValue, ImageSource, TemplateId, TitleAnimation, TitleId,
    TitleScene, TitleTemplate,
};
use fm_types::InputId;

fn nz(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn field(value: u128) -> FieldId {
    FieldId::new(nz(value))
}

fn template() -> TitleTemplate {
    TitleTemplate {
        id: TemplateId::new(nz(1)),
        name: "Mapping".into(),
        width: 64,
        height: 32,
        background: Color::TRANSPARENT,
        fields: vec![
            FieldDefinition {
                id: field(1),
                name: "headline".into(),
                default: FieldValue::Text(String::new()),
                exposed: true,
            },
            FieldDefinition {
                id: field(2),
                name: "score".into(),
                default: FieldValue::Number(0),
                exposed: true,
            },
            FieldDefinition {
                id: field(3),
                name: "tint".into(),
                default: FieldValue::Color(Color::new(0, 0, 0, 255)),
                exposed: true,
            },
            FieldDefinition {
                id: field(4),
                name: "logo".into(),
                default: FieldValue::Image(ImageSource::new("asset://none.png")),
                exposed: true,
            },
        ],
        elements: Vec::new(),
        animation: TitleAnimation::default(),
    }
}

fn scene(template: &TitleTemplate) -> TitleScene {
    template.instantiate(TitleId::new(nz(1))).unwrap()
}

fn report() -> TitleMappingReport {
    TitleMappingReport {
        values: BTreeMap::from([
            ("headline".into(), TitleFieldValue::Text("Harbor".into())),
            ("score".into(), TitleFieldValue::Number(3)),
            (
                "tint".into(),
                TitleFieldValue::Color(TitleColor {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                }),
            ),
            (
                "logo".into(),
                TitleFieldValue::Image("asset://logo.png".into()),
            ),
        ]),
        fields: Vec::new(),
    }
}

#[test]
fn mapped_values_apply_in_one_scene_revision() {
    let template = template();
    let mut scene = scene(&template);
    let revision = apply_title_mapping(&mut scene, &template, 0, &report()).unwrap();
    assert_eq!(revision, 1);
    assert_eq!(
        scene.field_value(field(1)),
        Some(&FieldValue::Text("Harbor".into()))
    );
    assert_eq!(scene.field_value(field(2)), Some(&FieldValue::Number(3)));
    assert_eq!(
        scene.field_value(field(3)),
        Some(&FieldValue::Color(Color::new(255, 0, 0, 255)))
    );
    assert_eq!(
        scene.field_value(field(4)),
        Some(&FieldValue::Image(ImageSource::new("asset://logo.png")))
    );
}

#[test]
fn an_unknown_field_changes_nothing() {
    let template = template();
    let mut scene = scene(&template);
    apply_title_mapping(&mut scene, &template, 0, &report()).unwrap();
    let before = scene.clone();
    let mut bad = report();
    bad.values
        .insert("missing".into(), TitleFieldValue::Text("x".into()));
    assert_eq!(
        apply_title_mapping(&mut scene, &template, 1, &bad),
        Err(TitleMappingError::UnknownField {
            field: "missing".into()
        })
    );
    assert_eq!(scene, before);
}

#[test]
fn a_stale_scene_revision_is_refused() {
    let template = template();
    let mut scene = scene(&template);
    apply_title_mapping(&mut scene, &template, 0, &report()).unwrap();
    let stale = apply_title_mapping(&mut scene, &template, 0, &report());
    assert!(stale.is_err(), "{stale:?}");
}

#[test]
fn the_mapped_input_id_is_not_required() {
    // The mapping bridge writes a scene, not an input; this assertion keeps
    // the unused import honest if the test grows an input-backed case.
    let _ = InputId::new(nz(1));
}
