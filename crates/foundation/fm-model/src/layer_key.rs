//! Per-scene-layer keys (IN-020, SW-008).
//!
//! A scene layer may carry one [`LayerKey`]: a chroma key that removes one
//! colour, a luma key that removes a brightness band, or a key/fill pair
//! whose matte comes from a separate key input. The key is applied after the
//! layer's crop, multiplied with its mask, and before its effects and grade,
//! with the compositor's exact integer semantics:
//!
//! * **Chroma**: a pixel's distance is the largest per-channel difference
//!   from the key colour. Alpha is `0` while the distance is at most
//!   `similarity`, rises linearly to `255` over the next `smoothness`
//!   codes, and spill suppression (`spill`, `0` = off) pulls the key
//!   colour's dominant channel of partially keyed pixels toward the other
//!   two.
//! * **Luma**: alpha is `0` while the pixel's BT.709 luma is at most `low`,
//!   rises to `255` over `softness` codes, and (when `high < 255`) falls back
//!   to `0` over `softness` codes below `high`; `invert` swaps kept and keyed
//!   pixels.
//! * **Key/fill** ([`FillLayerKey`]): the layer's source is the fill and the
//!   matte is the luma ramp above applied to the key input's luma (a linear
//!   key) or alpha ([`KeyChannel`]); `low` and `softness` act as clip and
//!   gain. The key input is cropped like the fill and must be a non-Scene
//!   input.
//!
//! Every key carries a [`KeyRefinement`] (the identity by default): the raw
//! matte is clipped, choked or spread, and softened before the mask, an
//! optional green or blue despill lowers the backing colour's spill, and an
//! optional light wrap bleeds the background into the layer's edges.

use core::fmt;

use fm_types::{GarbageMatte, InputId};

/// Largest choke or spread radius, in pixels.
pub const MAX_KEY_CHOKE: u8 = 8;
/// Largest edge-softness radius, in pixels.
pub const MAX_KEY_EDGE_SOFTNESS: u8 = 8;
/// Largest light wrap width, in pixels.
pub const MAX_KEY_LIGHT_WRAP_WIDTH: u8 = 8;

/// Spill suppression of a green or blue backing.
///
/// For a green backing green is lowered toward `(red + blue + 1) / 2` by
/// `strength / 255` of its excess (blue likewise for a blue backing); a
/// channel is only ever lowered.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum KeyDespill {
    #[default]
    Off,
    Green {
        strength: u8,
    },
    Blue {
        strength: u8,
    },
}

/// Matte refinement applied to a layer key's raw alpha.
///
/// Order: clip (alpha at most `clip_black` becomes `0`, at least
/// `clip_white` becomes `255`, linear between), then choke (`choke > 0`
/// erodes with a `2 * choke + 1` square window minimum, `choke < 0` spreads
/// with the maximum), then edge softness (the rounded mean over a
/// `2 * edge_softness + 1` window), then the mask. Windows are clamped to
/// the cropped layer. [`KeyDespill`] adjusts colour independently.
///
/// Light wrap (`light_wrap > 0`) bleeds the light of what lies beneath the
/// layer into its edges where it is composited: an edge pixel moves toward
/// the background averaged over `light_wrap_width` pixels around it, by up
/// to `light_wrap / 255` (the compositor's `LightWrap` has the exact
/// formula).
///
/// A polygon [`GarbageMatte`] (normalized to the cropped layer; see
/// [`fm_types::garbage_matte`]) removes the pixels outside it (or inside
/// when inverted), multiplied with the matte together with the layer's
/// rectangular mask.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyRefinement {
    pub clip_black: u8,
    /// Always above `clip_black`.
    pub clip_white: u8,
    /// `-MAX_KEY_CHOKE..=MAX_KEY_CHOKE`.
    pub choke: i8,
    /// `0..=MAX_KEY_EDGE_SOFTNESS`.
    pub edge_softness: u8,
    pub despill: KeyDespill,
    /// Light wrap amount, `0` off.
    pub light_wrap: u8,
    /// Light wrap reach in pixels: `0` exactly when `light_wrap` is `0`,
    /// otherwise `1..=MAX_KEY_LIGHT_WRAP_WIDTH`.
    pub light_wrap_width: u8,
    /// The polygon garbage matte, if any.
    pub garbage_matte: Option<GarbageMatte>,
}

impl Default for KeyRefinement {
    fn default() -> Self {
        Self::NONE
    }
}

impl KeyRefinement {
    /// The identity refinement: no clip, choke, softness, or despill.
    pub const NONE: Self = Self {
        clip_black: 0,
        clip_white: 255,
        choke: 0,
        edge_softness: 0,
        despill: KeyDespill::Off,
        light_wrap: 0,
        light_wrap_width: 0,
        garbage_matte: None,
    };

    /// The light wrap width a newly enabled light wrap starts with.
    pub const DEFAULT_LIGHT_WRAP_WIDTH: u8 = 3;

    /// Whether this refinement changes nothing.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self == Self::NONE
    }

    /// Checks the clip range, the choke and softness radii, and the light
    /// wrap.
    ///
    /// # Errors
    ///
    /// [`LayerKeyError::EmptyClipRange`], [`LayerKeyError::ChokeOutOfRange`],
    /// [`LayerKeyError::EdgeSoftnessOutOfRange`], or
    /// [`LayerKeyError::LightWrapOutOfRange`].
    pub const fn validate(self) -> Result<(), LayerKeyError> {
        if self.clip_black >= self.clip_white {
            return Err(LayerKeyError::EmptyClipRange);
        }
        if self.choke.unsigned_abs() > MAX_KEY_CHOKE {
            return Err(LayerKeyError::ChokeOutOfRange);
        }
        if self.edge_softness > MAX_KEY_EDGE_SOFTNESS {
            return Err(LayerKeyError::EdgeSoftnessOutOfRange);
        }
        if (self.light_wrap == 0) != (self.light_wrap_width == 0)
            || self.light_wrap_width > MAX_KEY_LIGHT_WRAP_WIDTH
        {
            return Err(LayerKeyError::LightWrapOutOfRange);
        }
        Ok(())
    }
}

/// A chroma key: removes pixels near one colour.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChromaLayerKey {
    /// The key colour (straight 8-bit sRGB).
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    /// Largest colour distance keyed out completely.
    pub similarity: u8,
    /// Codes over which alpha rises from keyed to kept beyond `similarity`.
    pub smoothness: u8,
    /// Spill suppression strength, `0` off.
    pub spill: u8,
    pub refinement: KeyRefinement,
}

impl ChromaLayerKey {
    /// A green-screen key: pure green, similarity 64, smoothness 32, no
    /// spill suppression or refinement.
    pub const GREEN: Self = Self {
        red: 0,
        green: 255,
        blue: 0,
        similarity: 64,
        smoothness: 32,
        spill: 0,
        refinement: KeyRefinement::NONE,
    };
}

/// A luma key: removes pixels by brightness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LumaLayerKey {
    /// Largest luma keyed out completely at the dark end.
    pub low: u8,
    /// Smallest luma keyed out completely at the bright end; `255` keeps
    /// every bright pixel. Always above `low`.
    pub high: u8,
    /// Codes over which alpha ramps at each bound.
    pub softness: u8,
    /// Keeps the keyed band and removes the rest instead.
    pub invert: bool,
    pub refinement: KeyRefinement,
}

impl LumaLayerKey {
    /// A key removing black: low 16, no upper bound, softness 32.
    pub const BLACK: Self = Self {
        low: 16,
        high: 255,
        softness: 32,
        invert: false,
        refinement: KeyRefinement::NONE,
    };
}

/// Which channel of a key/fill key input drives the matte.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum KeyChannel {
    /// The key input's BT.709 luma (a classic linear key signal).
    #[default]
    Luma,
    /// The key input's alpha channel.
    Alpha,
}

/// A key/fill pair: the layer's source is the fill and `key_input` supplies
/// the matte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FillLayerKey {
    /// The key signal; a non-Scene input of the project.
    pub key_input: InputId,
    pub channel: KeyChannel,
    /// Matte ramp, as [`LumaLayerKey`]: `low` clips the key signal's dark
    /// end, `softness` sets the gain, `high < 255` adds an upper bound, and
    /// `invert` inverts the matte. `low < high`.
    pub low: u8,
    pub high: u8,
    pub softness: u8,
    pub invert: bool,
    pub refinement: KeyRefinement,
}

impl FillLayerKey {
    /// A linear key: the matte equals the key input's luma.
    #[must_use]
    pub const fn linear(key_input: InputId) -> Self {
        Self {
            key_input,
            channel: KeyChannel::Luma,
            low: 0,
            high: 255,
            softness: 255,
            invert: false,
            refinement: KeyRefinement::NONE,
        }
    }
}

/// One scene layer's key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerKey {
    Chroma(ChromaLayerKey),
    Luma(LumaLayerKey),
    Fill(FillLayerKey),
}

impl LayerKey {
    /// Checks the key's bounds: a luma or key/fill band's `low` must be
    /// below its `high`, and the refinement must hold
    /// ([`KeyRefinement::validate`]).
    ///
    /// # Errors
    ///
    /// [`LayerKeyError::EmptyLumaBand`] or a refinement error.
    pub const fn validate(self) -> Result<(), LayerKeyError> {
        let band = match self {
            Self::Chroma(_) => true,
            Self::Luma(luma) => luma.low < luma.high,
            Self::Fill(fill) => fill.low < fill.high,
        };
        if !band {
            return Err(LayerKeyError::EmptyLumaBand);
        }
        self.refinement().validate()
    }

    /// The key's matte refinement.
    #[must_use]
    pub const fn refinement(self) -> KeyRefinement {
        match self {
            Self::Chroma(chroma) => chroma.refinement,
            Self::Luma(luma) => luma.refinement,
            Self::Fill(fill) => fill.refinement,
        }
    }

    /// The same key with `refinement`.
    #[must_use]
    pub const fn with_refinement(self, refinement: KeyRefinement) -> Self {
        match self {
            Self::Chroma(mut chroma) => {
                chroma.refinement = refinement;
                Self::Chroma(chroma)
            }
            Self::Luma(mut luma) => {
                luma.refinement = refinement;
                Self::Luma(luma)
            }
            Self::Fill(mut fill) => {
                fill.refinement = refinement;
                Self::Fill(fill)
            }
        }
    }

    /// The key/fill key input, if this is a key/fill key.
    #[must_use]
    pub const fn key_input(self) -> Option<InputId> {
        match self {
            Self::Fill(fill) => Some(fill.key_input),
            Self::Chroma(_) | Self::Luma(_) => None,
        }
    }
}

/// Why a [`LayerKey`] is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerKeyError {
    /// A luma or key/fill band whose `low` is not below its `high`.
    EmptyLumaBand,
    /// A refinement whose `clip_black` is not below its `clip_white`.
    EmptyClipRange,
    /// A choke or spread beyond [`MAX_KEY_CHOKE`] pixels.
    ChokeOutOfRange,
    /// An edge softness beyond [`MAX_KEY_EDGE_SOFTNESS`] pixels.
    EdgeSoftnessOutOfRange,
    /// A light wrap width beyond [`MAX_KEY_LIGHT_WRAP_WIDTH`] pixels, or one
    /// that is zero exactly when the amount is not.
    LightWrapOutOfRange,
}

impl fmt::Display for LayerKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyLumaBand => formatter.write_str("luma key low must be below its high"),
            Self::EmptyClipRange => {
                formatter.write_str("key clip black must be below its clip white")
            }
            Self::ChokeOutOfRange => write!(
                formatter,
                "key choke must be within {MAX_KEY_CHOKE} pixels either way"
            ),
            Self::EdgeSoftnessOutOfRange => write!(
                formatter,
                "key edge softness must be at most {MAX_KEY_EDGE_SOFTNESS} pixels"
            ),
            Self::LightWrapOutOfRange => write!(
                formatter,
                "key light wrap width must be 1..={MAX_KEY_LIGHT_WRAP_WIDTH} pixels when its amount is set, and 0 otherwise"
            ),
        }
    }
}

impl std::error::Error for LayerKeyError {}
