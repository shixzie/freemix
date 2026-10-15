//! SW-006 layer designer through the public `fm_ui_model::scene_designer`
//! API: one operator session on a replicated scene layout, from direct
//! manipulation with safe-area snapping through numeric fields, crop, mask,
//! stacking, and undo, to the `freemix-cli scene-layer-*` commands that
//! apply the draft.

use core::num::{NonZeroU32, NonZeroU128};

use fm_protocol::{
    WireInputId, WireSceneId, WireSceneLayerId, WireSceneLayerOutline, WireSceneLayerSource,
    WireSceneLayout,
};
use fm_types::InputId;
use fm_ui_model::scene_designer::geometry::{Bounds, Edge, LayerPlacement, Point};
use fm_ui_model::scene_designer::gesture::{CanvasMetrics, GestureKind, GestureModifiers};
use fm_ui_model::scene_designer::snapping::{
    ACTION_SAFE, GuideKind, SnapSettings, TITLE_SAFE, safe_area,
};
use fm_ui_model::scene_designer::{
    EditLabel, Field, LIVE_LAYER_EDITS, SceneLayerEdit, SceneLayoutDocument, layout_stamp,
};

const METRICS: CanvasMetrics = CanvasMetrics {
    tolerance: 8.0,
    rotate_offset: 24.0,
    snap_distance: 6.0,
};

const NONE: GestureModifiers = GestureModifiers {
    shift: false,
    alt: false,
    command: false,
};

const SHIFT: GestureModifiers = GestureModifiers {
    shift: true,
    alt: false,
    command: false,
};

const ALT: GestureModifiers = GestureModifiers {
    shift: false,
    alt: true,
    command: false,
};

fn layer_id(value: u32) -> WireSceneLayerId {
    WireSceneLayerId::new(NonZeroU32::new(value).unwrap())
}

fn outline(id: u32, x: i32, y: i32, width: u32, height: u32) -> WireSceneLayerOutline {
    WireSceneLayerOutline {
        id: layer_id(id),
        name: format!("Layer {id}"),
        source: WireSceneLayerSource::Input(WireInputId::from_domain(InputId::new(
            NonZeroU128::new(u128::from(id)).unwrap(),
        ))),
        x,
        y,
        width,
        height,
        quarter_turns: 0,
        opacity: 255,
        enabled: true,
    }
}

/// A full-frame background, a picture-in-picture, and a logo on 1080p.
fn layout() -> WireSceneLayout {
    WireSceneLayout {
        width: 1_920,
        height: 1_080,
        layers: vec![
            outline(1, 0, 0, 1_920, 1_080),
            outline(2, 1_360, 740, 480, 270),
            outline(3, 100, 100, 200, 100),
        ],
    }
}

fn scene() -> WireSceneId {
    WireSceneId::new(NonZeroU128::new(100).unwrap())
}

#[test]
fn safe_areas_are_the_broadcast_shares_of_the_canvas() {
    let title = safe_area((1_920, 1_080), TITLE_SAFE);
    let expected = Bounds::new(96.0, 54.0, 1_824.0, 1_026.0);
    for (got, want) in [
        (title.left, expected.left),
        (title.top, expected.top),
        (title.right, expected.right),
        (title.bottom, expected.bottom),
    ] {
        assert!((got - want).abs() < 1e-9, "{got} != {want}");
    }
    let action = safe_area((1_920, 1_080), ACTION_SAFE);
    assert!((action.left - 67.2).abs() < 1e-9);
    assert!((action.bottom - 1_042.2).abs() < 1e-9);
}

#[test]
fn an_operator_session_becomes_the_existing_scene_layer_commands() {
    let layout = layout();
    let mut document = SceneLayoutDocument::new(scene(), &layout);
    assert!(!document.is_dirty());

    // Drag the logo near the title-safe corner: it snaps onto it.
    let started = document.press(
        Point::new(200.0, 150.0),
        NONE,
        METRICS,
        SnapSettings::default(),
    );
    assert_eq!(started, Some(GestureKind::Move));
    let feedback = document.drag(Point::new(194.0, 101.0), NONE);
    assert!(
        feedback
            .guides
            .iter()
            .any(|guide| guide.kind == GuideKind::TitleSafe)
    );
    assert!(document.release());
    let logo = document.layer(layer_id(3)).unwrap().placement;
    assert_eq!((logo.x, logo.y), (96, 54));
    assert_eq!(document.undo_label(), Some(EditLabel::Move));

    // Shift on a corner keeps the picture-in-picture's 16:9.
    document.select(layer_id(2), false);
    let started = document.press(
        Point::new(1_840.0, 1_010.0),
        NONE,
        METRICS,
        SnapSettings::default(),
    );
    assert!(matches!(started, Some(GestureKind::Scale(_))));
    document.drag(Point::new(1_600.0, 1_000.0), SHIFT);
    assert!(document.release());
    let pip = document.layer(layer_id(2)).unwrap().placement;
    assert_eq!(
        (pip.x, pip.y, pip.width, pip.height),
        (1_360, 740, 462, 260)
    );

    // Alt on its left edge crops the source without moving the box.
    let started = document.press(
        Point::new(1_360.0, 870.0),
        ALT,
        METRICS,
        SnapSettings::default(),
    );
    assert_eq!(started, Some(GestureKind::Crop(Edge::Left)));
    document.drag(Point::new(1_400.0, 870.0), ALT);
    assert!(document.release());
    let cropped = document.layer(layer_id(2)).unwrap();
    assert!(cropped.crop.left > 0);
    assert_eq!(cropped.placement.x, 1_360);
    assert_eq!(document.undo_label(), Some(EditLabel::Crop));

    // A crop the operator regrets is one undo step; the scale stays.
    assert!(document.undo());
    assert_eq!(document.layer(layer_id(2)).unwrap().crop.left, 0);
    assert_eq!(document.undo_label(), Some(EditLabel::Scale));

    // Numeric fields: opacity, a typed crop, an inverted mask.
    assert!(document.set_field(Field::Opacity, 200.0));
    assert!(document.set_field(Field::CropTop, 40.0));
    assert!(document.set_field(Field::Masked, 1.0));
    assert!(document.set_field(Field::MaskInverted, 1.0));
    assert_eq!(
        document.field_value(layer_id(2), Field::Opacity),
        Some(200.0)
    );

    // Bring the background to the front (drawn last), then send it back:
    // z-order moves keep the selection.
    document.select(layer_id(1), false);
    assert!(document.send_selection_to_end(true));
    assert_eq!(document.index_of(layer_id(1)), Some(2));
    assert!(document.send_selection_to_end(false));
    assert_eq!(document.index_of(layer_id(1)), Some(0));
    assert!(document.is_selected(layer_id(1)));

    let edits = document.edits();
    assert!(edits.contains(&SceneLayerEdit::Geometry {
        index: 1,
        placement: LayerPlacement::new(1_360, 740, 462, 260, 0),
    }));
    assert!(edits.contains(&SceneLayerEdit::Geometry {
        index: 2,
        placement: LayerPlacement::new(96, 54, 200, 100, 0),
    }));
    assert!(edits.contains(&SceneLayerEdit::Appearance {
        index: 1,
        enabled: true,
        opacity: 200,
    }));
    let script = document.cli_script("show.freemix");
    for line in script.lines() {
        assert!(line.starts_with("freemix-cli scene-layer-"), "{line}");
        assert!(line.contains(" show.freemix 100 "), "{line}");
    }
    assert!(script.contains("scene-layer-crop show.freemix 100 1 0 40 1920 1040"));
    assert!(script.contains("scene-layer-geometry show.freemix 100 2 96 54 200 100 0"));
    assert!(
        script
            .lines()
            .any(|line| line.contains("scene-layer-mask") && line.ends_with(" inverted"))
    );
}

#[test]
fn a_new_server_layout_keeps_a_dirty_draft_until_reverted() {
    // The protocol has no live scene layer edit command yet.
    const { assert!(!LIVE_LAYER_EDITS) };
    let layout = layout();
    let mut document = SceneLayoutDocument::new(scene(), &layout);
    document.select(layer_id(3), false);
    assert!(document.nudge(4, 0));
    let mut moved = layout.clone();
    moved.layers[0].opacity = 128;
    assert_ne!(layout_stamp(&moved), layout_stamp(&layout));
    document.rebase(&moved);
    assert!(document.is_dirty());
    document.revert();
    assert!(!document.is_dirty());
    assert_eq!(document.layer(layer_id(1)).unwrap().opacity, 128);
    assert_eq!(document.edits(), Vec::new());
}
