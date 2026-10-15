//! Per-scene-layer keys (IN-020).
//!
//! A scene layer may carry one [`LayerKey`]: a chroma key that removes one
//! colour or a luma key that removes a brightness band. The key is applied
//! after the layer's crop, multiplied with its mask, and before its effects
//! and grade, with the compositor's exact integer semantics:
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

use core::fmt;

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
}

impl ChromaLayerKey {
    /// A green-screen key: pure green, similarity 64, smoothness 32, no
    /// spill suppression.
    pub const GREEN: Self = Self {
        red: 0,
        green: 255,
        blue: 0,
        similarity: 64,
        smoothness: 32,
        spill: 0,
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
}

impl LumaLayerKey {
    /// A key removing black: low 16, no upper bound, softness 32.
    pub const BLACK: Self = Self {
        low: 16,
        high: 255,
        softness: 32,
        invert: false,
    };
}

/// One scene layer's key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerKey {
    Chroma(ChromaLayerKey),
    Luma(LumaLayerKey),
}

impl LayerKey {
    /// Checks the key's bounds: a luma key's `low` must be below its
    /// `high`. Every chroma key is valid.
    ///
    /// # Errors
    ///
    /// [`LayerKeyError::EmptyLumaBand`].
    pub const fn validate(self) -> Result<(), LayerKeyError> {
        match self {
            Self::Chroma(_) => Ok(()),
            Self::Luma(luma) => {
                if luma.low < luma.high {
                    Ok(())
                } else {
                    Err(LayerKeyError::EmptyLumaBand)
                }
            }
        }
    }
}

/// Why a [`LayerKey`] is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerKeyError {
    /// A luma key whose `low` is not below its `high`.
    EmptyLumaBand,
}

impl fmt::Display for LayerKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyLumaBand => formatter.write_str("luma key low must be below its high"),
        }
    }
}

impl std::error::Error for LayerKeyError {}
