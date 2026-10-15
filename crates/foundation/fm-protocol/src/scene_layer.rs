//! Live scene layer editing (Protocol 2.39, SW-006).
//!
//! A [`SceneLayerEdit`] changes one layer of one scene, addressed by scene
//! id and stable layer id, whether or not a Scene input shows the scene. It
//! is a project edit: padlocked (the project padlock and the padlock of every
//! Scene input showing the scene), undoable with an exact inverse, and
//! journaled. Every accepted edit, undo and redo included, is announced by
//! `event=scene_layout_changed` with the edit and the scene's whole new
//! layout.
//!
//! | Operation | Payload | Fields beyond `scene`, `layer` |
//! |---|---|---|
//! | [`SceneLayerOperation::SetGeometry`] | `scene_layer_geometry_set` | `x`, `y`, `width`, `height`, `quarter_turns`, `tilt_millidegrees`, `anchor_x`, `anchor_y` |
//! | [`SceneLayerOperation::SetAppearance`] | `scene_layer_appearance_set` | `enabled`, `opacity` |
//! | [`SceneLayerOperation::SetCrop`] | `scene_layer_crop_set` / `scene_layer_crop_clear` | `x`, `y`, `width`, `height` (set only) |
//! | [`SceneLayerOperation::SetMask`] | `scene_layer_mask_set` / `scene_layer_mask_clear` | `x`, `y`, `width`, `height`, `invert` (set only) |
//! | [`SceneLayerOperation::SetZOrder`] | `scene_layer_z_order_set` | `z_order` |
//! | [`SceneLayerOperation::Move`] | `scene_layer_move` | `index` |
//! | [`SceneLayerOperation::Add`] | `scene_layer_add` | `index`, `spec` |
//! | [`SceneLayerOperation::Remove`] | `scene_layer_remove` | (none) |

use crate::{WireSceneId, WireSceneLayerId, WireSceneLayerOutline};

/// The far edge of a layer anchor coordinate, in basis points.
pub const SCENE_LAYER_ANCHOR_MAX: u16 = 10_000;
/// The box centre, the default anchor coordinate.
pub const SCENE_LAYER_ANCHOR_CENTER: u16 = 5_000;
/// Smallest stored layer tilt, inclusive.
pub const MIN_SCENE_LAYER_TILT_MILLIDEGREES: i32 = -45_000;
/// Largest stored layer tilt, exclusive.
pub const MAX_SCENE_LAYER_TILT_MILLIDEGREES: i32 = 45_000;
/// Most effects an added layer carries.
pub const MAX_SCENE_LAYER_EFFECTS: usize = 16;
/// Longest effect name, in UTF-8 bytes.
pub const MAX_SCENE_LAYER_EFFECT_NAME_BYTES: usize = 64;
/// Most parameters one effect carries.
pub const MAX_SCENE_LAYER_EFFECT_PARAMETERS: usize = 32;

/// A layer's source crop in canvas pixels: nonempty and inside the canvas.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WireLayerCrop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// A layer's hard-edged rectangular mask in post-crop source pixels:
/// nonempty and inside the cropped source. `invert` keeps the outside.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WireLayerMask {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub invert: bool,
}

/// A layer's whole geometry: the box, its quarter turns and tilt (free
/// rotation `quarter_turns * 90_000 + tilt_millidegrees` millidegrees), and
/// its scale and rotation pivot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WireLayerGeometry {
    /// Top-left of the quarter-turned (untilted) footprint.
    pub x: i32,
    pub y: i32,
    /// Size before the quarter turns, each nonzero.
    pub width: u32,
    pub height: u32,
    /// Clockwise quarter turns, `0..=3`.
    pub quarter_turns: u8,
    /// Further clockwise turn about the anchor, in
    /// `MIN_SCENE_LAYER_TILT_MILLIDEGREES..MAX_SCENE_LAYER_TILT_MILLIDEGREES`.
    pub tilt_millidegrees: i32,
    /// Pivot in the box's own frame, basis points, each
    /// `0..=SCENE_LAYER_ANCHOR_MAX`.
    pub anchor_x: u16,
    pub anchor_y: u16,
}

impl WireLayerGeometry {
    /// Whether every bound of the type documentation holds (the protocol
    /// does not bound the sides beyond nonzero; the authority does).
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.width != 0
            && self.height != 0
            && self.quarter_turns <= 3
            && self.tilt_millidegrees >= MIN_SCENE_LAYER_TILT_MILLIDEGREES
            && self.tilt_millidegrees < MAX_SCENE_LAYER_TILT_MILLIDEGREES
            && self.anchor_x <= SCENE_LAYER_ANCHOR_MAX
            && self.anchor_y <= SCENE_LAYER_ANCHOR_MAX
    }

    /// The whole clockwise turn in `0..360_000` millidegrees.
    #[must_use]
    pub const fn rotation_millidegrees(&self) -> i32 {
        (self.quarter_turns as i32 * 90_000 + self.tilt_millidegrees).rem_euclid(360_000)
    }
}

impl WireSceneLayerOutline {
    /// The layer's whole geometry.
    #[must_use]
    pub const fn geometry(&self) -> WireLayerGeometry {
        WireLayerGeometry {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            quarter_turns: self.quarter_turns,
            tilt_millidegrees: self.tilt_millidegrees,
            anchor_x: self.anchor_x,
            anchor_y: self.anchor_y,
        }
    }

    /// Replaces the layer's whole geometry.
    pub const fn set_geometry(&mut self, geometry: WireLayerGeometry) {
        self.x = geometry.x;
        self.y = geometry.y;
        self.width = geometry.width;
        self.height = geometry.height;
        self.quarter_turns = geometry.quarter_turns;
        self.tilt_millidegrees = geometry.tilt_millidegrees;
        self.anchor_x = geometry.anchor_x;
        self.anchor_y = geometry.anchor_y;
    }
}

/// One ordered video effect of an added layer: a nonempty name of at most
/// [`MAX_SCENE_LAYER_EFFECT_NAME_BYTES`] bytes without control characters
/// and at most [`MAX_SCENE_LAYER_EFFECT_PARAMETERS`] parameters.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WireLayerEffect {
    pub name: String,
    pub parameters: Vec<i32>,
}

/// A complete layer as `scene_layer_add` carries it: the full design (its
/// name nonblank) and its ordered effects (at most
/// [`MAX_SCENE_LAYER_EFFECTS`]). The undo of a removal carries the removed
/// layer exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireSceneLayerSpec {
    pub layer: WireSceneLayerOutline,
    pub effects: Vec<WireLayerEffect>,
}

/// What one live scene layer edit changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SceneLayerOperation {
    SetGeometry(WireLayerGeometry),
    SetAppearance {
        enabled: bool,
        opacity: u8,
    },
    /// Sets (`Some`) or clears the crop. Refused when the layer's mask would
    /// fall outside the new crop.
    SetCrop(Option<WireLayerCrop>),
    /// Sets (`Some`) or clears the mask.
    SetMask(Option<WireLayerMask>),
    SetZOrder {
        z_order: i32,
    },
    /// Moves the layer to stacking (list) position `index`, `0` at the
    /// bottom.
    Move {
        index: u16,
    },
    /// Inserts a new layer (`spec.layer.id` is the edit's layer) at list
    /// position `index` (at most the layer count).
    Add {
        index: u16,
        spec: Box<WireSceneLayerSpec>,
    },
    /// Removes a layer no animation track or grade names.
    Remove,
}

impl SceneLayerOperation {
    /// The wire payload name.
    #[must_use]
    pub const fn payload(&self) -> &'static str {
        match self {
            Self::SetGeometry(_) => "scene_layer_geometry_set",
            Self::SetAppearance { .. } => "scene_layer_appearance_set",
            Self::SetCrop(Some(_)) => "scene_layer_crop_set",
            Self::SetCrop(None) => "scene_layer_crop_clear",
            Self::SetMask(Some(_)) => "scene_layer_mask_set",
            Self::SetMask(None) => "scene_layer_mask_clear",
            Self::SetZOrder { .. } => "scene_layer_z_order_set",
            Self::Move { .. } => "scene_layer_move",
            Self::Add { .. } => "scene_layer_add",
            Self::Remove => "scene_layer_remove",
        }
    }

    /// Every payload name, for routing.
    pub const PAYLOADS: [&'static str; 10] = [
        "scene_layer_geometry_set",
        "scene_layer_appearance_set",
        "scene_layer_crop_set",
        "scene_layer_crop_clear",
        "scene_layer_mask_set",
        "scene_layer_mask_clear",
        "scene_layer_z_order_set",
        "scene_layer_move",
        "scene_layer_add",
        "scene_layer_remove",
    ];
}

/// One live scene layer edit: layer `layer` of scene `scene`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneLayerEdit {
    pub scene: WireSceneId,
    pub layer: WireSceneLayerId,
    pub operation: SceneLayerOperation,
    /// Whether the edit continues one gesture (a drag or a field scrub):
    /// the server folds it into the latest undo step when that step edits
    /// the same property of the same layer and was opened by the same
    /// [`Self::gesture`], so a whole gesture undoes at once. The first edit
    /// of a gesture sends `false`.
    pub continues: bool,
    /// The gesture the edit belongs to (Protocol 2.41): a token the client
    /// picks once per gesture, unique across sessions (Studio picks a
    /// random one), or `0` outside any gesture. A continuing edit folds only
    /// into a step its own gesture opened, so a gesture whose first edit
    /// was refused or lost opens a new step instead of folding into an
    /// earlier gesture's.
    pub gesture: u64,
}
