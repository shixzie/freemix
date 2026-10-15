//! Scene layer animation (GX-011 stage 5): clips keyed by stable layer ids,
//! scene-only properties, presets and stretches, and exact integer posing.

use fm_model::{
    CropRect, Layer, LayerGeometry, LayerId, LayerPose, RectMask, Rotation, Scene, SceneAnimation,
    SceneAnimationError, SourceRef, apply_pose,
    fm_animation::{
        AnimationPreset, ClipError, ClipPosition, ClipState, Easing, Keyframe, Phase, Property,
        Rgba, Track, TrackKey, Value,
    },
};
use fm_types::{InputId, SceneId};
use std::num::NonZeroU128;

fn layer_id(value: u32) -> LayerId {
    LayerId::new(value).unwrap()
}

fn layer(id: u32, geometry: LayerGeometry) -> Layer {
    Layer {
        id: layer_id(id),
        name: format!("Layer {id}"),
        source: SourceRef::Input(InputId::new(NonZeroU128::new(1).unwrap())),
        enabled: true,
        geometry,
        crop: None,
        mask: None,
        effects: Vec::new(),
        opacity: 200,
        z_order: 0,
    }
}

fn scene() -> Scene {
    Scene {
        id: SceneId::new(NonZeroU128::new(1).unwrap()),
        name: "Scene".into(),
        background: fm_model::Rgba8::OPAQUE_BLACK,
        layers: vec![
            layer(3, LayerGeometry::new(100, 50, 400, 200, Rotation::Deg0)),
            layer(7, LayerGeometry::new(0, 0, 1920, 1080, Rotation::Deg0)),
        ],
        animation: SceneAnimation::default(),
    }
}

#[test]
fn layer_ids_are_allocated_above_the_highest() {
    let mut scene = scene();
    assert_eq!(scene.next_layer_id(), Some(layer_id(8)));
    scene.layers.clear();
    assert_eq!(scene.next_layer_id(), Some(layer_id(1)));
    scene.layers = vec![layer(
        u32::MAX,
        LayerGeometry::new(0, 0, 1, 1, Rotation::Deg0),
    )];
    assert_eq!(scene.next_layer_id(), Some(layer_id(1)));
}

#[test]
fn edits_name_existing_layers_and_scene_properties_only() {
    let mut scene = scene();
    assert_eq!(
        scene.apply_animation_preset(
            layer_id(4),
            Phase::In,
            AnimationPreset::Fade,
            10,
            1920,
            1080
        ),
        Err(SceneAnimationError::UnknownLayer(layer_id(4)))
    );
    let color = Track::new(
        TrackKey::new(layer_id(3), Phase::In, Property::Color),
        vec![Keyframe::new(
            0,
            Value::Color(Rgba::new(1, 2, 3, 4)),
            Easing::Linear,
        )],
    );
    assert_eq!(
        scene.edit_animation(|clip| clip.set_track(color)),
        Err(SceneAnimationError::UnsupportedProperty {
            layer: layer_id(3),
            property: Property::Color
        })
    );
    assert_eq!(scene.animation, SceneAnimation::default());

    assert_eq!(
        scene.apply_animation_preset(
            layer_id(3),
            Phase::In,
            AnimationPreset::SlideFromLeft,
            10,
            1920,
            1080
        ),
        Ok(1)
    );
    assert_eq!(scene.animation.clip.in_frames(), 10);
    // Off the left edge: -(x + width).
    assert_eq!(
        scene.animation.clip.sample(
            layer_id(3),
            Property::PositionX,
            ClipState::In(ClipPosition::frame(0))
        ),
        Some(Value::Scalar(-500))
    );
    assert_eq!(scene.stretch_animation(Phase::In, 20), Ok(2));
    assert_eq!(scene.animation.clip.in_frames(), 20);
    assert_eq!(
        scene.stretch_animation(Phase::In, fm_model::fm_animation::MAX_PHASE_FRAMES + 1),
        Err(SceneAnimationError::Clip(ClipError::PhaseTooLong {
            phase: Phase::In,
            frames: fm_model::fm_animation::MAX_PHASE_FRAMES + 1
        }))
    );
    assert_eq!(scene.animation.revision, 2);
}

#[test]
fn posing_is_identity_when_static_or_holding_identity() {
    let mut scene = scene();
    assert_eq!(scene.posed(ClipState::Hold, 1920, 1080), scene);
    scene
        .apply_animation_preset(layer_id(3), Phase::In, AnimationPreset::Pop, 10, 1920, 1080)
        .unwrap();
    assert_eq!(scene.posed(ClipState::Static, 1920, 1080), scene);
    // An In preset ends at identity: the hold pose is the static design.
    assert_eq!(scene.posed(ClipState::Hold, 1920, 1080), scene);
    // Pop starts at scale zero: the layer is hidden.
    let first = scene.posed(ClipState::In(ClipPosition::frame(0)), 1920, 1080);
    assert!(!first.layers[0].enabled);
    assert_eq!(first.layers[1], scene.layers[1]);
}

#[test]
fn scale_is_about_the_footprint_centre_and_position_adds() {
    let base = layer(3, LayerGeometry::new(100, 50, 400, 200, Rotation::Deg0));
    let pose = LayerPose {
        scale_x: 5_000,
        scale_y: 15_000,
        dx: 7,
        dy: -3,
        ..LayerPose::IDENTITY
    };
    let posed = apply_pose(&base, pose, 1920, 1080);
    // 200x300 centred on (300, 150), then moved.
    assert_eq!(
        posed.geometry,
        LayerGeometry::new(200 + 7, 0 - 3, 200, 300, Rotation::Deg0)
    );
    assert_eq!(posed.crop, None);
    assert_eq!(posed.opacity, 200);
}

#[test]
fn rotation_snaps_to_the_nearest_quarter_turn_about_the_centre() {
    let base = layer(3, LayerGeometry::new(100, 50, 400, 200, Rotation::Deg0));
    let at = |millidegrees| {
        apply_pose(
            &base,
            LayerPose {
                rotation: millidegrees,
                ..LayerPose::IDENTITY
            },
            1920,
            1080,
        )
        .geometry
    };
    assert_eq!(at(44_999), base.geometry);
    // Half up: 45 degrees turns. The 200x400 footprint keeps the centre.
    assert_eq!(
        at(45_000),
        LayerGeometry::new(200, -50, 400, 200, Rotation::Deg90)
    );
    assert_eq!(
        at(-90_000),
        LayerGeometry::new(200, -50, 400, 200, Rotation::Deg270)
    );
    assert_eq!(at(360_000), base.geometry);
}

#[test]
fn crop_hides_box_edges_and_shrinks_the_source_in_proportion() {
    let mut base = layer(3, LayerGeometry::new(100, 50, 400, 200, Rotation::Deg0));
    base.crop = Some(CropRect::new(100, 100, 800, 400));
    base.mask = Some(RectMask::new(0, 0, 100, 400));
    let pose = LayerPose {
        crop: [100, 0, 50, 20],
        opacity: 128,
        ..LayerPose::IDENTITY
    };
    let posed = apply_pose(&base, pose, 1920, 1080);
    assert_eq!(
        posed.geometry,
        LayerGeometry::new(200, 50, 250, 180, Rotation::Deg0)
    );
    // Source is 2 px per box px: 200 left, 100 right, 40 bottom.
    assert_eq!(posed.crop, Some(CropRect::new(300, 100, 500, 360)));
    // The mask covered source x 100..200, now entirely cropped away.
    assert!(!posed.enabled);
    // An inverted mask that no longer covers anything is dropped.
    base.mask = Some(RectMask::new(0, 0, 100, 400).inverted(true));
    let posed = apply_pose(&base, pose, 1920, 1080);
    assert!(posed.enabled);
    assert_eq!(posed.mask, None);
    // 200 * 128 / 255, half up.
    assert_eq!(posed.opacity, 100);
    // A mask overlapping the cropped region is clipped to it.
    base.mask = Some(RectMask::new(150, 10, 300, 500));
    let posed = apply_pose(&base, pose, 1920, 1080);
    assert_eq!(posed.mask, Some(RectMask::new(0, 10, 250, 350)));
}

#[test]
fn rotated_crops_offset_the_footprint_by_the_turned_edges() {
    let base = layer(3, LayerGeometry::new(0, 0, 400, 200, Rotation::Deg90));
    let pose = LayerPose {
        crop: [10, 20, 30, 40],
        ..LayerPose::IDENTITY
    };
    let posed = apply_pose(&base, pose, 1920, 1080);
    // Clockwise: the box's bottom edge lands on the left, its left on top.
    assert_eq!(
        posed.geometry,
        LayerGeometry::new(40, 10, 360, 140, Rotation::Deg90)
    );
    // Wiping the whole box hides the layer.
    let gone = apply_pose(
        &base,
        LayerPose {
            crop: [400, 0, 0, 0],
            ..LayerPose::IDENTITY
        },
        1920,
        1080,
    );
    assert!(!gone.enabled);
}
