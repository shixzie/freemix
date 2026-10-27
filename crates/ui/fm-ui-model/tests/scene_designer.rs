//! SW-006 layer designer through the public `fm_ui_model::scene_designer`
//! API: one operator session on a replicated scene layout, from direct
//! manipulation with safe-area snapping through numeric fields, crop, mask,
//! stacking, and undo, to the live `scene_layer_*` edits it sends (one
//! server undo step per gesture) and the `freemix-cli scene-layer-*`
//! commands it exports.

use core::num::{NonZeroU32, NonZeroU128};

use fm_protocol::{
    SceneLayerOperation, WireChromaKey, WireInputId, WireLayerCrop, WireLayerEffect, WireLayerKey,
    WireLayerMask, WireLumaKey, WireSceneId, WireSceneLayerId, WireSceneLayerOutline,
    WireSceneLayerSource, WireSceneLayout,
};
use fm_types::InputId;
use fm_ui_model::scene_designer::geometry::{
    Bounds, Edge, Handle, LayerPlacement, MaskRect, Point,
};
use fm_ui_model::scene_designer::gesture::{CanvasMetrics, GestureKind, GestureModifiers};
use fm_ui_model::scene_designer::snapping::{
    ACTION_SAFE, GuideKind, SnapSettings, TITLE_SAFE, safe_area,
};
use fm_ui_model::scene_designer::{
    CHROMA_GREEN, EditLabel, Field, LIVE_LAYER_EDITS, LUMA_BLACK, MAX_SCENE_LAYER_EFFECTS,
    MAX_VISIBLE_LAYERS, SCENE_LAYER_EFFECT_KINDS, SceneLayerEdit, SceneLayoutDocument,
    default_effect, layout_stamp, wire_geometry,
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
        tilt_millidegrees: 0,
        anchor_x: 5_000,
        anchor_y: 5_000,
        z_order: 0,
        crop: None,
        key: None,
        effects: Vec::new(),
        mask: None,
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
fn unsent_offline_edits_survive_a_new_server_layout_until_reverted() {
    let layout = layout();
    let mut document = SceneLayoutDocument::new(scene(), &layout);
    document.select(layer_id(3), false);
    assert!(document.nudge(4, 0));
    let mut moved = layout.clone();
    moved.layers[0].opacity = 128;
    assert_ne!(layout_stamp(&moved), layout_stamp(&layout));
    document.rebase(&moved);
    assert!(document.is_dirty());
    assert!(document.base_changed());
    document.revert();
    assert!(!document.is_dirty());
    assert_eq!(document.layer(layer_id(1)).unwrap().opacity, 128);
    assert_eq!(document.edits(), Vec::new());
}

/// The replicated layout as the designer's base: crop, mask, z-order,
/// tilt, and anchor all come from the server.
fn designed_layout() -> WireSceneLayout {
    let mut layout = layout();
    let pip = &mut layout.layers[1];
    pip.tilt_millidegrees = 12_500;
    pip.anchor_x = 0;
    pip.anchor_y = 10_000;
    pip.z_order = 7;
    pip.crop = Some(WireLayerCrop {
        x: 100,
        y: 50,
        width: 1_600,
        height: 900,
    });
    pip.mask = Some(WireLayerMask {
        x: 10,
        y: 20,
        width: 300,
        height: 200,
        invert: true,
    });
    layout
}

#[test]
fn the_designer_starts_from_the_replicated_crop_mask_z_order_tilt_and_anchor() {
    const { assert!(LIVE_LAYER_EDITS) };
    let document = SceneLayoutDocument::new(scene(), &designed_layout());
    let pip = document.layer(layer_id(2)).unwrap();
    assert_eq!(pip.placement.tilt_millidegrees, 12_500);
    assert_eq!(
        (pip.placement.anchor_x, pip.placement.anchor_y),
        (0, 10_000)
    );
    assert_eq!(pip.z_order, 7);
    assert_eq!(
        (pip.crop.left, pip.crop.top, pip.crop.right, pip.crop.bottom),
        (100, 50, 220, 130)
    );
    assert_eq!(pip.crop.rect((1_920, 1_080)), Some((100, 50, 1_600, 900)));
    assert_eq!(
        pip.mask,
        Some(MaskRect {
            x: 10,
            y: 20,
            width: 300,
            height: 200,
            inverted: true,
        })
    );
    assert_eq!(
        document.field_value(layer_id(2), Field::Rotation),
        Some(12.5)
    );
    assert_eq!(
        document.field_value(layer_id(2), Field::AnchorY),
        Some(10_000.0)
    );
    assert_eq!(document.field_value(layer_id(2), Field::ZOrder), Some(7.0));
    // The z-order 7 layer draws last, over the logo listed above it.
    let order: Vec<_> = document.draw_order().iter().map(|layer| layer.id).collect();
    assert_eq!(order, vec![layer_id(1), layer_id(3), layer_id(2)]);
    assert!(!document.is_dirty());
    // A clean replicated design sends nothing.
    let mut document = document;
    assert_eq!(document.take_live_edits().len(), 0);
}

#[test]
fn a_drag_sends_live_edits_that_continue_one_server_undo_step() {
    let layout = layout();
    let mut document = SceneLayoutDocument::new(scene(), &layout);
    let started = document.press(
        Point::new(1_600.0, 875.0),
        NONE,
        METRICS,
        SnapSettings {
            enabled: false,
            ..SnapSettings::default()
        },
    );
    assert_eq!(started, Some(GestureKind::Move));
    document.drag(Point::new(1_590.0, 865.0), NONE);
    // The first paced send opens the gesture's undo step.
    let first = document.take_live_edits();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].scene, scene());
    assert_eq!(first[0].layer, layer_id(2));
    assert!(!first[0].continues);
    // The gesture carries a token of its own, so the server folds only its
    // own later sends into the step.
    let gesture = first[0].gesture;
    assert_ne!(gesture, 0);
    let SceneLayerOperation::SetGeometry(geometry) = first[0].operation else {
        panic!("a move sends geometry: {:?}", first[0].operation);
    };
    assert_eq!((geometry.x, geometry.y), (1_350, 730));
    // Nothing new, nothing sent.
    assert_eq!(document.take_live_edits().len(), 0);
    // Later sends of the same drag continue it, including the release.
    document.drag(Point::new(1_580.0, 855.0), NONE);
    assert!(document.release());
    let last = document.take_live_edits();
    assert_eq!(last.len(), 1);
    assert!(last[0].continues);
    assert_eq!(last[0].gesture, gesture);
    assert_eq!(
        last[0].operation,
        SceneLayerOperation::SetGeometry(wire_geometry(LayerPlacement::new(
            1_340, 720, 480, 270, 0
        )))
    );
    assert!(!document.has_unsent());

    // The server's echo becomes the draft again; nothing is left to send.
    let mut echoed = layout.clone();
    echoed.layers[1].x = 1_340;
    echoed.layers[1].y = 720;
    document.rebase(&echoed);
    assert!(!document.is_dirty());
    assert!(!document.base_changed());
    assert_eq!(document.take_live_edits().len(), 0);

    // The next gesture opens a new step.
    document.select(layer_id(2), false);
    assert!(document.set_field(Field::Opacity, 100.0));
    let appearance = document.take_live_edits();
    assert_eq!(appearance.len(), 1);
    assert!(!appearance[0].continues);
    assert_ne!(appearance[0].gesture, 0);
    assert_ne!(appearance[0].gesture, gesture);
    assert_eq!(
        appearance[0].operation,
        SceneLayerOperation::SetAppearance {
            enabled: true,
            opacity: 100,
        }
    );
}

#[test]
fn an_echo_mid_gesture_never_snaps_the_draft_back() {
    let layout = layout();
    let mut document = SceneLayoutDocument::new(scene(), &layout);
    document.select(layer_id(3), false);
    document.begin(EditLabel::Field(Field::X));
    document.set_field(Field::X, 150.0);
    let sent = document.take_live_edits();
    assert_eq!(sent.len(), 1);
    document.set_field(Field::X, 180.0);
    // An echo of the first send arrives while the scrub is open.
    let mut echoed = layout.clone();
    echoed.layers[2].x = 150;
    document.rebase(&echoed);
    assert_eq!(document.layer(layer_id(3)).unwrap().placement.x, 180);
    assert!(document.commit());
    let rest = document.take_live_edits();
    assert_eq!(rest.len(), 1);
    assert!(rest[0].continues);
    // A refused edit leaves no echo: settling takes the server's layout.
    document.settle();
    assert_eq!(document.layer(layer_id(3)).unwrap().placement.x, 150);
}

#[test]
fn an_echo_of_an_earlier_send_after_release_never_snaps_the_draft_back() {
    let layout = layout();
    let mut document = SceneLayoutDocument::new(scene(), &layout);
    document.select(layer_id(3), false);
    document.begin(EditLabel::Field(Field::X));
    document.set_field(Field::X, 150.0);
    assert_eq!(document.take_live_edits().len(), 1);
    document.set_field(Field::X, 180.0);
    assert!(document.commit());
    // The release sends the final value...
    assert_eq!(document.take_live_edits().len(), 1);
    // ...before the echo of the first send arrives: the draft keeps the
    // value still in flight instead of jumping back to 150.
    let mut first = layout.clone();
    first.layers[2].x = 150;
    document.rebase(&first);
    assert_eq!(document.layer(layer_id(3)).unwrap().placement.x, 180);
    // A nudge now starts from what was sent, not from the stale echo.
    assert!(document.nudge(1, 0));
    let nudged = document.take_live_edits();
    assert_eq!(nudged.len(), 1);
    let SceneLayerOperation::SetGeometry(geometry) = nudged[0].operation else {
        panic!("a nudge sends geometry: {:?}", nudged[0].operation);
    };
    assert_eq!(geometry.x, 181);
    // The echoes of the later sends settle the draft on the server's layout.
    let mut last = layout.clone();
    last.layers[2].x = 180;
    document.rebase(&last);
    assert_eq!(document.layer(layer_id(3)).unwrap().placement.x, 181);
    last.layers[2].x = 181;
    document.rebase(&last);
    assert!(!document.is_dirty());
    assert_eq!(document.layer(layer_id(3)).unwrap().placement.x, 181);
    // With nothing in flight another editor's change is taken at once.
    let mut other = last.clone();
    other.layers[0].opacity = 7;
    document.rebase(&other);
    assert_eq!(document.layer(layer_id(1)).unwrap().opacity, 7);
    assert!(!document.is_dirty());
}

#[test]
fn echoes_of_live_edits_never_flag_the_server_layout_as_changed_under_the_draft() {
    let layout = layout();
    let mut document = SceneLayoutDocument::new(scene(), &layout);
    document.select(layer_id(3), false);
    document.begin(EditLabel::Field(Field::X));
    document.set_field(Field::X, 150.0);
    assert_eq!(document.take_live_edits().len(), 1);
    // Between paced sends the scrub has moved on when its own echo lands.
    document.set_field(Field::X, 180.0);
    let mut first = layout.clone();
    first.layers[2].x = 150;
    document.rebase(&first);
    assert!(!document.base_changed());
    assert!(document.commit());
    assert_eq!(document.take_live_edits().len(), 1);
    // The last send is refused: no echo, so the draft settles on the
    // server's layout and nothing stays flagged.
    document.settle();
    assert_eq!(document.layer(layer_id(3)).unwrap().placement.x, 150);
    assert!(!document.base_changed());
    assert!(!document.is_dirty());
}

#[test]
fn free_rotation_turns_about_the_anchor_and_moving_the_anchor_keeps_the_layer_put() {
    let mut document = SceneLayoutDocument::new(scene(), &layout());
    document.select(layer_id(2), false);
    let before = document.layer(layer_id(2)).unwrap().placement;
    let pivot = before.anchor_point();
    // A typed free angle: 30 degrees about the centre anchor.
    assert!(document.set_field(Field::Rotation, 30.0));
    let turned = document.layer(layer_id(2)).unwrap().placement;
    assert_eq!(
        (turned.quarter_turns, turned.tilt_millidegrees),
        (0, 30_000)
    );
    assert!(turned.anchor_point().distance(pivot) < 1.0);
    // 100 degrees is a quarter turn and a 10 degree tilt.
    assert!(document.set_field(Field::Rotation, 100.0));
    let turned = document.layer(layer_id(2)).unwrap().placement;
    assert_eq!(
        (turned.quarter_turns, turned.tilt_millidegrees),
        (1, 10_000)
    );
    assert_eq!(
        document.field_value(layer_id(2), Field::Rotation),
        Some(100.0)
    );

    // Moving the anchor to the top-left corner keeps the drawn box.
    let frame = document.layer(layer_id(2)).unwrap().placement.frame();
    assert!(document.set_field(Field::AnchorX, 0.0));
    assert!(document.set_field(Field::AnchorY, 0.0));
    let anchored = document.layer(layer_id(2)).unwrap().placement;
    assert_eq!((anchored.anchor_x, anchored.anchor_y), (0, 0));
    for (moved, kept) in anchored.frame().corners().iter().zip(frame.corners()) {
        assert!(moved.distance(kept) < 1.5, "{moved:?} vs {kept:?}");
    }
    // Rotating now turns about that corner: it stays put.
    let corner = anchored.anchor_point();
    assert!(document.set_field(Field::Rotation, 145.0));
    let turned = document.layer(layer_id(2)).unwrap().placement;
    assert!(turned.anchor_point().distance(corner) < 1.0);
    assert_eq!(
        (turned.quarter_turns, turned.tilt_millidegrees),
        (2, -35_000)
    );

    // Alt on the anchor handle drags the pivot (without Alt a press there
    // moves the layer); snapping lands on the centre.
    let centre = document
        .layer(layer_id(2))
        .unwrap()
        .placement
        .frame()
        .centre;
    let anchor_handle = turned.frame().handle_position(Handle::Anchor, 24.0);
    let started = document.press(anchor_handle, ALT, METRICS, SnapSettings::default());
    assert_eq!(started, Some(GestureKind::Anchor));
    document.drag(Point::new(centre.x + 2.0, centre.y - 2.0), ALT);
    assert!(document.release());
    let placement = document.layer(layer_id(2)).unwrap().placement;
    assert_eq!((placement.anchor_x, placement.anchor_y), (5_000, 5_000));
    assert_eq!(document.undo_label(), Some(EditLabel::Anchor));

    // The live edit carries the whole geometry: quarter turns, tilt, anchor.
    let sent = document.take_live_edits();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].operation,
        SceneLayerOperation::SetGeometry(wire_geometry(placement))
    );
    // The export names the angle in degrees and the anchor when not centred.
    assert!(
        document
            .layout_script("show.freemix")
            .contains("scene-layer-geometry show.freemix 100 1 ")
    );
    let edit = SceneLayerEdit::Geometry {
        index: 1,
        placement: LayerPlacement::new(10, 20, 300, 200, 1)
            .with_tilt(-12_250)
            .with_anchor(0, 10_000),
    };
    assert_eq!(
        edit.cli_command("show.freemix", scene()),
        "freemix-cli scene-layer-geometry show.freemix 100 1 10 20 300 200 77.75 --anchor 0 10000"
    );
}

#[test]
fn crop_never_cuts_the_mask_and_mask_and_crop_send_in_a_valid_order() {
    let mut document = SceneLayoutDocument::new(scene(), &designed_layout());
    document.select(layer_id(2), false);
    // The mask reaches 310 x 220 of the 1600 x 900 cropped source: a crop
    // stops there.
    assert!(document.set_field(Field::CropRight, 5_000.0));
    let pip = document.layer(layer_id(2)).unwrap();
    assert_eq!(pip.crop.cropped((1_920, 1_080)).0, 310);
    let sent = document.take_live_edits();
    assert_eq!(sent.len(), 1);
    assert!(matches!(
        sent[0].operation,
        SceneLayerOperation::SetCrop(Some(_))
    ));
    // Clearing the mask, then cropping further: the mask clear goes first.
    assert!(document.set_field(Field::Masked, 0.0));
    assert!(document.set_field(Field::CropRight, 1_700.0));
    let sent = document.take_live_edits();
    assert!(matches!(
        sent[0].operation,
        SceneLayerOperation::SetMask(None)
    ));
    assert!(matches!(
        sent[1].operation,
        SceneLayerOperation::SetCrop(Some(_))
    ));
    // A z-order edit is its own live edit.
    assert!(document.set_field(Field::ZOrder, -3.0));
    let sent = document.take_live_edits();
    assert_eq!(
        sent[0].operation,
        SceneLayerOperation::SetZOrder { z_order: -3 }
    );
    assert!(
        document
            .layout_script("show.freemix")
            .contains("scene-layer-z-order show.freemix 100 1 -3")
    );
}

#[test]
fn a_restack_sends_moves_by_layer_id() {
    let mut document = SceneLayoutDocument::new(scene(), &layout());
    document.select(layer_id(1), false);
    assert!(document.send_selection_to_end(true));
    let sent = document.take_live_edits();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].layer, layer_id(1));
    assert_eq!(sent[0].operation, SceneLayerOperation::Move { index: 2 });
    assert!(!sent[0].continues);
}

/// A scene draws at most 64 visible layers (the compositor's limit): the
/// designer does not show a hidden layer of a full scene, so it never sends
/// an edit the server would refuse.
#[test]
fn a_full_scene_keeps_its_hidden_layers_hidden() {
    assert_eq!(MAX_VISIBLE_LAYERS, 64);
    let mut layout = layout();
    layout.layers = (1..=65)
        .map(|id| {
            let mut layer = outline(id, 0, 0, 10, 10);
            layer.enabled = id <= 64;
            layer
        })
        .collect();
    let mut document = SceneLayoutDocument::new(scene(), &layout);
    assert_eq!(document.visible_layer_count(), 64);
    assert!(document.can_show(layer_id(1)));
    assert!(!document.can_show(layer_id(65)));
    document.select(layer_id(65), false);
    assert!(!document.set_field(Field::Enabled, 1.0));
    assert_eq!(document.take_live_edits().len(), 0);
    // Hiding another layer makes room.
    document.select(layer_id(1), false);
    assert!(document.set_field(Field::Enabled, 0.0));
    assert!(document.can_show(layer_id(65)));
    document.select(layer_id(65), false);
    assert!(document.set_field(Field::Enabled, 1.0));
    assert_eq!(document.visible_layer_count(), 64);
}

fn effect(name: &str, parameters: &[i32]) -> WireLayerEffect {
    WireLayerEffect {
        name: name.into(),
        parameters: parameters.to_vec(),
    }
}

/// IN-020: the designer starts from the replicated key and effects, and a
/// change of either restarts the exact preview.
#[test]
fn the_designer_starts_from_the_replicated_key_and_effects() {
    let mut keyed = layout();
    keyed.layers[1].key = Some(WireLayerKey::Luma(WireLumaKey {
        low: 20,
        high: 200,
        softness: 8,
        invert: true,
        refinement: fm_protocol::WireKeyRefinement::NONE,
    }));
    keyed.layers[1].effects = vec![effect("saturation", &[-40])];
    assert_ne!(layout_stamp(&keyed), layout_stamp(&layout()));
    let document = SceneLayoutDocument::new(scene(), &keyed);
    let pip = document.layer(layer_id(2)).unwrap();
    assert_eq!(pip.key, keyed.layers[1].key);
    assert_eq!(pip.effects, keyed.layers[1].effects);
    assert_eq!(
        document.field_value(layer_id(2), Field::LumaHigh),
        Some(200.0)
    );
    assert_eq!(
        document.field_value(layer_id(2), Field::LumaInvert),
        Some(1.0)
    );
    let saturation = Field::EffectParameter {
        effect: 0,
        parameter: 0,
    };
    assert_eq!(document.field_value(layer_id(2), saturation), Some(-40.0));
    let missing = Field::EffectParameter {
        effect: 1,
        parameter: 0,
    };
    assert_eq!(document.field_value(layer_id(2), missing), None);
}

/// IN-020: choosing a key kind is one step and one live `SetKey`; a
/// similarity scrub folds into one undo step under one gesture token, and
/// the next edit opens a new one.
#[test]
fn key_edits_send_set_key_and_a_scrub_folds_into_one_gesture() {
    let mut document = SceneLayoutDocument::new(scene(), &layout());
    document.select(layer_id(2), false);
    assert!(document.set_key_kind(Some(true)));
    assert_eq!(document.undo_label(), Some(EditLabel::Key));
    let sent = document.take_live_edits();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].layer, layer_id(2));
    assert_eq!(
        sent[0].operation,
        SceneLayerOperation::SetKey(Some(WireLayerKey::Chroma(CHROMA_GREEN)))
    );
    assert!(!sent[0].continues);
    let pick = sent[0].gesture;
    assert_ne!(pick, 0);
    // The same kind again keeps the key as it is.
    assert!(!document.set_key_kind(Some(true)));

    // A similarity scrub: paced sends continue one gesture.
    document.begin(EditLabel::Field(Field::KeySimilarity));
    assert!(document.set_field(Field::KeySimilarity, 80.0));
    let first = document.take_live_edits();
    assert_eq!(first.len(), 1);
    assert!(!first[0].continues);
    let scrub = first[0].gesture;
    assert_ne!(scrub, 0);
    assert_ne!(scrub, pick);
    assert!(document.set_field(Field::KeySimilarity, 300.0));
    assert!(document.set_field(Field::KeySpill, 12.0));
    assert!(document.commit());
    let last = document.take_live_edits();
    assert_eq!(last.len(), 1);
    assert!(last[0].continues);
    assert_eq!(last[0].gesture, scrub);
    assert_eq!(
        last[0].operation,
        SceneLayerOperation::SetKey(Some(WireLayerKey::Chroma(WireChromaKey {
            similarity: 255,
            spill: 12,
            ..CHROMA_GREEN
        })))
    );
    assert_eq!(
        document.undo_label(),
        Some(EditLabel::Field(Field::KeySimilarity))
    );
    // One undo takes back the whole scrub.
    assert!(document.undo());
    assert_eq!(
        document.layer(layer_id(2)).unwrap().key,
        Some(WireLayerKey::Chroma(CHROMA_GREEN))
    );
    assert_eq!(document.take_live_edits().len(), 1);

    // A luma key: its thresholds never cross, and luma fields leave a
    // layer without a luma key alone.
    document.select(layer_id(3), false);
    assert!(!document.set_field(Field::LumaLow, 40.0));
    assert!(document.set_key_kind(Some(false)));
    assert_eq!(
        document.layer(layer_id(3)).unwrap().key,
        Some(WireLayerKey::Luma(LUMA_BLACK))
    );
    assert!(document.set_field(Field::LumaHigh, 100.0));
    assert!(document.set_field(Field::LumaLow, 150.0));
    assert!(document.set_field(Field::LumaInvert, 1.0));
    assert_eq!(
        document.layer(layer_id(3)).unwrap().key,
        Some(WireLayerKey::Luma(WireLumaKey {
            low: 99,
            high: 100,
            softness: LUMA_BLACK.softness,
            invert: true,
            refinement: fm_protocol::WireKeyRefinement::NONE,
        }))
    );
    assert!(!document.set_key(Some(WireLayerKey::Luma(WireLumaKey {
        low: 9,
        high: 9,
        softness: 0,
        invert: false,
        refinement: fm_protocol::WireKeyRefinement::NONE,
    }))));
    // Removing the key is one step and one `SetKey(None)`.
    document.take_live_edits();
    assert!(document.set_key_kind(None));
    let cleared = document.take_live_edits();
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0].operation, SceneLayerOperation::SetKey(None));
    assert!(!cleared[0].continues);
}

/// SW-008: a key/fill key takes its matte from another input, keeps its
/// matte settings when only the input changes, and every key carries the
/// matte refinements, clamped to their bounds, into the live edit and the
/// exported command.
#[test]
fn key_fill_and_matte_refinements_edit_live_and_export() {
    let key_input = WireInputId::from_domain(InputId::new(NonZeroU128::new(7).unwrap()));
    let other = WireInputId::from_domain(InputId::new(NonZeroU128::new(8).unwrap()));
    let mut document = SceneLayoutDocument::new(scene(), &layout());
    document.select(layer_id(2), false);
    // A chroma key's refinement carries over to the key/fill key.
    assert!(document.set_key_kind(Some(true)));
    assert!(document.set_field(Field::KeyChoke, 3.0));
    assert!(document.set_key_fill(key_input));
    let Some(WireLayerKey::Fill(fill)) = document.layer(layer_id(2)).unwrap().key else {
        panic!("a key/fill key");
    };
    assert_eq!(
        fill,
        fm_protocol::WireFillKey {
            refinement: fm_protocol::WireKeyRefinement {
                choke: 3,
                ..fm_protocol::WireKeyRefinement::NONE
            },
            ..fm_ui_model::scene_designer::fill_linear(key_input)
        }
    );
    // Luma fields and the alpha switch edit the key/fill key.
    assert!(document.set_field(Field::LumaLow, 30.0));
    assert!(document.set_field(Field::FillAlpha, 1.0));
    assert_eq!(
        document.field_value(layer_id(2), Field::FillAlpha),
        Some(1.0)
    );
    // Changing only the input keeps the matte settings.
    assert!(document.set_key_fill(other));
    let Some(WireLayerKey::Fill(fill)) = document.layer(layer_id(2)).unwrap().key else {
        panic!("a key/fill key");
    };
    assert_eq!(
        (fill.key_input, fill.low, fill.alpha_channel),
        (other, 30, true)
    );
    assert!(!document.set_key_fill(other));

    // Refinements clamp: choke within 8 either way, edge softness within
    // 8, clip black below clip white.
    assert!(document.set_field(Field::KeyChoke, -50.0));
    assert_eq!(
        document.field_value(layer_id(2), Field::KeyChoke),
        Some(-8.0)
    );
    assert!(document.set_field(Field::KeyEdgeSoftness, 40.0));
    assert_eq!(
        document.field_value(layer_id(2), Field::KeyEdgeSoftness),
        Some(8.0)
    );
    assert!(document.set_field(Field::KeyClipWhite, 100.0));
    assert!(document.set_field(Field::KeyClipBlack, 200.0));
    assert_eq!(
        document.field_value(layer_id(2), Field::KeyClipBlack),
        Some(99.0)
    );
    // Despill starts at full strength; its strength edits only while on.
    assert!(!document.set_field(Field::KeyDespillStrength, 10.0));
    assert!(document.set_field(Field::KeyDespill, 2.0));
    assert_eq!(
        document.field_value(layer_id(2), Field::KeyDespillStrength),
        Some(255.0)
    );
    assert!(document.set_field(Field::KeyDespillStrength, 128.0));
    // Light wrap starts at its default width when turned on; its width
    // edits only while on and stays within 1..=8.
    assert!(!document.set_field(Field::KeyLightWrapWidth, 5.0));
    assert!(document.set_field(Field::KeyLightWrap, 300.0));
    assert_eq!(
        document.field_value(layer_id(2), Field::KeyLightWrapWidth),
        Some(f64::from(
            fm_ui_model::scene_designer::DEFAULT_LIGHT_WRAP_WIDTH
        ))
    );
    assert!(document.set_field(Field::KeyLightWrapWidth, 0.0));
    assert_eq!(
        document.field_value(layer_id(2), Field::KeyLightWrapWidth),
        Some(1.0)
    );
    assert!(document.set_field(Field::KeyLightWrapWidth, 40.0));
    let live = document.take_live_edits();
    let Some(SceneLayerOperation::SetKey(Some(sent))) =
        live.last().map(|edit| edit.operation.clone())
    else {
        panic!("a live SetKey");
    };
    assert_eq!(
        sent.refinement(),
        fm_protocol::WireKeyRefinement {
            clip_black: 99,
            clip_white: 100,
            choke: -8,
            edge_softness: 8,
            despill: fm_protocol::WireKeyDespill::Blue { strength: 128 },
            light_wrap: 255,
            light_wrap_width: 8,
            garbage_matte: None,
        }
    );
    assert!(sent.refinement().is_valid());
    let script = document.cli_script("show.freemix");
    assert!(script.lines().any(|line| line
        == "freemix-cli scene-layer-key show.freemix 100 1 fill 8 alpha 30 255 255 off \
            --clip 99 100 --choke -8 --edge-softness 8 --despill blue 128 --light-wrap 255 8"));
    // Back to a luma key: the refinement stays, the key input goes.
    assert!(document.set_key_kind(Some(false)));
    let key = document.layer(layer_id(2)).unwrap().key.unwrap();
    assert!(matches!(key, WireLayerKey::Luma(_)));
    assert_eq!(key.refinement(), sent.refinement());
}

/// SW-008: a key's polygon garbage matte turns on as an inset
/// quadrilateral, keeps its inside or outside, moves points within the
/// layer, adds and removes points within three to eight as one step each,
/// and reaches the live edit and the exported command.
#[test]
fn garbage_matte_points_edit_live_and_export() {
    use fm_types::{GarbageMatte, MattePoint};
    let matte = |document: &SceneLayoutDocument| {
        document
            .layer(layer_id(2))
            .unwrap()
            .key
            .and_then(|key| key.refinement().garbage_matte)
    };
    let mut document = SceneLayoutDocument::new(scene(), &layout());
    document.select(layer_id(2), false);
    // Without a key there is no matte to edit.
    assert!(!document.set_field(Field::Matte, 1.0));
    assert!(document.set_key_kind(Some(true)));
    assert_eq!(document.field_value(layer_id(2), Field::Matte), Some(0.0));
    assert!(!document.set_field(Field::MattePointX { point: 0 }, 10.0));
    assert!(!document.add_matte_point());

    assert!(document.set_field(Field::Matte, 1.0));
    let inset = GarbageMatte::inset(fm_ui_model::scene_designer::DEFAULT_GARBAGE_MATTE_INSET);
    assert_eq!(matte(&document), Some(inset));
    assert_eq!(
        document.field_value(layer_id(2), Field::MattePointX { point: 1 }),
        Some(900.0)
    );
    // Keeping the outside keeps the points.
    assert!(document.set_field(Field::Matte, 2.0));
    assert_eq!(matte(&document), Some(inset.with_invert(true)));
    // A feather (Protocol 2.115) clamps to 64 pixels and survives point
    // edits; it ends at 0 again for the checks below.
    assert!(document.set_field(Field::MatteFeather, 500.0));
    assert_eq!(
        document.field_value(layer_id(2), Field::MatteFeather),
        Some(64.0)
    );
    assert!(document.set_field(Field::MattePointX { point: 1 }, 880.0));
    assert!(document.add_matte_point());
    assert_eq!(matte(&document).unwrap().feather(), 64);
    assert!(document.undo());
    assert!(document.undo());
    assert!(document.set_field(Field::MatteFeather, 0.0));
    assert_eq!(matte(&document), Some(inset.with_invert(true)));
    // Points clamp to the layer; a missing point is not edited.
    assert!(document.set_field(Field::MattePointX { point: 0 }, -40.0));
    assert!(document.set_field(Field::MattePointY { point: 0 }, 5_000.0));
    assert_eq!(
        matte(&document).unwrap().points()[0],
        MattePoint::new(0, 1_000)
    );
    assert!(!document.set_field(Field::MattePointX { point: 4 }, 10.0));
    assert_eq!(
        document.undo_label(),
        Some(EditLabel::Field(Field::MattePointY { point: 0 }))
    );

    // A new point halves the closing edge; eight is the most.
    assert!(document.add_matte_point());
    assert_eq!(document.undo_label(), Some(EditLabel::GarbageMatte));
    assert_eq!(
        matte(&document).unwrap().points()[4],
        MattePoint::new(50, 950)
    );
    while matte(&document).unwrap().points().len() < 8 {
        assert!(document.add_matte_point());
    }
    assert!(!document.add_matte_point());
    // Removal stops at three points and ignores a missing index.
    assert!(!document.remove_matte_point(8));
    while matte(&document).unwrap().points().len() > 3 {
        assert!(document.remove_matte_point(0));
    }
    assert!(!document.remove_matte_point(0));
    let three = matte(&document).unwrap();
    assert!(three.invert());

    document.take_live_edits();
    assert!(document.set_field(Field::MattePointX { point: 2 }, 333.0));
    let live = document.take_live_edits();
    let Some(SceneLayerOperation::SetKey(Some(sent))) =
        live.last().map(|edit| edit.operation.clone())
    else {
        panic!("a live SetKey");
    };
    let sent_matte = sent.refinement().garbage_matte.unwrap();
    assert_eq!(sent_matte.points()[2].x, 333);
    assert!(sent.refinement().is_valid());
    let points = sent_matte
        .points()
        .iter()
        .map(|point| format!("{}:{}", point.x, point.y))
        .collect::<Vec<_>>()
        .join(",");
    let script = document.cli_script("show.freemix");
    assert!(
        script
            .lines()
            .any(|line| line.ends_with(&format!(" --matte outside {points}"))),
        "{script}"
    );
    // Off drops the matte and undo puts it back.
    assert!(document.set_field(Field::Matte, 0.0));
    assert_eq!(matte(&document), None);
    assert!(document.undo());
    assert_eq!(matte(&document), Some(sent_matte));
}

/// SW-008: a garbage matte edit that would leave every point on one line
/// (a matte without area, which removes the whole layer) is refused and
/// flagged for the inspector; the next accepted edit clears the flag, and a
/// drag moves both coordinates at once so it may pass the line diagonally.
#[test]
fn garbage_matte_edits_refuse_a_flat_matte() {
    use fm_types::MattePoint;
    let matte = |document: &SceneLayoutDocument| {
        document
            .layer(layer_id(2))
            .unwrap()
            .key
            .and_then(|key| key.refinement().garbage_matte)
            .unwrap()
    };
    let mut document = SceneLayoutDocument::new(scene(), &layout());
    document.select(layer_id(2), false);
    assert!(document.set_key_kind(Some(true)));
    assert!(document.set_field(Field::Matte, 1.0));
    assert!(document.remove_matte_point(3));
    // (100,100) (900,100) (900,900): moving the last point onto the line
    // through the first two is refused.
    let before = matte(&document);
    assert!(!document.set_field(Field::MattePointY { point: 2 }, 100.0));
    assert_eq!(matte(&document), before);
    assert!(document.matte_refused());
    // An accepted move clears the flag.
    assert!(document.set_field(Field::MattePointY { point: 2 }, 500.0));
    assert!(!document.matte_refused());
    // A drag onto the line is refused as a whole; off it, both axes move.
    assert!(!document.move_matte_point(2, 500.0, 100.0));
    assert!(document.matte_refused());
    assert!(document.move_matte_point(2, 100.0, 900.0));
    assert!(!document.matte_refused());
    assert_eq!(matte(&document).points()[2], MattePoint::new(100, 900));

    // Removing a point that leaves the other three on one line is refused.
    assert!(document.add_matte_point());
    assert!(document.move_matte_point(3, 500.0, 100.0));
    assert!(!document.remove_matte_point(2));
    assert!(document.matte_refused());
    assert_eq!(matte(&document).points().len(), 4);
    assert!(document.remove_matte_point(3));
    assert!(!document.matte_refused());
}

/// IN-020: effect list edits send the whole list as `SetEffects`; a
/// parameter scrub folds into one gesture; reorder, removal, the list
/// limit, and mixed selections behave.
#[test]
fn effect_edits_send_set_effects_and_respect_the_limits() {
    assert_eq!(MAX_SCENE_LAYER_EFFECTS, 8);
    assert_eq!(default_effect("levels").unwrap().parameters, vec![0, 255]);
    assert!(default_effect("blur").is_none());
    for (name, ranges) in SCENE_LAYER_EFFECT_KINDS {
        let neutral = default_effect(name).unwrap();
        assert!(neutral.is_valid(), "{name}");
        assert_eq!(neutral.parameters.len(), ranges.len());
    }

    let mut document = SceneLayoutDocument::new(scene(), &layout());
    document.select(layer_id(2), false);
    assert!(!document.add_effect("blur"));
    assert!(document.add_effect("levels"));
    assert_eq!(document.undo_label(), Some(EditLabel::Effects));
    let sent = document.take_live_edits();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].operation,
        SceneLayerOperation::SetEffects(vec![effect("levels", &[0, 255])])
    );
    assert!(!sent[0].continues);
    let added = sent[0].gesture;

    // A black-level scrub stops below the white level and folds.
    let black = Field::EffectParameter {
        effect: 0,
        parameter: 0,
    };
    let white = Field::EffectParameter {
        effect: 0,
        parameter: 1,
    };
    document.begin(EditLabel::Field(black));
    assert!(document.set_field(black, 30.0));
    let first = document.take_live_edits();
    assert!(!first[0].continues);
    assert_ne!(first[0].gesture, added);
    assert!(document.set_field(black, 999.0));
    assert!(document.commit());
    let last = document.take_live_edits();
    assert!(last[0].continues);
    assert_eq!(last[0].gesture, first[0].gesture);
    assert_eq!(
        last[0].operation,
        SceneLayerOperation::SetEffects(vec![effect("levels", &[254, 255])])
    );
    assert!(!document.set_field(white, 0.0));
    assert_eq!(
        document.layer(layer_id(2)).unwrap().effects,
        vec![effect("levels", &[254, 255])]
    );

    // Reorder and remove are one step each.
    assert!(document.add_effect("flip"));
    let flip_vertical = Field::EffectParameter {
        effect: 1,
        parameter: 1,
    };
    assert!(document.set_field(flip_vertical, 1.0));
    assert!(document.move_effect(1, 0));
    assert_eq!(document.undo_label(), Some(EditLabel::Effects));
    document.take_live_edits();
    assert_eq!(
        document.layer(layer_id(2)).unwrap().effects,
        vec![effect("flip", &[0, 1]), effect("levels", &[254, 255])]
    );
    assert!(document.remove_effect(1));
    let removed = document.take_live_edits();
    assert_eq!(
        removed[0].operation,
        SceneLayerOperation::SetEffects(vec![effect("flip", &[0, 1])])
    );

    // At most eight effects.
    while document.can_add_effect() {
        assert!(document.add_effect("sharpen"));
    }
    assert_eq!(
        document.layer(layer_id(2)).unwrap().effects.len(),
        MAX_SCENE_LAYER_EFFECTS
    );
    assert!(!document.add_effect("saturation"));
    document.take_live_edits();

    // A selection whose effect kinds differ takes no effect edits.
    document.select(layer_id(3), true);
    assert!(!document.effects_shared());
    assert!(!document.can_add_effect());
    assert!(!document.remove_effect(0));
    // Layers that share their (empty) list take an effect together.
    document.select(layer_id(1), false);
    document.select(layer_id(3), true);
    assert!(document.effects_shared());
    assert!(document.add_effect("saturation"));
    let both = document.take_live_edits();
    assert_eq!(both.len(), 2);
    assert_eq!(both[0].gesture, both[1].gesture);
}

/// IN-020: the export reproduces keys and effect lists with the
/// `scene-layer-key` and `scene-layer-effect-*` commands.
#[test]
fn keys_and_effects_export_as_freemix_cli_commands() {
    let mut document = SceneLayoutDocument::new(scene(), &layout());
    document.select(layer_id(2), false);
    assert!(document.set_key_kind(Some(true)));
    assert!(document.add_effect("brightness_contrast"));
    let brightness = Field::EffectParameter {
        effect: 0,
        parameter: 0,
    };
    assert!(document.set_field(brightness, -20.0));
    assert!(document.edits().contains(&SceneLayerEdit::Key {
        index: 1,
        key: Some(WireLayerKey::Chroma(CHROMA_GREEN)),
    }));
    let script = document.cli_script("show.freemix");
    let lines: Vec<&str> = script.lines().collect();
    assert!(
        lines.contains(&"freemix-cli scene-layer-key show.freemix 100 1 chroma 0 255 0 64 32 0")
    );
    assert!(lines.contains(&"freemix-cli scene-layer-effect-clear show.freemix 100 1"));
    assert!(lines.contains(
        &"freemix-cli scene-layer-effect-add show.freemix 100 1 brightness_contrast -20 0"
    ));
    // The whole layout sets every other layer's key and effects too.
    let layout_script = document.layout_script("show.freemix");
    let lines: Vec<&str> = layout_script.lines().collect();
    assert!(lines.contains(&"freemix-cli scene-layer-key show.freemix 100 0 none"));
    assert!(lines.contains(&"freemix-cli scene-layer-effect-clear show.freemix 100 2"));
    let luma = SceneLayerEdit::Key {
        index: 3,
        key: Some(WireLayerKey::Luma(WireLumaKey {
            low: 10,
            high: 240,
            softness: 5,
            invert: true,
            refinement: fm_protocol::WireKeyRefinement::NONE,
        })),
    };
    assert_eq!(
        luma.cli_command("show.freemix", scene()),
        "freemix-cli scene-layer-key show.freemix 100 3 luma 10 240 5 on"
    );
}

/// SW-008: an auto key from the layer's input picture is one undo step and
/// one live `SetKey` that keeps the layer's matte refinement; applying the
/// same result again changes nothing, and Undo puts the old key back.
#[test]
fn an_auto_key_is_one_step_and_one_set_key() {
    use fm_ui_model::auto_key::{AutoKeyPicture, AutoKeySample, auto_key};
    let mut keyed = layout();
    let refinement = fm_protocol::WireKeyRefinement {
        clip_black: 8,
        clip_white: 250,
        choke: 1,
        edge_softness: 2,
        despill: fm_protocol::WireKeyDespill::Off,
        light_wrap: 0,
        light_wrap_width: 0,
        garbage_matte: Some(fm_types::GarbageMatte::inset(50)),
    };
    keyed.layers[1].key = Some(WireLayerKey::Luma(WireLumaKey {
        low: 16,
        high: 255,
        softness: 8,
        invert: false,
        refinement,
    }));
    let mut document = SceneLayoutDocument::new(scene(), &keyed);
    let mut rgba = Vec::new();
    for index in 0..16 * 9 {
        let pixel = if index % 16 < 4 {
            [200, 160, 130, 255]
        } else {
            [30, 190, 60, 255]
        };
        rgba.extend_from_slice(&pixel);
    }
    let picture = AutoKeyPicture::new(16, 9, &rgba).unwrap();
    let auto = auto_key(picture, AutoKeySample::Dominant).unwrap();
    assert!(document.apply_auto_key(layer_id(2), &auto));
    assert_eq!(document.undo_label(), Some(EditLabel::AutoKey));
    let expected = WireLayerKey::Chroma(WireChromaKey {
        red: 30,
        green: 190,
        blue: 60,
        similarity: auto.similarity,
        smoothness: auto.smoothness,
        spill: auto.spill,
        refinement: fm_protocol::WireKeyRefinement {
            despill: auto.despill,
            ..refinement
        },
    });
    let sent = document.take_live_edits();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].layer, layer_id(2));
    assert_eq!(
        sent[0].operation,
        SceneLayerOperation::SetKey(Some(expected))
    );
    assert!(!document.apply_auto_key(layer_id(2), &auto));
    assert!(!document.apply_auto_key(layer_id(99), &auto));
    assert!(document.undo());
    assert_eq!(
        document.layer(layer_id(2)).unwrap().key,
        keyed.layers[1].key
    );
}
