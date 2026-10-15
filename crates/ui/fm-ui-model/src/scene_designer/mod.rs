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
//!   (Shift steps 15°), and crop (Alt on an edge), one undo step each.
//! - [`SceneLayoutDocument`]: the server's layout, the operator's draft
//!   over it, the selection, field edits, stacking order, and the draft's
//!   undo history.
//! - [`SceneLayerEdit`]: the draft as the existing scene layer edit
//!   commands, by zero-based layer index.
//! - [`preview`]: pacing of exact `scene_preview_request`s.
//!
//! The protocol carries a scene's layout only in snapshots
//! (`scene_layout_state`) and has no live scene layer edit command, so a
//! draft cannot be sent to a running server ([`LIVE_LAYER_EDITS`]). The
//! designer keeps it locally and hands it over as the equivalent
//! `freemix-cli scene-layer-*` commands. The replicated layout also omits
//! each layer's crop, mask, and z-order, so the draft starts those as
//! uncropped, unmasked, and stacked in list order, and only edits the
//! operator makes to them become commands.

mod document;
mod edits;
pub mod geometry;
pub mod gesture;
pub mod preview;
pub mod snapping;
#[cfg(test)]
mod tests;

use core::hash::{Hash, Hasher};

use fm_protocol::{WireSceneLayerSource, WireSceneLayout};

pub use document::{DesignLayer, EditLabel, Field, MAX_UNDO_STEPS, SceneLayoutDocument};
pub use edits::SceneLayerEdit;

/// Whether a running server accepts live scene layer edits. The protocol
/// has no scene layer edit command yet, so a draft is applied offline with
/// `freemix-cli` (see [`SceneLayoutDocument::cli_script`]).
pub const LIVE_LAYER_EDITS: bool = false;

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
    }
    hasher.finish()
}
