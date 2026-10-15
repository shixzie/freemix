//! Wire mapping of the live scene layer edits (`scene_layer_*`) and the
//! `scene_layout_changed` event (Protocol 2.39, SW-006).
//!
//! A wire edit names its scene and stable layer id; the engine edit is the
//! same edit on domain types. Every accepted edit, undo and redo included,
//! publishes the edit applied and the scene's whole layout afterwards, so a
//! client replaces the scene's replicated `scene_layout_state` with it.

use fm_engine::{Engine, EngineCommand, EngineSceneLayerEdit};
use fm_model::{
    CropRect, Layer, LayerAnchor, LayerEffect, LayerGeometry, LayerId, RectMask, Rotation,
    SceneLayerChange, SourceRef,
};
use fm_protocol::{
    EventPayload, SceneLayerEdit, SceneLayerOperation, WireInputId, WireLayerCrop, WireLayerEffect,
    WireLayerGeometry, WireLayerMask, WireSceneId, WireSceneLayerId, WireSceneLayerOutline,
    WireSceneLayerSource, WireSceneLayerSpec,
};

// The wire carries exactly what the model stores and the engine accepts.
const _: () = {
    assert!(fm_protocol::MAX_SCENE_LAYOUT_LAYERS == fm_engine::MAX_SCENE_LAYERS);
    assert!(fm_protocol::MAX_SCENE_LAYER_NAME_BYTES == fm_engine::MAX_SCENE_LAYER_NAME_BYTES);
    assert!(fm_protocol::MAX_SCENE_LAYER_EFFECTS == LayerEffect::MAX_EFFECTS_PER_LAYER);
    assert!(fm_protocol::MAX_SCENE_LAYER_EFFECT_NAME_BYTES == LayerEffect::MAX_NAME_BYTES);
    assert!(fm_protocol::MAX_SCENE_LAYER_EFFECT_PARAMETERS == LayerEffect::MAX_PARAMETERS);
    assert!(fm_protocol::SCENE_LAYER_ANCHOR_MAX == LayerAnchor::MAX);
    assert!(fm_protocol::SCENE_LAYER_ANCHOR_CENTER == LayerAnchor::CENTER.x);
    assert!(fm_protocol::SCENE_LAYER_ANCHOR_CENTER == LayerAnchor::CENTER.y);
    assert!(fm_protocol::MIN_SCENE_LAYER_TILT_MILLIDEGREES == LayerGeometry::MIN_TILT_MILLIDEGREES);
    assert!(fm_protocol::MAX_SCENE_LAYER_TILT_MILLIDEGREES == LayerGeometry::MAX_TILT_MILLIDEGREES);
};

const fn rotation(quarter_turns: u8) -> Rotation {
    match quarter_turns % 4 {
        1 => Rotation::Deg90,
        2 => Rotation::Deg180,
        3 => Rotation::Deg270,
        _ => Rotation::Deg0,
    }
}

/// `rotation` as clockwise quarter turns.
#[must_use]
pub const fn quarter_turns(rotation: Rotation) -> u8 {
    match rotation {
        Rotation::Deg0 => 0,
        Rotation::Deg90 => 1,
        Rotation::Deg180 => 2,
        Rotation::Deg270 => 3,
    }
}

/// The domain geometry a wire geometry names.
#[must_use]
pub const fn domain_geometry(geometry: &WireLayerGeometry) -> LayerGeometry {
    LayerGeometry::new(
        geometry.x,
        geometry.y,
        geometry.width,
        geometry.height,
        rotation(geometry.quarter_turns),
    )
    .with_tilt(geometry.tilt_millidegrees)
    .with_anchor(LayerAnchor::new(geometry.anchor_x, geometry.anchor_y))
}

/// `geometry` on the wire.
#[must_use]
pub const fn wire_geometry(geometry: &LayerGeometry) -> WireLayerGeometry {
    WireLayerGeometry {
        x: geometry.translation_x,
        y: geometry.translation_y,
        width: geometry.width,
        height: geometry.height,
        quarter_turns: quarter_turns(geometry.rotation),
        tilt_millidegrees: geometry.tilt_millidegrees,
        anchor_x: geometry.anchor.x,
        anchor_y: geometry.anchor.y,
    }
}

const fn domain_crop(crop: WireLayerCrop) -> CropRect {
    CropRect::new(crop.x, crop.y, crop.width, crop.height)
}

/// `crop` on the wire.
#[must_use]
pub const fn wire_crop(crop: CropRect) -> WireLayerCrop {
    WireLayerCrop {
        x: crop.x,
        y: crop.y,
        width: crop.width,
        height: crop.height,
    }
}

const fn domain_mask(mask: WireLayerMask) -> RectMask {
    RectMask::new(mask.x, mask.y, mask.width, mask.height).inverted(mask.invert)
}

/// `mask` on the wire.
#[must_use]
pub const fn wire_mask(mask: RectMask) -> WireLayerMask {
    WireLayerMask {
        x: mask.x,
        y: mask.y,
        width: mask.width,
        height: mask.height,
        invert: mask.invert,
    }
}

/// `source` on the wire.
#[must_use]
pub fn wire_layer_source(source: SourceRef) -> WireSceneLayerSource {
    match source {
        SourceRef::Input(input) => WireSceneLayerSource::Input(WireInputId::from_domain(input)),
        SourceRef::Scene(scene) => WireSceneLayerSource::Scene(WireSceneId::from_domain(scene)),
    }
}

fn domain_layer_source(source: WireSceneLayerSource) -> SourceRef {
    match source {
        WireSceneLayerSource::Input(input) => SourceRef::Input(input.to_domain()),
        WireSceneLayerSource::Scene(scene) => SourceRef::Scene(scene.to_domain()),
    }
}

/// `layer`'s whole design on the wire, its name as given (`name`).
#[must_use]
pub fn wire_layer_outline(layer: &Layer, name: String) -> WireSceneLayerOutline {
    let geometry = wire_geometry(&layer.geometry);
    WireSceneLayerOutline {
        id: WireSceneLayerId::new(layer.id.non_zero()),
        name,
        source: wire_layer_source(layer.source),
        x: geometry.x,
        y: geometry.y,
        width: geometry.width,
        height: geometry.height,
        quarter_turns: geometry.quarter_turns,
        tilt_millidegrees: geometry.tilt_millidegrees,
        anchor_x: geometry.anchor_x,
        anchor_y: geometry.anchor_y,
        opacity: layer.opacity,
        enabled: layer.enabled,
        z_order: layer.z_order,
        crop: layer.crop.map(wire_crop),
        mask: layer.mask.map(wire_mask),
    }
}

/// The complete domain layer a wire spec carries.
#[must_use]
pub fn domain_layer(spec: &WireSceneLayerSpec) -> Layer {
    let outline = &spec.layer;
    Layer {
        id: LayerId::from_non_zero(outline.id.get()),
        name: outline.name.clone(),
        source: domain_layer_source(outline.source),
        enabled: outline.enabled,
        geometry: domain_geometry(&outline.geometry()),
        crop: outline.crop.map(domain_crop),
        mask: outline.mask.map(domain_mask),
        effects: spec
            .effects
            .iter()
            .map(|effect| LayerEffect::new(effect.name.clone(), effect.parameters.clone()))
            .collect(),
        opacity: outline.opacity,
        z_order: outline.z_order,
    }
}

/// `layer` exactly, as a wire spec.
#[must_use]
pub fn wire_layer_spec(layer: &Layer) -> WireSceneLayerSpec {
    WireSceneLayerSpec {
        layer: wire_layer_outline(layer, layer.name.clone()),
        effects: layer
            .effects
            .iter()
            .map(|effect| WireLayerEffect {
                name: effect.name.clone(),
                parameters: effect.parameters.clone(),
            })
            .collect(),
    }
}

/// The engine edit a wire edit names.
#[must_use]
pub fn engine_scene_layer_edit(edit: &SceneLayerEdit) -> EngineSceneLayerEdit {
    EngineSceneLayerEdit {
        scene: edit.scene.to_domain(),
        layer: LayerId::from_non_zero(edit.layer.get()),
        change: match &edit.operation {
            SceneLayerOperation::SetGeometry(geometry) => {
                SceneLayerChange::Geometry(domain_geometry(geometry))
            }
            SceneLayerOperation::SetAppearance { enabled, opacity } => {
                SceneLayerChange::Appearance {
                    enabled: *enabled,
                    opacity: *opacity,
                }
            }
            SceneLayerOperation::SetCrop(crop) => SceneLayerChange::Crop(crop.map(domain_crop)),
            SceneLayerOperation::SetMask(mask) => SceneLayerChange::Mask(mask.map(domain_mask)),
            SceneLayerOperation::SetZOrder { z_order } => SceneLayerChange::ZOrder(*z_order),
            SceneLayerOperation::Move { index } => SceneLayerChange::Move {
                index: usize::from(*index),
            },
            SceneLayerOperation::Add { index, spec } => SceneLayerChange::Add {
                index: usize::from(*index),
                layer: Box::new(domain_layer(spec)),
            },
            SceneLayerOperation::Remove => SceneLayerChange::Remove,
        },
        continues: edit.continues,
    }
}

/// Maps a wire scene layer command onto the engine.
#[must_use]
pub fn scene_layer_engine_command(edit: &SceneLayerEdit) -> EngineCommand {
    EngineCommand::SceneLayer(engine_scene_layer_edit(edit))
}

/// An engine edit back on the wire, exactly. Positions beyond the wire
/// range saturate (the engine never holds that many layers).
#[must_use]
pub fn wire_scene_layer_edit(edit: &EngineSceneLayerEdit) -> SceneLayerEdit {
    let index = |index: usize| u16::try_from(index).unwrap_or(u16::MAX);
    SceneLayerEdit {
        scene: WireSceneId::from_domain(edit.scene),
        layer: WireSceneLayerId::new(edit.layer.non_zero()),
        operation: match &edit.change {
            SceneLayerChange::Geometry(geometry) => {
                SceneLayerOperation::SetGeometry(wire_geometry(geometry))
            }
            SceneLayerChange::Appearance { enabled, opacity } => {
                SceneLayerOperation::SetAppearance {
                    enabled: *enabled,
                    opacity: *opacity,
                }
            }
            SceneLayerChange::Crop(crop) => SceneLayerOperation::SetCrop(crop.map(wire_crop)),
            SceneLayerChange::Mask(mask) => SceneLayerOperation::SetMask(mask.map(wire_mask)),
            SceneLayerChange::ZOrder(z_order) => {
                SceneLayerOperation::SetZOrder { z_order: *z_order }
            }
            SceneLayerChange::Move { index: to } => SceneLayerOperation::Move { index: index(*to) },
            SceneLayerChange::Add { index: at, layer } => SceneLayerOperation::Add {
                index: index(*at),
                spec: Box::new(wire_layer_spec(layer)),
            },
            SceneLayerChange::Remove => SceneLayerOperation::Remove,
        },
        continues: edit.continues,
    }
}

/// The `scene_layout_changed` payload for an accepted `edit` in the
/// post-command `engine`.
///
/// # Panics
///
/// When the engine no longer holds the edited scene (an accepted edit
/// always leaves it).
#[must_use]
pub fn scene_layout_changed(engine: &Engine, edit: &EngineSceneLayerEdit) -> EventPayload {
    let scene = engine
        .show()
        .scene(edit.scene)
        .expect("an accepted scene layer edit leaves its scene");
    EventPayload::SceneLayoutChanged {
        edit: wire_scene_layer_edit(edit),
        layout: Box::new(crate::scene_layout_to_wire(
            &scene.scene,
            scene.canvas_width,
            scene.canvas_height,
        )),
    }
}
