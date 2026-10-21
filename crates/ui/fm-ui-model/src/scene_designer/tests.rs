use core::num::{NonZeroU32, NonZeroU128};
use core::time::Duration;

use fm_protocol::{
    TitlePreviewSample, WireInputId, WireSceneId, WireSceneLayerId, WireSceneLayerOutline,
    WireSceneLayerSource, WireSceneLayout,
};
use fm_types::InputId;

use super::geometry::{
    Bounds, Corner, CropInsets, Edge, Handle, HitLayer, HitTarget, LayerPlacement, MaskRect, Point,
    hit_test,
};
use super::gesture::{
    CanvasMetrics, GestureKind, GestureModifiers, Readout, quarter_turns, rotation_angle,
};
use super::preview::{
    PREVIEW_MIN_INTERVAL, PREVIEW_REFRESH, PREVIEW_TIMEOUT, PreviewPacer, PreviewSubject,
};
use super::snapping::{
    GuideKind, SnapSettings, SnapTargets, TITLE_SAFE, safe_area, snap_bounds, snap_point,
};
use super::{EditLabel, Field, SceneLayerEdit, SceneLayoutDocument, layout_stamp};

const EPSILON: f64 = 1e-9;

fn layer_id(value: u32) -> WireSceneLayerId {
    WireSceneLayerId::new(NonZeroU32::new(value).unwrap())
}

fn scene() -> WireSceneId {
    WireSceneId::new(NonZeroU128::new(100).unwrap())
}

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn outline(id: u32, x: i32, y: i32, width: u32, height: u32) -> WireSceneLayerOutline {
    WireSceneLayerOutline {
        id: layer_id(id),
        name: format!("Layer {id}"),
        source: WireSceneLayerSource::Input(WireInputId::from_domain(input(u128::from(id)))),
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

fn document() -> SceneLayoutDocument {
    SceneLayoutDocument::new(scene(), &layout())
}

/// One canvas pixel per screen point.
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

fn close(a: Point, b: Point) -> bool {
    a.distance(b) < 1e-6
}

fn no_snapping() -> SnapSettings {
    SnapSettings {
        enabled: false,
        ..SnapSettings::default()
    }
}

// Geometry.

#[test]
fn a_quarter_turn_swaps_the_footprint_and_keeps_the_frame_centred() {
    let placement = LayerPlacement::new(100, 50, 400, 200, 1);
    assert_eq!(placement.footprint_size(), (200, 400));
    let footprint = placement.footprint();
    assert_eq!(footprint, Bounds::new(100.0, 50.0, 300.0, 450.0));
    let frame = placement.frame();
    assert!(close(frame.centre, Point::new(200.0, 250.0)));
    // The layer's own top-left corner turns to the footprint's top-right.
    assert!(close(frame.corners()[0], Point::new(300.0, 50.0)));
    assert!(close(frame.bounds().centre(), footprint.centre()));
}

#[test]
fn local_and_world_coordinates_round_trip_at_any_angle() {
    for angle in [0.0, 17.5, 90.0, 135.0, 270.0, 333.0] {
        let frame = super::geometry::LayerFrame {
            centre: Point::new(640.0, 360.0),
            half_width: 200.0,
            half_height: 80.0,
            angle_degrees: angle,
            anchor: Point::default(),
        };
        let world = Point::new(700.0, 300.0);
        assert!(
            close(frame.to_world(frame.to_local(world)), world),
            "{angle}"
        );
    }
    let turned = super::geometry::LayerFrame {
        centre: Point::new(0.0, 0.0),
        half_width: 10.0,
        half_height: 5.0,
        angle_degrees: 90.0,
        anchor: Point::default(),
    };
    // Clockwise in y-down canvas space: local +x points down.
    assert!(close(
        turned.to_world(Point::new(10.0, 0.0)),
        Point::new(0.0, 10.0)
    ));
    // The rotate handle stands above the layer's own top edge.
    assert!(close(
        turned.handle_position(Handle::Rotate, 20.0),
        Point::new(25.0, 0.0)
    ));
}

#[test]
fn hit_testing_prefers_handles_of_the_selection_then_the_topmost_body() {
    let frame = |placement: LayerPlacement| placement.frame();
    let back = HitLayer {
        id: layer_id(1),
        frame: frame(LayerPlacement::new(0, 0, 400, 400, 0)),
        selected: true,
    };
    let front = HitLayer {
        id: layer_id(2),
        frame: frame(LayerPlacement::new(100, 100, 100, 100, 0)),
        selected: false,
    };
    let layers = [back, front];
    assert_eq!(
        hit_test(&layers, Point::new(150.0, 150.0), 8.0, 24.0),
        Some(HitTarget::Body(layer_id(2)))
    );
    assert_eq!(
        hit_test(&layers, Point::new(398.0, 401.0), 8.0, 24.0),
        Some(HitTarget::Handle(
            layer_id(1),
            Handle::Corner(Corner::BottomRight)
        ))
    );
    assert_eq!(
        hit_test(&layers, Point::new(200.0, -24.0), 8.0, 24.0),
        Some(HitTarget::Handle(layer_id(1), Handle::Rotate))
    );
    assert_eq!(
        hit_test(&layers, Point::new(0.0, 200.0), 8.0, 24.0),
        Some(HitTarget::Handle(layer_id(1), Handle::Edge(Edge::Left)))
    );
    assert_eq!(hit_test(&layers, Point::new(600.0, 600.0), 8.0, 24.0), None);
    // A multi-selection offers no handles.
    let both = [
        back,
        HitLayer {
            selected: true,
            ..front
        },
    ];
    assert_eq!(hit_test(&both, Point::new(398.0, 401.0), 8.0, 24.0), None);
    assert_eq!(
        hit_test(&both, Point::new(390.0, 390.0), 8.0, 24.0),
        Some(HitTarget::Body(layer_id(1)))
    );
}

#[test]
fn hit_testing_follows_a_turned_box() {
    let frame = super::geometry::LayerFrame {
        centre: Point::new(500.0, 500.0),
        half_width: 300.0,
        half_height: 20.0,
        angle_degrees: 45.0,
        anchor: Point::default(),
    };
    let layer = HitLayer {
        id: layer_id(7),
        frame,
        selected: true,
    };
    // Along the diagonal: inside the turned bar though far outside its
    // unturned box vertically.
    assert_eq!(
        hit_test(&[layer], Point::new(600.0, 600.0), 4.0, 24.0),
        Some(HitTarget::Body(layer_id(7)))
    );
    assert_eq!(
        hit_test(&[layer], Point::new(700.0, 500.0), 4.0, 24.0),
        None
    );
    let corner = frame.handle_position(Handle::Corner(Corner::TopRight), 24.0);
    assert_eq!(
        hit_test(&[layer], corner, 4.0, 24.0),
        Some(HitTarget::Handle(
            layer_id(7),
            Handle::Corner(Corner::TopRight)
        ))
    );
}

#[test]
fn crop_insets_keep_a_pixel_and_map_to_the_stored_rectangle() {
    let source = (1_920, 1_080);
    let mut crop = CropInsets::default();
    assert_eq!(crop.rect(source), None);
    crop.set(Edge::Left, 100, source);
    crop.set(Edge::Right, 5_000, source);
    assert_eq!(crop.right, 1_819);
    assert_eq!(crop.rect(source), Some((100, 0, 1, 1_080)));
    crop.set(Edge::Right, 20, source);
    crop.set(Edge::Bottom, 80, source);
    assert_eq!(crop.rect(source), Some((100, 0, 1_800, 1_000)));
    let mask = MaskRect {
        x: 1_700,
        y: 10,
        width: 500,
        height: 20,
        inverted: true,
    }
    .clamped(crop.cropped(source));
    assert_eq!((mask.x, mask.width, mask.height), (1_700, 100, 20));
}

// Snapping.

#[test]
fn safe_areas_keep_their_share_of_the_canvas() {
    let action = safe_area((1_920, 1_080), super::snapping::ACTION_SAFE);
    assert!((action.left - 67.2).abs() < EPSILON);
    assert!((action.width() - 1_785.6).abs() < 1e-6);
    let title = safe_area((1_920, 1_080), TITLE_SAFE);
    assert!((title.top - 54.0).abs() < EPSILON);
    assert!((title.bottom - 1_026.0).abs() < EPSILON);
}

#[test]
fn a_moving_box_snaps_its_nearest_edge_or_centre_and_reports_the_guides() {
    let targets = SnapTargets::new(
        (1_920, 1_080),
        &[Bounds::new(1_000.0, 500.0, 1_200.0, 700.0)],
        SnapSettings::default(),
    );
    // Its centre is 3 px right of the canvas centre and its top 4 px
    // below the title-safe line.
    let moving = Bounds::new(763.0, 58.0, 1_163.0, 258.0);
    let snapped = snap_bounds(moving, &targets, 6.0);
    assert!((snapped.dx + 3.0).abs() < EPSILON);
    assert!((snapped.dy + 4.0).abs() < EPSILON);
    assert!(snapped.guides.iter().any(|guide| {
        guide.kind == GuideKind::CanvasCentre && (guide.position - 960.0).abs() < EPSILON
    }));
    assert!(
        snapped
            .guides
            .iter()
            .any(|guide| guide.kind == GuideKind::TitleSafe)
    );
    // Out of reach: nothing moves.
    let far = snap_bounds(Bounds::new(300.0, 300.0, 310.0, 310.0), &targets, 6.0);
    assert_eq!((far.dx, far.dy), (0.0, 0.0));
    assert_eq!(far.guides.len(), 0);
}

#[test]
fn snapping_to_another_layer_and_switching_families_off() {
    let other = Bounds::new(1_000.0, 500.0, 1_200.0, 700.0);
    let layers_only = SnapSettings {
        canvas: false,
        safe_areas: false,
        ..SnapSettings::default()
    };
    let targets = SnapTargets::new((1_920, 1_080), &[other], layers_only);
    let (point, guides) = snap_point(Point::new(1_204.0, 300.0), &targets, 6.0, (true, true));
    assert!(close(point, Point::new(1_200.0, 300.0)));
    assert_eq!(guides.len(), 1);
    assert_eq!(guides[0].kind, GuideKind::LayerEdge);
    assert!(SnapTargets::new((1_920, 1_080), &[other], no_snapping()).is_empty());
}

// Gestures.

#[test]
fn dragging_a_body_moves_the_whole_selection_as_one_undo_step() {
    let mut document = document();
    document.select(layer_id(3), false);
    document.select(layer_id(2), true);
    let started = document.press(Point::new(150.0, 150.0), NONE, METRICS, no_snapping());
    assert_eq!(started, Some(GestureKind::Move));
    document.drag(Point::new(160.0, 155.0), NONE);
    let feedback = document.drag(Point::new(175.0, 170.0), NONE);
    assert_eq!(feedback.readout, Some(Readout::Position { x: 125, y: 120 }));
    assert!(document.release());
    assert_eq!(document.layer(layer_id(3)).unwrap().placement.x, 125);
    assert_eq!(document.layer(layer_id(2)).unwrap().placement.x, 1_385);
    assert_eq!(document.layer(layer_id(2)).unwrap().placement.y, 760);
    assert_eq!(document.undo_label(), Some(EditLabel::Move));
    assert!(document.undo());
    assert!(!document.is_dirty());
    assert!(document.redo());
    assert_eq!(document.layer(layer_id(3)).unwrap().placement.y, 120);
}

#[test]
fn a_move_snaps_to_the_canvas_centre_unless_shift_is_held() {
    let mut document = document();
    // The logo's centre starts at x 200; drag it to 957.
    document.press(
        Point::new(200.0, 150.0),
        NONE,
        METRICS,
        SnapSettings::default(),
    );
    let feedback = document.drag(Point::new(957.0, 150.0), NONE);
    assert!(
        feedback
            .guides
            .iter()
            .any(|guide| guide.kind == GuideKind::CanvasCentre)
    );
    document.release();
    assert_eq!(document.layer(layer_id(3)).unwrap().placement.x, 860);
    document.press(
        Point::new(960.0, 150.0),
        NONE,
        METRICS,
        SnapSettings::default(),
    );
    let free = document.drag(Point::new(963.0, 150.0), SHIFT);
    assert_eq!(free.guides.len(), 0);
    document.release();
    assert_eq!(document.layer(layer_id(3)).unwrap().placement.x, 863);
}

#[test]
fn a_click_on_empty_canvas_clears_the_selection_and_a_click_selects_alone() {
    let mut document = document();
    document.select_all();
    // A click without travel on one of several selected layers selects it.
    document.press(Point::new(150.0, 150.0), NONE, METRICS, no_snapping());
    assert!(!document.release());
    assert_eq!(document.selection(), &[layer_id(3)]);
    document.press(Point::new(-50.0, -50.0), NONE, METRICS, no_snapping());
    assert_eq!(document.selection().len(), 0);
}

#[test]
fn a_corner_scales_from_the_opposite_corner_and_shift_keeps_the_aspect() {
    let mut document = document();
    document.select(layer_id(2), false);
    // Bottom-right handle of the PiP at (1840, 1010).
    let kind = document.press(Point::new(1_840.0, 1_010.0), NONE, METRICS, no_snapping());
    assert_eq!(
        kind,
        Some(GestureKind::Scale(Handle::Corner(Corner::BottomRight)))
    );
    document.drag(Point::new(1_600.0, 1_000.0), NONE);
    let placement = document.layer(layer_id(2)).unwrap().placement;
    assert_eq!(
        (placement.x, placement.y, placement.width, placement.height),
        (1_360, 740, 240, 260)
    );
    let feedback = document.drag(Point::new(1_600.0, 1_000.0), SHIFT);
    let placement = document.layer(layer_id(2)).unwrap().placement;
    // The larger of the two ratios (260/270) wins; 16:9 is kept.
    assert_eq!((placement.width, placement.height), (462, 260));
    assert_eq!((placement.x, placement.y), (1_360, 740));
    assert_eq!(
        feedback.readout,
        Some(Readout::Size {
            width: 462,
            height: 260
        })
    );
    assert!(document.release());
    assert_eq!(document.undo_label(), Some(EditLabel::Scale));
}

#[test]
fn an_edge_scales_one_side_and_never_flips() {
    let mut document = document();
    document.select(layer_id(3), false);
    // Left edge midpoint at (100, 150).
    document.press(Point::new(100.0, 150.0), NONE, METRICS, no_snapping());
    document.drag(Point::new(50.0, 400.0), NONE);
    let placement = document.layer(layer_id(3)).unwrap().placement;
    assert_eq!(
        (placement.x, placement.y, placement.width, placement.height),
        (50, 100, 250, 100)
    );
    document.drag(Point::new(900.0, 150.0), NONE);
    let placement = document.layer(layer_id(3)).unwrap().placement;
    assert_eq!((placement.x, placement.width), (299, 1));
    document.release();
}

#[test]
fn scaling_a_turned_layer_works_in_its_own_axes() {
    let mut turned = layout();
    turned.layers[2].quarter_turns = 1;
    let mut document = SceneLayoutDocument::new(scene(), &turned);
    document.select(layer_id(3), false);
    // A quarter-turned 200x100 layer at (100,100) covers 100x200 on the
    // canvas; its own right edge is the footprint's bottom edge.
    let bottom = Point::new(150.0, 300.0);
    assert_eq!(
        document.press(bottom, NONE, METRICS, no_snapping()),
        Some(GestureKind::Scale(Handle::Edge(Edge::Right)))
    );
    document.drag(Point::new(150.0, 400.0), NONE);
    document.release();
    let placement = document.layer(layer_id(3)).unwrap().placement;
    assert_eq!((placement.width, placement.height), (300, 100));
    assert_eq!(
        placement.footprint(),
        Bounds::new(100.0, 100.0, 200.0, 400.0)
    );
}

#[test]
fn rotating_steps_by_fifteen_degrees_with_shift_and_stores_quarter_turns() {
    assert!((rotation_angle(52.0, true) - 45.0).abs() < EPSILON);
    assert!((rotation_angle(-7.0, false) - 353.0).abs() < EPSILON);
    assert!((rotation_angle(-3.0, true)).abs() < EPSILON);
    assert_eq!(quarter_turns(44.0), 0);
    assert_eq!(quarter_turns(46.0), 1);
    assert_eq!(quarter_turns(-80.0), 3);
    let mut document = document();
    document.select(layer_id(3), false);
    // The logo's centre is (200, 150); the rotate handle stands 24 px
    // above its top edge at (200, 76).
    let kind = document.press(Point::new(200.0, 76.0), NONE, METRICS, no_snapping());
    assert_eq!(kind, Some(GestureKind::Rotate));
    // A quarter turn clockwise: the pointer to the right of the centre.
    let feedback = document.drag(Point::new(290.0, 148.0), SHIFT);
    let Some(Readout::Angle { free, stored }) = feedback.readout else {
        panic!("no angle readout");
    };
    assert!((free - 90.0).abs() < EPSILON);
    assert!((stored - 90.0).abs() < EPSILON);
    document.release();
    let placement = document.layer(layer_id(3)).unwrap().placement;
    assert_eq!(placement.quarter_turns, 1);
    // Turned about its centre: 100x200 footprint around (200, 150).
    assert_eq!(
        placement.footprint(),
        Bounds::new(150.0, 50.0, 250.0, 250.0)
    );
}

#[test]
fn alt_on_an_edge_crops_in_source_pixels_without_moving_the_box() {
    let mut document = document();
    document.select(layer_id(2), false);
    // Left edge midpoint of the PiP at (1360, 875); the 480 px box shows
    // the whole 1920 px source, so one box pixel is four source pixels.
    let kind = document.press(Point::new(1_360.0, 875.0), ALT, METRICS, no_snapping());
    assert_eq!(kind, Some(GestureKind::Crop(Edge::Left)));
    let feedback = document.drag(Point::new(1_400.0, 875.0), ALT);
    assert_eq!(
        feedback.readout,
        Some(Readout::Crop {
            edge: Edge::Left,
            pixels: 160
        })
    );
    document.release();
    let layer = document.layer(layer_id(2)).unwrap();
    assert_eq!(layer.crop.left, 160);
    assert_eq!(layer.placement.x, 1_360);
    assert_eq!(document.undo_label(), Some(EditLabel::Crop));
}

#[test]
fn escape_cancels_a_gesture_without_a_step() {
    let mut document = document();
    document.press(Point::new(150.0, 150.0), NONE, METRICS, no_snapping());
    document.drag(Point::new(400.0, 400.0), NONE);
    document.cancel();
    assert!(!document.is_dirty());
    assert_eq!(document.undo_label(), None);
    assert_eq!(document.gesture_kind(), None);
}

// Fields, order, and commands.

#[test]
fn numeric_fields_clamp_and_scrubs_join_one_step() {
    let mut document = document();
    document.select(layer_id(2), false);
    document.begin(EditLabel::Field(Field::X));
    for x in [1_300.0, 1_250.0, 1_200.0] {
        document.set_field(Field::X, x);
    }
    assert!(document.commit());
    assert_eq!(document.layer(layer_id(2)).unwrap().placement.x, 1_200);
    assert!(document.undo());
    assert_eq!(document.layer(layer_id(2)).unwrap().placement.x, 1_360);
    assert!(document.redo());
    document.set_field(Field::Width, 0.0);
    assert_eq!(document.layer(layer_id(2)).unwrap().placement.width, 1);
    document.set_field(Field::Opacity, 400.0);
    assert_eq!(document.layer(layer_id(2)).unwrap().opacity, 255);
    document.set_field(Field::Opacity, 128.0);
    assert_eq!(
        document.field_value(layer_id(2), Field::Opacity),
        Some(128.0)
    );
    // Any angle: the nearest quarter turn and a tilt, to the millidegree.
    document.set_field(Field::Rotation, 260.0);
    let placement = document.layer(layer_id(2)).unwrap().placement;
    assert_eq!(
        (placement.quarter_turns, placement.tilt_millidegrees),
        (3, -10_000)
    );
    assert_eq!(
        document.field_value(layer_id(2), Field::Rotation),
        Some(260.0)
    );
    document.set_field(Field::Rotation, -0.0015);
    let placement = document.layer(layer_id(2)).unwrap().placement;
    assert_eq!(
        (placement.quarter_turns, placement.tilt_millidegrees),
        (0, -2)
    );
}

#[test]
fn zoom_scales_both_sides_about_the_centre() {
    let mut document = document();
    document.select(layer_id(2), false);
    assert_eq!(document.field_value(layer_id(2), Field::Zoom), Some(25.0));
    document.set_field(Field::Zoom, 50.0);
    let placement = document.layer(layer_id(2)).unwrap().placement;
    assert_eq!((placement.width, placement.height), (960, 540));
    assert_eq!(placement.footprint().centre(), Point::new(1_600.0, 875.0));
}

#[test]
fn masks_stay_inside_the_cropped_source() {
    let mut document = document();
    document.select(layer_id(1), false);
    document.set_field(Field::Masked, 1.0);
    let mask = document.layer(layer_id(1)).unwrap().mask.unwrap();
    assert_eq!(
        (mask.x, mask.y, mask.width, mask.height),
        (480, 270, 960, 540)
    );
    // A crop stops at the mask's far edge instead of cutting it.
    document.set_field(Field::CropRight, 1_000.0);
    let layer = document.layer(layer_id(1)).unwrap();
    assert_eq!(layer.crop.cropped((1_920, 1_080)), (1_440, 1_080));
    assert_eq!(layer.mask.unwrap().width, 960);
    // A narrower mask lets the crop go further.
    document.set_field(Field::MaskWidth, 440.0);
    document.set_field(Field::CropRight, 1_000.0);
    let layer = document.layer(layer_id(1)).unwrap();
    assert_eq!(layer.crop.cropped((1_920, 1_080)), (920, 1_080));
    document.set_field(Field::MaskInverted, 1.0);
    assert_eq!(
        document.field_value(layer_id(1), Field::MaskInverted),
        Some(1.0)
    );
    document.set_field(Field::Masked, 0.0);
    assert_eq!(document.layer(layer_id(1)).unwrap().mask, None);
}

#[test]
fn stacking_moves_keep_the_selection_order() {
    let mut document = document();
    document.select(layer_id(1), false);
    assert!(document.can_shift_selection(true));
    assert!(!document.can_shift_selection(false));
    assert!(document.shift_selection(true));
    let order = |document: &SceneLayoutDocument| {
        document
            .layers()
            .iter()
            .map(|layer| layer.id.get().get())
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&document), [2, 1, 3]);
    assert!(document.send_selection_to_end(true));
    assert_eq!(order(&document), [2, 3, 1]);
    assert!(!document.can_shift_selection(true));
    assert!(document.move_layer(layer_id(1), 0));
    assert_eq!(order(&document), [1, 2, 3]);
    assert_eq!(document.undo_label(), Some(EditLabel::Reorder));
    assert!(!document.is_dirty());
}

#[test]
fn the_draft_becomes_the_existing_layer_commands_in_a_valid_order() {
    let mut document = document();
    document.select(layer_id(1), false);
    document.send_selection_to_end(true);
    document.select(layer_id(2), false);
    document.set_field(Field::X, 1_000.0);
    document.set_field(Field::Opacity, 200.0);
    document.set_field(Field::CropTop, 40.0);
    document.set_field(Field::Masked, 1.0);
    document.set_field(Field::MaskInverted, 1.0);
    let edits = document.edits();
    assert_eq!(
        edits,
        vec![
            SceneLayerEdit::Move { from: 1, to: 0 },
            SceneLayerEdit::Move { from: 2, to: 1 },
            SceneLayerEdit::Geometry {
                index: 0,
                placement: LayerPlacement::new(1_000, 740, 480, 270, 0),
            },
            SceneLayerEdit::Appearance {
                index: 0,
                enabled: true,
                opacity: 200,
            },
            SceneLayerEdit::Crop {
                index: 0,
                rect: Some((0, 40, 1_920, 1_040)),
            },
            SceneLayerEdit::Mask {
                index: 0,
                mask: Some(MaskRect {
                    x: 480,
                    y: 260,
                    width: 960,
                    height: 520,
                    inverted: true,
                }),
            },
        ]
    );
    let script = document.cli_script("my show.freemix");
    let lines: Vec<&str> = script.lines().collect();
    assert_eq!(
        lines,
        [
            "freemix-cli scene-layer-move 'my show.freemix' 100 1 0",
            "freemix-cli scene-layer-move 'my show.freemix' 100 2 1",
            "freemix-cli scene-layer-geometry 'my show.freemix' 100 0 1000 740 480 270 0",
            "freemix-cli scene-layer-appearance 'my show.freemix' 100 0 on 200",
            "freemix-cli scene-layer-crop 'my show.freemix' 100 0 0 40 1920 1040",
            "freemix-cli scene-layer-mask 'my show.freemix' 100 0 480 260 960 520 inverted",
        ]
    );
}

#[test]
fn a_crop_cleared_and_a_disabled_layer_use_their_clear_and_off_forms() {
    let mut document = document();
    document.select(layer_id(3), false);
    document.set_field(Field::Enabled, 0.0);
    document.set_field(Field::CropLeft, 10.0);
    document.set_field(Field::CropLeft, 0.0);
    document.set_field(Field::Rotation, 180.0);
    assert_eq!(
        document.cli_script("show.freemix"),
        "freemix-cli scene-layer-geometry show.freemix 100 2 100 100 200 100 180\n\
         freemix-cli scene-layer-appearance show.freemix 100 2 off 255"
    );
    assert_eq!(
        SceneLayerEdit::Crop {
            index: 4,
            rect: None
        }
        .cli_command("show.freemix", scene()),
        "freemix-cli scene-layer-crop-clear show.freemix 100 4"
    );
}

#[test]
fn a_new_server_layout_replaces_a_clean_draft_and_flags_a_dirty_one() {
    let mut document = document();
    let mut moved = layout();
    moved.layers[2].x = 500;
    document.rebase(&moved);
    assert_eq!(document.layer(layer_id(3)).unwrap().placement.x, 500);
    assert!(!document.base_changed());
    document.select(layer_id(2), false);
    document.set_field(Field::Y, 10.0);
    moved.layers.remove(1);
    document.rebase(&moved);
    assert!(document.base_changed());
    assert!(document.is_dirty());
    // The removed layer's edits no longer apply.
    assert_eq!(document.edits().len(), 0);
    document.revert();
    assert!(!document.is_dirty());
    assert_eq!(document.selection().len(), 0);
}

#[test]
fn nudges_move_the_selection_by_whole_pixels() {
    let mut document = document();
    assert!(!document.nudge(1, 0));
    document.select(layer_id(3), false);
    assert!(document.nudge(-10, 1));
    let placement = document.layer(layer_id(3)).unwrap().placement;
    assert_eq!((placement.x, placement.y), (90, 101));
    assert_eq!(document.undo_label(), Some(EditLabel::Nudge));
}

// Previews.

#[test]
fn previews_are_paced_held_during_gestures_and_refreshed() {
    let subject = PreviewSubject {
        input: input(7),
        layout_stamp: layout_stamp(&layout()),
        size: (640, 360),
    };
    let mut pacer = PreviewPacer::default();
    let start = Duration::from_secs(10);
    assert!(pacer.poll(Some(subject), start, true, true).is_none());
    assert!(pacer.poll(Some(subject), start, false, false).is_none());
    let first = pacer.poll(Some(subject), start, false, true).unwrap();
    assert_eq!(first.request_id, "design-preview-1");
    assert_eq!(first.sample, TitlePreviewSample::Static);
    assert!(first.clip.is_none());
    assert!(PreviewPacer::owns(&first.request_id));
    // One in flight at a time.
    assert!(
        pacer
            .poll(Some(subject), start + PREVIEW_MIN_INTERVAL, false, true)
            .is_none()
    );
    assert!(!pacer.received("anim-preview-3"));
    assert!(pacer.received(&first.request_id));
    // Shown and fresh: nothing until the refresh period passes.
    let later = start + PREVIEW_MIN_INTERVAL;
    assert!(pacer.poll(Some(subject), later, false, true).is_none());
    // A layout change asks at once.
    let changed = PreviewSubject {
        layout_stamp: subject.layout_stamp + 1,
        ..subject
    };
    let second = pacer.poll(Some(changed), later, false, true).unwrap();
    // A lost answer is given up after the timeout.
    let lost = later + PREVIEW_TIMEOUT;
    let third = pacer.poll(Some(changed), lost, false, true).unwrap();
    assert_ne!(second.request_id, third.request_id);
    pacer.failed(&third.request_id, lost);
    assert!(
        pacer
            .poll(Some(changed), lost + PREVIEW_MIN_INTERVAL, false, true)
            .is_none()
    );
    assert!(
        pacer
            .poll(Some(changed), lost + PREVIEW_REFRESH, false, true)
            .is_some()
    );
}

/// SW-008, Protocol 2.98: the selected layer's input picture is asked for
/// once per input, kept, refreshed, retried after a refusal only once the
/// refresh passes, and never while the designer's own preview flies.
#[test]
fn key_pictures_are_paced_per_input_and_kept() {
    use super::preview::{KEY_PICTURE_REFRESH, KeyPicturePacer};
    use fm_protocol::{TitlePreviewImage, WireInputId};

    let image = |request_id: &str, input_id: u128| TitlePreviewImage {
        request_id: request_id.into(),
        input: WireInputId::from_domain(input(input_id)),
        animation_revision: 0,
        draft: false,
        width: 2,
        height: 1,
        rgba: vec![0; 8].into(),
    };
    let mut pacer = KeyPicturePacer::default();
    let start = Duration::from_secs(10);
    assert!(pacer.poll(None, (640, 360), start, true).is_none());
    assert!(
        pacer
            .poll(Some(input(7)), (640, 360), start, false)
            .is_none()
    );
    let first = pacer.poll(Some(input(7)), (640, 360), start, true).unwrap();
    assert_eq!(first.request_id, "design-key-picture-1");
    assert_eq!(first.input, WireInputId::from_domain(input(7)));
    assert_eq!(
        (first.width, first.height, first.clip.is_none()),
        (640, 360, true)
    );
    assert!(KeyPicturePacer::owns(&first.request_id));
    assert!(!PreviewPacer::owns(&first.request_id));
    assert!(pacer.in_flight());
    // A designer preview is not this pacer's.
    assert!(!pacer.received(&image("design-preview-4", 7)));
    assert!(pacer.picture(input(7)).is_none());
    assert!(pacer.received(&image(&first.request_id, 7)));
    assert_eq!(pacer.picture(input(7)).unwrap().width, 2);
    assert!(pacer.picture(input(8)).is_none());

    // Fresh: nothing until the refresh passes; another input asks at once.
    let later = start + PREVIEW_MIN_INTERVAL;
    assert!(
        pacer
            .poll(Some(input(7)), (640, 360), later, true)
            .is_none()
    );
    let other = pacer.poll(Some(input(8)), (640, 360), later, true).unwrap();
    pacer.failed(&other.request_id, later);
    assert!(!pacer.in_flight());
    // The older picture stays until another arrives.
    assert!(pacer.picture(input(7)).is_some());
    let soon = later + PREVIEW_MIN_INTERVAL;
    assert!(pacer.poll(Some(input(8)), (640, 360), soon, true).is_none());
    let retry = pacer
        .poll(
            Some(input(8)),
            (640, 360),
            later + KEY_PICTURE_REFRESH,
            true,
        )
        .unwrap();
    // A lost answer is given up after the timeout.
    let lost = later + KEY_PICTURE_REFRESH + PREVIEW_TIMEOUT;
    let again = pacer.poll(Some(input(8)), (640, 360), lost, true).unwrap();
    assert_ne!(retry.request_id, again.request_id);
    pacer.abandon();
    assert!(!pacer.in_flight());
}

#[test]
fn the_layout_stamp_follows_every_layout_change() {
    let base = layout();
    let mut changed = layout();
    assert_eq!(layout_stamp(&base), layout_stamp(&changed));
    changed.layers[0].opacity = 10;
    assert_ne!(layout_stamp(&base), layout_stamp(&changed));
}
