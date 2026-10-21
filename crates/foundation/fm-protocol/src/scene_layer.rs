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
//! | [`SceneLayerOperation::SetKey`] | `scene_layer_key_set` / `scene_layer_key_clear` | `kind` (`chroma`: `red`, `green`, `blue`, `similarity`, `smoothness`, `spill`; `luma`: `low`, `high`, `softness`, `invert`; `fill` (Protocol 2.70): `key_input`, `channel` (`luma`/`alpha`), `low`, `high`, `softness`, `invert`), then the refinement (Protocol 2.70): `clip_black`, `clip_white`, `choke`, `edge_softness`, `despill` (`off`/`green`/`blue`), `despill_strength` (set only) |
//! | [`SceneLayerOperation::SetEffects`] | `scene_layer_effects_set` | `effects` |
//! | [`SceneLayerOperation::SetZOrder`] | `scene_layer_z_order_set` | `z_order` |
//! | [`SceneLayerOperation::Move`] | `scene_layer_move` | `index` |
//! | [`SceneLayerOperation::Add`] | `scene_layer_add` | `index`, `spec` |
//! | [`SceneLayerOperation::Remove`] | `scene_layer_remove` | (none) |

use crate::{WireInputId, WireSceneId, WireSceneLayerId, WireSceneLayerOutline};

/// Largest key choke or spread radius, in pixels (Protocol 2.70).
pub const MAX_KEY_CHOKE: u8 = 8;
/// Largest key edge-softness radius, in pixels (Protocol 2.70).
pub const MAX_KEY_EDGE_SOFTNESS: u8 = 8;
/// Largest key light wrap width, in pixels (Protocol 2.98).
pub const MAX_KEY_LIGHT_WRAP_WIDTH: u8 = 8;

/// The far edge of a layer anchor coordinate, in basis points.
pub const SCENE_LAYER_ANCHOR_MAX: u16 = 10_000;
/// The box centre, the default anchor coordinate.
pub const SCENE_LAYER_ANCHOR_CENTER: u16 = 5_000;
/// Smallest stored layer tilt, inclusive.
pub const MIN_SCENE_LAYER_TILT_MILLIDEGREES: i32 = -45_000;
/// Largest stored layer tilt, exclusive.
pub const MAX_SCENE_LAYER_TILT_MILLIDEGREES: i32 = 45_000;
/// Most effects one layer carries (Protocol 2.43: 8).
pub const MAX_SCENE_LAYER_EFFECTS: usize = 8;
/// Every effect kind a layer carries (Protocol 2.43), in wire order (the
/// layout encoding's kind byte is the index), with the inclusive range of
/// each of its parameters. A `levels` effect also needs `black < white`.
pub const SCENE_LAYER_EFFECT_KINDS: [(&str, &[(i32, i32)]); 5] = [
    ("levels", &[(0, 255), (0, 255)]),
    ("brightness_contrast", &[(-100, 100), (-100, 100)]),
    ("saturation", &[(-100, 100)]),
    ("sharpen", &[(0, 1_000)]),
    ("flip", &[(0, 1), (0, 1)]),
];

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

/// One ordered video effect of a layer: a kind of
/// [`SCENE_LAYER_EFFECT_KINDS`] with exactly its parameters, each in range.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WireLayerEffect {
    pub name: String,
    pub parameters: Vec<i32>,
}

impl WireLayerEffect {
    /// The effect's kind index in [`SCENE_LAYER_EFFECT_KINDS`] when it is
    /// valid (known kind, exact parameter count, every parameter in range,
    /// and `black < white` for `levels`).
    #[must_use]
    pub fn kind_index(&self) -> Option<u8> {
        let (index, (_, ranges)) = SCENE_LAYER_EFFECT_KINDS
            .iter()
            .enumerate()
            .find(|(_, (name, _))| *name == self.name)?;
        let valid = self.parameters.len() == ranges.len()
            && self
                .parameters
                .iter()
                .zip(ranges.iter())
                .all(|(value, (minimum, maximum))| (*minimum..=*maximum).contains(value))
            && (self.name != "levels" || self.parameters[0] < self.parameters[1]);
        valid.then(|| u8::try_from(index).ok()).flatten()
    }

    /// Whether every bound of [`Self::kind_index`] holds.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.kind_index().is_some()
    }
}

/// Green or blue backing spill suppression of a key (Protocol 2.70).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum WireKeyDespill {
    #[default]
    Off,
    Green {
        strength: u8,
    },
    Blue {
        strength: u8,
    },
}

/// A key's matte refinement (Protocol 2.70): clip, then choke (`> 0`) or
/// spread (`< 0`), then edge softness, then the mask; despill adjusts
/// colour. `clip_black < clip_white`, `|choke| <= MAX_KEY_CHOKE`,
/// `edge_softness <= MAX_KEY_EDGE_SOFTNESS`. Light wrap (Protocol 2.98)
/// bleeds the background into the layer's edges: `light_wrap` is its
/// amount (`0` off) and `light_wrap_width` its reach, `0` exactly when the
/// amount is `0` and otherwise at most `MAX_KEY_LIGHT_WRAP_WIDTH`. A
/// polygon garbage matte (Protocol 2.98, [`fm_types::GarbageMatte`])
/// removes the pixels outside it, or inside when inverted.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WireKeyRefinement {
    pub clip_black: u8,
    pub clip_white: u8,
    pub choke: i8,
    pub edge_softness: u8,
    pub despill: WireKeyDespill,
    pub light_wrap: u8,
    pub light_wrap_width: u8,
    pub garbage_matte: Option<fm_types::GarbageMatte>,
}

impl Default for WireKeyRefinement {
    fn default() -> Self {
        Self::NONE
    }
}

impl WireKeyRefinement {
    /// The identity refinement.
    pub const NONE: Self = Self {
        clip_black: 0,
        clip_white: 255,
        choke: 0,
        edge_softness: 0,
        despill: WireKeyDespill::Off,
        light_wrap: 0,
        light_wrap_width: 0,
        garbage_matte: None,
    };

    /// Whether every bound holds.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.clip_black < self.clip_white
            && self.choke.unsigned_abs() <= MAX_KEY_CHOKE
            && self.edge_softness <= MAX_KEY_EDGE_SOFTNESS
            && (self.light_wrap == 0) == (self.light_wrap_width == 0)
            && self.light_wrap_width <= MAX_KEY_LIGHT_WRAP_WIDTH
    }
}

/// A chroma key (Protocol 2.43): removes pixels within `similarity` of the
/// key colour, ramps alpha over `smoothness`, and suppresses `spill`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WireChromaKey {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub similarity: u8,
    pub smoothness: u8,
    pub spill: u8,
    /// Protocol 2.70.
    pub refinement: WireKeyRefinement,
}

/// A luma key (Protocol 2.43): removes luma at most `low` and (when `high`
/// is below 255) at least `high`, ramping over `softness`; `invert` keeps
/// the band instead. `low < high`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WireLumaKey {
    pub low: u8,
    pub high: u8,
    pub softness: u8,
    pub invert: bool,
    /// Protocol 2.70.
    pub refinement: WireKeyRefinement,
}

/// A key/fill key (Protocol 2.70): the layer's source is the fill and the
/// matte is the luma-key ramp (`low`, `high`, `softness`, `invert`) of the
/// non-Scene input `key_input`'s luma, or its alpha when `alpha_channel`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WireFillKey {
    pub key_input: WireInputId,
    pub alpha_channel: bool,
    pub low: u8,
    pub high: u8,
    pub softness: u8,
    pub invert: bool,
    pub refinement: WireKeyRefinement,
}

/// A layer's key (Protocol 2.43; key/fill and refinements 2.70).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WireLayerKey {
    Chroma(WireChromaKey),
    Luma(WireLumaKey),
    Fill(WireFillKey),
}

impl WireLayerKey {
    /// Whether the key's bounds hold (a luma or key/fill band's
    /// `low < high`, and the refinement's bounds).
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        let band = match self {
            Self::Chroma(_) => true,
            Self::Luma(luma) => luma.low < luma.high,
            Self::Fill(fill) => fill.low < fill.high,
        };
        band && self.refinement().is_valid()
    }

    /// The key's matte refinement.
    #[must_use]
    pub const fn refinement(&self) -> WireKeyRefinement {
        match self {
            Self::Chroma(chroma) => chroma.refinement,
            Self::Luma(luma) => luma.refinement,
            Self::Fill(fill) => fill.refinement,
        }
    }
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
    /// Sets (`Some`) or clears the chroma or luma key (Protocol 2.43).
    SetKey(Option<WireLayerKey>),
    /// Replaces the ordered effect list, at most
    /// [`MAX_SCENE_LAYER_EFFECTS`] (Protocol 2.43).
    SetEffects(Vec<WireLayerEffect>),
    SetZOrder {
        z_order: i32,
    },
    /// Moves the layer to stacking (list) position `index`, `0` at the
    /// bottom.
    Move {
        index: u16,
    },
    /// Inserts a new layer (`spec.id` is the edit's layer, its name
    /// nonblank) at list position `index` (at most the layer count). The
    /// undo of a removal carries the removed layer exactly.
    Add {
        index: u16,
        spec: Box<WireSceneLayerOutline>,
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
            Self::SetKey(Some(_)) => "scene_layer_key_set",
            Self::SetKey(None) => "scene_layer_key_clear",
            Self::SetEffects(_) => "scene_layer_effects_set",
            Self::SetZOrder { .. } => "scene_layer_z_order_set",
            Self::Move { .. } => "scene_layer_move",
            Self::Add { .. } => "scene_layer_add",
            Self::Remove => "scene_layer_remove",
        }
    }

    /// Every payload name, for routing.
    pub const PAYLOADS: [&'static str; 13] = [
        "scene_layer_geometry_set",
        "scene_layer_appearance_set",
        "scene_layer_crop_set",
        "scene_layer_crop_clear",
        "scene_layer_mask_set",
        "scene_layer_mask_clear",
        "scene_layer_key_set",
        "scene_layer_key_clear",
        "scene_layer_effects_set",
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
