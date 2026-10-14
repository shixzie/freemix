use fm_titles::{
    Color, FieldDefinition, FieldId, FieldSetError, FieldType, FieldValue, ImageSource,
    MAX_TITLE_CONTENT_BYTES, MAX_TITLE_FIELDS, MAX_TITLE_TEXT_CHARS, TemplateId, TitleFieldSet,
    TitleId, TitleTemplate, UpdateError,
};
use std::num::NonZeroU128;

fn field_id(value: u128) -> FieldId {
    FieldId::new(NonZeroU128::new(value).unwrap())
}

fn definition(id: u128, default: FieldValue) -> FieldDefinition {
    FieldDefinition {
        id: field_id(id),
        name: format!("field-{id}"),
        default,
    }
}

fn template(fields: Vec<FieldDefinition>) -> TitleTemplate {
    TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(1).unwrap()),
        name: "lower third".into(),
        width: 64,
        height: 32,
        background: Color::TRANSPARENT,
        fields,
        elements: Vec::new(),
        animation: fm_titles::TitleAnimation::default(),
    }
}

fn three_fields() -> TitleTemplate {
    template(vec![
        definition(1, FieldValue::Text("Ada".into())),
        definition(2, FieldValue::Color(Color::BLACK)),
        definition(3, FieldValue::Image(ImageSource::new("asset://logo.png"))),
    ])
}

#[test]
fn from_template_holds_defaults_at_revision_zero() {
    let set = TitleFieldSet::from_template(&three_fields()).unwrap();
    assert_eq!(set.revision(), 0);
    assert_eq!(set.definitions().len(), 3);
    assert_eq!(set.field_type(field_id(2)), Some(FieldType::Color));
    assert_eq!(
        set.value(field_id(1)),
        Some(&FieldValue::Text("Ada".into()))
    );
    assert_eq!(set.content_bytes(), 3 + "asset://logo.png".len());
}

#[test]
fn restore_requires_exact_typed_coverage() {
    let template = three_fields();
    let values = || {
        vec![
            (field_id(1), FieldValue::Text("Grace".into())),
            (field_id(2), FieldValue::Color(Color::new(1, 2, 3, 4))),
            (
                field_id(3),
                FieldValue::Image(ImageSource::new("asset://x")),
            ),
        ]
    };
    let restored = TitleFieldSet::restore(&template, 7, values()).unwrap();
    assert_eq!(restored.revision(), 7);
    assert_eq!(
        restored.value(field_id(1)),
        Some(&FieldValue::Text("Grace".into()))
    );

    let mut missing = values();
    missing.pop();
    assert_eq!(
        TitleFieldSet::restore(&template, 7, missing),
        Err(FieldSetError::MissingField(field_id(3)))
    );
    let mut extra = values();
    extra.push((field_id(9), FieldValue::Number(1)));
    assert_eq!(
        TitleFieldSet::restore(&template, 7, extra),
        Err(FieldSetError::UnknownField(field_id(9)))
    );
    let mut repeated = values();
    repeated.push((field_id(1), FieldValue::Text("again".into())));
    assert_eq!(
        TitleFieldSet::restore(&template, 7, repeated),
        Err(FieldSetError::DuplicateField(field_id(1)))
    );
    let mut mistyped = values();
    mistyped[1].1 = FieldValue::Number(3);
    assert_eq!(
        TitleFieldSet::restore(&template, 7, mistyped),
        Err(FieldSetError::TypeMismatch {
            field: field_id(2),
            expected: FieldType::Color,
            actual: FieldType::Number,
        })
    );
    let mut blank = values();
    blank[2].1 = FieldValue::Image(ImageSource::new(" "));
    assert_eq!(
        TitleFieldSet::restore(&template, 7, blank),
        Err(FieldSetError::EmptyImageAsset(field_id(3)))
    );
}

#[test]
fn template_bounds_are_enforced() {
    let too_many = template(
        (1..=u128::try_from(MAX_TITLE_FIELDS).unwrap() + 1)
            .map(|id| definition(id, FieldValue::Number(0)))
            .collect(),
    );
    assert_eq!(
        TitleFieldSet::from_template(&too_many),
        Err(FieldSetError::TooManyFields {
            count: MAX_TITLE_FIELDS + 1
        })
    );
    let duplicate = template(vec![
        definition(1, FieldValue::Number(0)),
        definition(1, FieldValue::Number(1)),
    ]);
    assert_eq!(
        TitleFieldSet::from_template(&duplicate),
        Err(FieldSetError::DuplicateDefinition(field_id(1)))
    );
    let long = template(vec![definition(
        1,
        FieldValue::Text("x".repeat(MAX_TITLE_TEXT_CHARS + 1)),
    )]);
    assert_eq!(
        TitleFieldSet::from_template(&long),
        Err(FieldSetError::TextTooLong {
            field: field_id(1),
            chars: MAX_TITLE_TEXT_CHARS + 1
        })
    );
    let large = template(
        (1..=4)
            .map(|id| definition(id, FieldValue::Text("x".repeat(MAX_TITLE_TEXT_CHARS))))
            .collect(),
    );
    assert_eq!(
        TitleFieldSet::from_template(&large),
        Err(FieldSetError::ContentTooLarge {
            bytes: 4 * MAX_TITLE_TEXT_CHARS
        })
    );
}

#[test]
fn update_enforces_text_and_content_bounds_atomically() {
    let template = template(
        (1..=4)
            .map(|id| definition(id, FieldValue::Text(String::new())))
            .collect(),
    );
    let mut set = TitleFieldSet::from_template(&template).unwrap();
    // Multi-byte characters count as one character each but several bytes.
    let at_char_limit = "é".repeat(MAX_TITLE_TEXT_CHARS);
    assert_eq!(
        set.update(0, &[(field_id(1), FieldValue::Text(at_char_limit.clone()))]),
        Ok(1)
    );
    assert_eq!(
        set.update(
            1,
            &[(
                field_id(2),
                FieldValue::Text("x".repeat(MAX_TITLE_TEXT_CHARS + 1))
            )]
        ),
        Err(UpdateError::TextTooLong {
            field: field_id(2),
            chars: MAX_TITLE_TEXT_CHARS + 1
        })
    );
    // 8192 bytes already held; 4097 more would cross the content bound.
    let filler = "x".repeat(MAX_TITLE_CONTENT_BYTES - at_char_limit.len());
    assert_eq!(
        set.update(
            1,
            &[
                (field_id(2), FieldValue::Text(filler.clone())),
                (field_id(3), FieldValue::Text("y".into())),
            ]
        ),
        Err(UpdateError::ContentTooLarge {
            bytes: MAX_TITLE_CONTENT_BYTES + 1
        })
    );
    assert_eq!(set.revision(), 1);
    assert_eq!(
        set.value(field_id(2)),
        Some(&FieldValue::Text(String::new()))
    );
    // Exactly at the bound is accepted, and replacing a value frees its bytes.
    assert_eq!(
        set.update(1, &[(field_id(2), FieldValue::Text(filler))]),
        Ok(2)
    );
    assert_eq!(set.content_bytes(), MAX_TITLE_CONTENT_BYTES);
    assert_eq!(
        set.update(
            2,
            &[
                (field_id(1), FieldValue::Text(String::new())),
                (field_id(3), FieldValue::Text("z".repeat(100))),
            ]
        ),
        Ok(3)
    );
}

#[test]
fn scenes_delegate_to_the_field_set() {
    let template = three_fields();
    let mut scene = template
        .instantiate(TitleId::new(NonZeroU128::new(5).unwrap()))
        .unwrap();
    assert_eq!(
        scene.field_set(),
        &TitleFieldSet::from_template(&template).unwrap()
    );
    assert_eq!(
        scene.update_fields(0, &[(field_id(1), FieldValue::Text("Linus".into()))]),
        Ok(1)
    );
    assert_eq!(scene.field_set().revision(), 1);
    assert_eq!(scene.fields(), scene.field_set().values());
}
