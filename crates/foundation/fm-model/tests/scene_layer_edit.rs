//! SW-006 live scene layer edits on the model: free rotation (quarter turns
//! plus a stored tilt) and the anchor, how drawing resolves them and how
//! animation posing composes with them, and every layer change with its
//! exact inverse.

use std::num::NonZeroU128;

use fm_model::{
    ChromaLayerKey, CropRect, Layer, LayerAnchor, LayerEffect, LayerEffectError, LayerGeometry,
    LayerId, LayerKey, LayerKeyError, LayerPose, LumaLayerKey, RectMask, Rotation, Scene,
    SceneAnimation, SceneLayerChange, SceneLayerEditError, SourceRef, apply_pose,
    check_scene_layer, fm_animation::ClipState, resolve_anchor,
};
use fm_types::{InputId, SceneId};

const CANVAS: (u32, u32) = (1_920, 1_080);

fn layer_id(value: u32) -> LayerId {
    LayerId::new(value).unwrap()
}

fn layer(id: u32, geometry: LayerGeometry) -> Layer {
    Layer {
        id: layer_id(id),
        name: format!("Layer {id}"),
        source: SourceRef::Input(InputId::new(NonZeroU128::new(u128::from(id)).unwrap())),
        enabled: true,
        geometry,
        crop: None,
        mask: None,
        key: None,
        effects: Vec::new(),
        opacity: 255,
        z_order: 0,
    }
}

fn scene() -> Scene {
    Scene {
        id: SceneId::new(NonZeroU128::new(9).unwrap()),
        name: "Studio".into(),
        background: fm_model::Rgba8::OPAQUE_BLACK,
        layers: vec![
            layer(1, LayerGeometry::new(0, 0, 1_920, 1_080, Rotation::Deg0)),
            layer(2, LayerGeometry::new(100, 50, 400, 200, Rotation::Deg0)),
            layer(3, LayerGeometry::new(1_400, 700, 320, 180, Rotation::Deg90)),
        ],
        animation: SceneAnimation::default(),
    }
}

#[test]
fn any_angle_is_the_nearest_quarter_turn_and_a_stored_tilt() {
    let base = LayerGeometry::new(0, 0, 10, 10, Rotation::Deg0);
    for (angle, rotation, tilt) in [
        (0, Rotation::Deg0, 0),
        (12_500, Rotation::Deg0, 12_500),
        (-12_500, Rotation::Deg0, -12_500),
        (44_999, Rotation::Deg0, 44_999),
        // Half up: 45 degrees is the next quarter turn turned back.
        (45_000, Rotation::Deg90, -45_000),
        (100_000, Rotation::Deg90, 10_000),
        (180_000, Rotation::Deg180, 0),
        (315_000, Rotation::Deg0, -45_000),
        (-90_000, Rotation::Deg270, 0),
        (720_001, Rotation::Deg0, 1),
    ] {
        let geometry = base.with_rotation_millidegrees(angle);
        assert_eq!(
            (geometry.rotation, geometry.tilt_millidegrees),
            (rotation, tilt),
            "{angle}"
        );
        assert!(geometry.is_storable(), "{angle}");
        assert_eq!(
            geometry.rotation_millidegrees(),
            angle.rem_euclid(360_000),
            "{angle}"
        );
    }
    // The stored tilt and the anchor are bounded.
    assert!(!base.with_tilt(45_000).is_storable());
    assert!(!base.with_tilt(-45_001).is_storable());
    assert!(
        !base
            .with_anchor(LayerAnchor::new(LayerAnchor::MAX + 1, 0))
            .is_storable()
    );
    assert!(
        base.with_anchor(LayerAnchor::new(0, LayerAnchor::MAX))
            .is_storable()
    );
}

#[test]
fn drawing_turns_the_tilt_about_the_anchor() {
    // A centred anchor or no tilt draws as stored.
    let plain = LayerGeometry::new(100, 50, 400, 200, Rotation::Deg0).with_tilt(30_000);
    assert_eq!(resolve_anchor(plain), plain);
    let untilted =
        LayerGeometry::new(100, 50, 400, 200, Rotation::Deg0).with_anchor(LayerAnchor::new(0, 0));
    assert_eq!(
        resolve_anchor(untilted),
        LayerGeometry::new(100, 50, 400, 200, Rotation::Deg0)
    );
    // Turned 30 degrees about its top-left corner, which stays at (100,
    // 50): the box centre lands at (223.2, 236.6), so the footprint the
    // compositor turns about its centre moves by (-77, 87).
    let pinned = untilted.with_tilt(30_000);
    assert_eq!(
        resolve_anchor(pinned),
        LayerGeometry::new(23, 137, 400, 200, Rotation::Deg0).with_tilt(30_000)
    );
    // The scene as drawn resolves every anchor, static or not.
    let mut scene = scene();
    scene.layers[1].geometry = pinned;
    let drawn = scene.posed(ClipState::Static, CANVAS.0, CANVAS.1);
    assert_eq!(drawn.layers[1].geometry, resolve_anchor(pinned));
    assert_eq!(drawn.layers[0].geometry, scene.layers[0].geometry);
}

#[test]
fn animation_turns_and_scales_about_the_anchor_on_top_of_the_stored_tilt() {
    let anchored = LayerGeometry::new(100, 50, 400, 200, Rotation::Deg0)
        .with_anchor(LayerAnchor::new(0, 0))
        .with_tilt(10_000);
    let animated = layer(2, anchored);
    // An animated 20 degrees adds to the stored 10: drawn as a stored 30.
    let turned = apply_pose(
        &animated,
        LayerPose {
            rotation: 20_000,
            ..LayerPose::IDENTITY
        },
        CANVAS.0,
        CANVAS.1,
    );
    assert_eq!(turned.geometry, resolve_anchor(anchored.with_tilt(30_000)));
    // The identity pose draws the stored geometry.
    let still = apply_pose(&animated, LayerPose::IDENTITY, CANVAS.0, CANVAS.1);
    assert_eq!(still.geometry, resolve_anchor(anchored));
    // Doubling the size about the top-left anchor keeps that corner.
    let corner = layer(
        2,
        LayerGeometry::new(100, 50, 400, 200, Rotation::Deg0).with_anchor(LayerAnchor::new(0, 0)),
    );
    let scaled = apply_pose(
        &corner,
        LayerPose {
            scale_x: 20_000,
            scale_y: 20_000,
            ..LayerPose::IDENTITY
        },
        CANVAS.0,
        CANVAS.1,
    );
    assert_eq!(
        scaled.geometry,
        LayerGeometry::new(100, 50, 800, 400, Rotation::Deg0)
    );
}

/// Applies `change` to layer `id`, checks it, then checks that its inverse
/// restores the scene exactly.
fn round_trip(id: u32, change: &SceneLayerChange) -> Scene {
    let before = scene();
    let mut after = before.clone();
    let inverse = after
        .layer_change_inverse(layer_id(id), change)
        .expect("an edit of an existing layer has an inverse");
    after
        .apply_layer_change(layer_id(id), change, CANVAS.0, CANVAS.1)
        .unwrap();
    assert_ne!(after, before, "{change:?} changed nothing");
    let mut restored = after.clone();
    restored
        .apply_layer_change(layer_id(id), &inverse, CANVAS.0, CANVAS.1)
        .unwrap();
    assert_eq!(restored, before, "{change:?} then {inverse:?}");
    after
}

#[test]
fn every_layer_change_applies_and_its_inverse_restores_the_scene() {
    let geometry = LayerGeometry::new(-40, 30, 640, 360, Rotation::Deg270)
        .with_tilt(-12_345)
        .with_anchor(LayerAnchor::new(2_500, 10_000));
    let after = round_trip(2, &SceneLayerChange::Geometry(geometry));
    assert_eq!(after.layers[1].geometry, geometry);
    let after = round_trip(
        2,
        &SceneLayerChange::Appearance {
            enabled: false,
            opacity: 17,
        },
    );
    assert_eq!(
        (after.layers[1].enabled, after.layers[1].opacity),
        (false, 17)
    );
    let crop = CropRect::new(100, 40, 1_600, 1_000);
    let after = round_trip(2, &SceneLayerChange::Crop(Some(crop)));
    assert_eq!(after.layers[1].crop, Some(crop));
    let mask = RectMask {
        x: 10,
        y: 20,
        width: 300,
        height: 200,
        invert: true,
    };
    let after = round_trip(2, &SceneLayerChange::Mask(Some(mask)));
    assert_eq!(after.layers[1].mask, Some(mask));
    // IN-020: keys and effect lists set, clear, and undo exactly.
    let key = LayerKey::Chroma(ChromaLayerKey::GREEN);
    let after = round_trip(2, &SceneLayerChange::Key(Some(key)));
    assert_eq!(after.layers[1].key, Some(key));
    let effects = vec![
        LayerEffect::new("levels", vec![16, 235]),
        LayerEffect::new("flip", vec![1, 0]),
    ];
    let after = round_trip(2, &SceneLayerChange::Effects(effects.clone()));
    assert_eq!(after.layers[1].effects, effects);
    let mut keyed = scene();
    keyed.layers[1].key = Some(key);
    keyed.layers[1].effects.clone_from(&effects);
    for change in [
        SceneLayerChange::Key(None),
        SceneLayerChange::Effects(Vec::new()),
    ] {
        let inverse = keyed.layer_change_inverse(layer_id(2), &change).unwrap();
        let mut edited = keyed.clone();
        edited
            .apply_layer_change(layer_id(2), &change, CANVAS.0, CANVAS.1)
            .unwrap();
        assert_ne!(edited, keyed);
        edited
            .apply_layer_change(layer_id(2), &inverse, CANVAS.0, CANVAS.1)
            .unwrap();
        assert_eq!(edited, keyed);
    }
    let after = round_trip(2, &SceneLayerChange::ZOrder(-7));
    assert_eq!(after.layers[1].z_order, -7);
    let after = round_trip(1, &SceneLayerChange::Move { index: 2 });
    let order: Vec<u32> = after.layers.iter().map(|layer| layer.id.get()).collect();
    assert_eq!(order, [2, 3, 1]);
    let mut added = layer(
        8,
        LayerGeometry::new(5, 6, 70, 80, Rotation::Deg180).with_tilt(1),
    );
    added.effects = vec![LayerEffect::new("sharpen", vec![300])];
    added.key = Some(LayerKey::Luma(LumaLayerKey::BLACK));
    added.crop = Some(CropRect::new(0, 0, 960, 540));
    let after = round_trip(
        8,
        &SceneLayerChange::Add {
            index: 1,
            layer: Box::new(added.clone()),
        },
    );
    assert_eq!(after.layers[1], added);
    // A removal's inverse adds the whole layer back where it was.
    let mut removed = scene();
    removed.layers[1].effects = vec![LayerEffect::new("saturation", vec![-100])];
    removed.layers[1].key = Some(LayerKey::Chroma(ChromaLayerKey::GREEN));
    let inverse = removed
        .layer_change_inverse(layer_id(2), &SceneLayerChange::Remove)
        .unwrap();
    assert_eq!(
        inverse,
        SceneLayerChange::Add {
            index: 1,
            layer: Box::new(removed.layers[1].clone()),
        }
    );
    let before = removed.clone();
    removed
        .apply_layer_change(layer_id(2), &SceneLayerChange::Remove, CANVAS.0, CANVAS.1)
        .unwrap();
    assert_eq!(removed.layers.len(), 2);
    removed
        .apply_layer_change(layer_id(2), &inverse, CANVAS.0, CANVAS.1)
        .unwrap();
    assert_eq!(removed, before);
}

#[allow(clippy::too_many_lines)] // one table of every refused change
#[test]
fn a_refused_change_leaves_the_scene_untouched() {
    let refusals = [
        (
            9,
            SceneLayerChange::ZOrder(1),
            SceneLayerEditError::UnknownLayer(layer_id(9)),
        ),
        (
            2,
            SceneLayerChange::Geometry(
                LayerGeometry::new(0, 0, 10, 10, Rotation::Deg0).with_tilt(45_000),
            ),
            SceneLayerEditError::InvalidGeometry,
        ),
        (
            2,
            SceneLayerChange::Geometry(LayerGeometry::new(0, 0, 0, 10, Rotation::Deg0)),
            SceneLayerEditError::InvalidGeometry,
        ),
        (
            2,
            SceneLayerChange::Crop(Some(CropRect::new(1_900, 0, 40, 10))),
            SceneLayerEditError::InvalidCrop,
        ),
        (
            2,
            SceneLayerChange::Mask(Some(RectMask {
                x: 0,
                y: 0,
                width: 1_921,
                height: 1,
                invert: false,
            })),
            SceneLayerEditError::InvalidMask,
        ),
        (
            2,
            SceneLayerChange::Key(Some(LayerKey::Luma(LumaLayerKey {
                low: 100,
                high: 50,
                softness: 0,
                invert: false,
            }))),
            SceneLayerEditError::InvalidKey(LayerKeyError::EmptyLumaBand),
        ),
        (
            2,
            SceneLayerChange::Effects(vec![LayerEffect::new("blur", vec![3])]),
            SceneLayerEditError::InvalidEffect {
                effect_index: 0,
                reason: LayerEffectError::UnknownEffect,
            },
        ),
        (
            2,
            SceneLayerChange::Effects(vec![
                LayerEffect::new("flip", vec![0, 0]);
                LayerEffect::MAX_EFFECTS_PER_LAYER + 1
            ]),
            SceneLayerEditError::TooManyEffects,
        ),
        (
            2,
            SceneLayerChange::Move { index: 3 },
            SceneLayerEditError::IndexOutOfRange {
                index: 3,
                length: 3,
            },
        ),
        (
            2,
            SceneLayerChange::Add {
                index: 0,
                layer: Box::new(layer(2, LayerGeometry::new(0, 0, 1, 1, Rotation::Deg0))),
            },
            SceneLayerEditError::DuplicateLayer(layer_id(2)),
        ),
    ];
    for (id, change, error) in refusals {
        let mut edited = scene();
        assert_eq!(
            edited.apply_layer_change(layer_id(id), &change, CANVAS.0, CANVAS.1),
            Err(error),
            "{change:?}"
        );
        assert_eq!(edited, scene());
    }
    // A crop that would leave the mask outside it is refused.
    let mut masked = scene();
    masked.layers[1].mask = Some(RectMask {
        x: 1_000,
        y: 0,
        width: 100,
        height: 100,
        invert: false,
    });
    let before = masked.clone();
    assert_eq!(
        masked.apply_layer_change(
            layer_id(2),
            &SceneLayerChange::Crop(Some(CropRect::new(0, 0, 960, 540))),
            CANVAS.0,
            CANVAS.1,
        ),
        Err(SceneLayerEditError::CropWouldInvalidateMask)
    );
    assert_eq!(masked, before);
    // A blank added layer is refused by the whole-layer check.
    let mut blank = layer(5, LayerGeometry::new(0, 0, 1, 1, Rotation::Deg0));
    blank.name = "  ".into();
    assert_eq!(
        check_scene_layer(&blank, CANVAS),
        Err(SceneLayerEditError::EmptyName)
    );
}

/// The compositor draws at most `Scene::MAX_RENDERED_LAYERS` enabled layers
/// of one scene: an add or an enable past it is refused, while disabled
/// layers, disabling, and every other edit stay available.
#[test]
fn enabled_layers_are_capped_at_the_compositor_limit() {
    let full = || {
        let mut full = scene();
        full.layers = (1..=u32::try_from(Scene::MAX_RENDERED_LAYERS).unwrap())
            .map(|id| layer(id, LayerGeometry::new(0, 0, 10, 10, Rotation::Deg0)))
            .collect();
        full
    };
    let extra = |id: u32, enabled: bool| {
        let mut extra = layer(id, LayerGeometry::new(0, 0, 10, 10, Rotation::Deg0));
        extra.enabled = enabled;
        extra
    };
    let next = u32::try_from(Scene::MAX_RENDERED_LAYERS).unwrap() + 1;
    let mut edited = full();
    assert_eq!(edited.enabled_layer_count(), Scene::MAX_RENDERED_LAYERS);
    let refused = edited.apply_layer_change(
        layer_id(next),
        &SceneLayerChange::Add {
            index: 0,
            layer: Box::new(extra(next, true)),
        },
        CANVAS.0,
        CANVAS.1,
    );
    assert_eq!(refused, Err(SceneLayerEditError::TooManyEnabledLayers));
    assert_eq!(refused.unwrap_err().code(), "too_many_enabled_layers");
    assert_eq!(edited, full());
    // A disabled layer may be added, but not enabled.
    edited
        .apply_layer_change(
            layer_id(next),
            &SceneLayerChange::Add {
                index: 0,
                layer: Box::new(extra(next, false)),
            },
            CANVAS.0,
            CANVAS.1,
        )
        .unwrap();
    let enable = SceneLayerChange::Appearance {
        enabled: true,
        opacity: 255,
    };
    let before = edited.clone();
    assert_eq!(
        edited.apply_layer_change(layer_id(next), &enable, CANVAS.0, CANVAS.1),
        Err(SceneLayerEditError::TooManyEnabledLayers)
    );
    assert_eq!(edited, before);
    // An enabled layer's opacity still changes, and disabling another
    // makes room.
    edited
        .apply_layer_change(
            layer_id(1),
            &SceneLayerChange::Appearance {
                enabled: true,
                opacity: 9,
            },
            CANVAS.0,
            CANVAS.1,
        )
        .unwrap();
    edited
        .apply_layer_change(
            layer_id(2),
            &SceneLayerChange::Appearance {
                enabled: false,
                opacity: 255,
            },
            CANVAS.0,
            CANVAS.1,
        )
        .unwrap();
    edited
        .apply_layer_change(layer_id(next), &enable, CANVAS.0, CANVAS.1)
        .unwrap();
    assert_eq!(edited.enabled_layer_count(), Scene::MAX_RENDERED_LAYERS);
}
