//! GX-002: title-designer documents persist strictly and round-trip exactly.

use std::{collections::BTreeMap, fs, num::NonZeroU128};

use fm_persistence::{
    CURRENT_SCHEMA_VERSION, MAX_TITLE_DESIGN_BYTES, TitleDesignError, decode_title_design,
    encode_title_design, read_title_design_file, write_title_design_file,
};
use fm_titles::{
    Bounds, ClipMask, Color, DesignError, DesignNode, Element, ElementId, ElementKind,
    FieldDefinition, FieldId, FieldValue, Group, GroupTransform, Style, TemplateId, TitleAnimation,
    TitleDesign, default_title_timebase,
};

fn nz(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn design() -> TitleDesign {
    let group = Group {
        id: ElementId::new(nz(2)),
        name: "Group".into(),
        visible: true,
        transform: GroupTransform {
            offset_x: 10,
            offset_y: -5,
            opacity: 200,
        },
        mask: Some(ClipMask::rounded(
            Bounds {
                x: 0,
                y: 0,
                width: 320,
                height: 180,
            },
            12,
        )),
    };
    let element = Element {
        id: ElementId::new(nz(3)),
        name: "Box".into(),
        bounds: Bounds {
            x: 20,
            y: 30,
            width: 100,
            height: 50,
        },
        z_index: 0,
        visible: true,
        style: Style::default(),
        color_field: None,
        kind: ElementKind::Rectangle,
    };
    let mut nodes = BTreeMap::new();
    nodes.insert(group.id, DesignNode::Group(group));
    nodes.insert(element.id, DesignNode::Element(element));
    let mut children = BTreeMap::new();
    children.insert(ElementId::new(nz(2)), vec![ElementId::new(nz(3))]);
    TitleDesign::from_parts(
        TemplateId::new(nz(1)),
        "Design".into(),
        640,
        360,
        Color::TRANSPARENT,
        vec![FieldDefinition {
            id: FieldId::new(nz(1)),
            name: "title".into(),
            default: FieldValue::Text("x".into()),
            exposed: true,
        }],
        nodes,
        vec![ElementId::new(nz(2))],
        children,
        TitleAnimation::empty(default_title_timebase()).clip,
    )
    .unwrap()
}

#[test]
fn designs_round_trip_exactly_through_a_strict_document() {
    let design = design();
    let document = encode_title_design(&design).unwrap();
    assert!(document.contains("\"contract\": \"freemix.title-design\""));
    assert!(document.contains("\"mask\""));
    assert_eq!(decode_title_design(&document).unwrap(), design);
}

#[test]
fn design_files_round_trip_and_respect_the_byte_cap() {
    let dir = std::env::temp_dir().join(format!("fm-design-{}-{}", std::process::id(), line!()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("show.fmdesign");
    write_title_design_file(&path, &design()).unwrap();
    assert_eq!(read_title_design_file(&path).unwrap(), design());
    fs::write(
        &path,
        "x".repeat(usize::try_from(MAX_TITLE_DESIGN_BYTES).unwrap() + 1),
    )
    .unwrap();
    assert!(matches!(
        read_title_design_file(&path),
        Err(TitleDesignError::TooLarge { .. })
    ));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn foreign_and_malformed_documents_are_refused() {
    let document = encode_title_design(&design()).unwrap();
    let wrong_contract = document.replace("freemix.title-design", "other");
    assert!(matches!(
        decode_title_design(&wrong_contract),
        Err(TitleDesignError::Contract { .. })
    ));
    let wrong_schema = document.replace(
        &format!("\"schema_version\": {CURRENT_SCHEMA_VERSION}"),
        "\"schema_version\": 1",
    );
    assert!(matches!(
        decode_title_design(&wrong_schema),
        Err(TitleDesignError::Schema { .. })
    ));
    let duplicate = document.replace(
        "\"type\": \"element\", \"element\": {\"id\": 3",
        "\"type\": \"element\", \"element\": {\"id\": 2",
    );
    assert!(matches!(
        decode_title_design(&duplicate),
        Err(TitleDesignError::Design(DesignError::DuplicateNode(_)))
    ));
    let unknown = document.replace("\"type\": \"element\"", "\"type\": \"shape\"");
    assert!(matches!(
        decode_title_design(&unknown),
        Err(TitleDesignError::Decode(_))
    ));
    let orphan = document.replace(
        "{\"group\": 2, \"children\": [3]}",
        "{\"group\": 2, \"children\": []}",
    );
    assert!(matches!(
        decode_title_design(&orphan),
        Err(TitleDesignError::Design(DesignError::OrphanNode(_)))
    ));
}
