//! The headless scene layer designer (SW-006): direct manipulation and
//! numeric editing of one scene's layers in project canvas pixels.
//!
//! - [`geometry`]: layer placements, turned frames, handle positions, and
//!   hit-testing, exact for any rotation.
//! - [`snapping`]: canvas edges and centre, action-safe (93%) and
//!   title-safe (90%) areas, and the other layers' edges and centres, with
//!   the guides a snapped result lines up with.
//! - [`gesture`]: press, drag, and release on the canvas: move (the whole
//!   selection), scale (corner or edge, Shift keeps the aspect), rotate
//!   about the anchor to any angle (Shift steps 15°), move the anchor, and
//!   crop (Alt on an edge), one undo step each.
//! - [`SceneLayoutDocument`]: the server's layout, the operator's draft
//!   over it, the selection, field edits, stacking order, the draft's
//!   undo history, and the live edits it sends.
//! - [`SceneLayerEdit`]: the layout as offline scene layer edit commands,
//!   by zero-based layer index (the secondary `freemix-cli` export).
//! - [`preview`]: pacing of exact `scene_preview_request`s.
//!
//! The replicated layout (`scene_layout_state`, `scene_layout_changed`)
//! carries every layer's geometry (quarter turns, tilt, and anchor), crop,
//! mask, key (chroma or luma), effect list (Protocol 2.43), and z-order,
//! and a running server takes live `scene_layer_*`
//! edits (Protocol 2.39, [`LIVE_LAYER_EDITS`]): the designer sends the
//! draft while a gesture runs, paced by the window, and the server's undo
//! takes back a whole gesture of one layer at once.

pub(crate) mod document;
mod edits;
pub mod geometry;
pub mod gesture;
pub mod preview;
pub mod snapping;
#[cfg(test)]
mod tests;

use core::hash::{Hash, Hasher};

use fm_protocol::{WireSceneLayerSource, WireSceneLayout};

pub use document::{
    CHROMA_GREEN, DEFAULT_DESPILL_STRENGTH, DEFAULT_LIGHT_WRAP_WIDTH, DesignLayer, EditLabel,
    Field, LUMA_BLACK, MAX_UNDO_STEPS, SceneLayoutDocument, default_effect, effect_ranges,
    fill_linear, wire_geometry,
};
pub use edits::SceneLayerEdit;
pub use fm_protocol::{MAX_SCENE_LAYER_EFFECTS, SCENE_LAYER_EFFECT_KINDS};

/// Whether a running server accepts live scene layer edits (Protocol
/// 2.39): the designer sends its draft as `scene_layer_*` commands
/// ([`SceneLayoutDocument::take_live_edits`]).
pub const LIVE_LAYER_EDITS: bool = true;

/// Most visible layers a scene draws (the compositor's limit): the server
/// refuses to show a layer past it, so the designer does not offer to.
pub const MAX_VISIBLE_LAYERS: usize = fm_protocol::MAX_SCENE_ENABLED_LAYERS;

/// A stamp that changes whenever `layout` does, for preview pacing.
#[must_use]
pub fn layout_stamp(layout: &WireSceneLayout) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (layout.width, layout.height).hash(&mut hasher);
    for layer in &layout.layers {
        layer.id.hash(&mut hasher);
        match layer.source {
            WireSceneLayerSource::Input(input) => (0_u8, input.get().get()),
            WireSceneLayerSource::Scene(scene) => (1_u8, scene.get().get()),
        }
        .hash(&mut hasher);
        (
            layer.x,
            layer.y,
            layer.width,
            layer.height,
            layer.quarter_turns,
            layer.opacity,
            layer.enabled,
        )
            .hash(&mut hasher);
        (
            layer.tilt_millidegrees,
            layer.anchor_x,
            layer.anchor_y,
            layer.z_order,
            layer
                .crop
                .map(|crop| (crop.x, crop.y, crop.width, crop.height)),
            layer
                .mask
                .map(|mask| (mask.x, mask.y, mask.width, mask.height, mask.invert)),
        )
            .hash(&mut hasher);
        (layer.key, &layer.effects).hash(&mut hasher);
    }
    hasher.finish()
}
