//! Secondary mixes on the wire (IN-023, Protocol 2.158).
//!
//! The show's secondary mixes travel in a snapshot's `secondary_mixes` list
//! and change with `secondary_mix_set` or `secondary_mix_clear`. Each entry is
//! `<number>~<name>~<mode>~<program>~<preview>`:
//!
//! * `number` is 2 through 16; Main is the switcher's own mix.
//! * `name` is escaped and bounded.
//! * `mode` is `preview_program` or `output_only`.
//! * `program` and `preview` are `input:<id>` or `mix:<1..=16>`; `preview` is
//!   `-` exactly when the mix is output-only.
//!
//! Entries are strictly ascending by number. The engine and project validate
//! references, Preview/Program equality, and cycles.

use core::fmt;

/// Number of secondary mixes beside Main.
pub const MAX_WIRE_SECONDARY_MIXES: usize = 15;
/// Highest valid mix number (Main is 1).
pub const MAX_WIRE_MIX_NUMBER: u8 = 16;

/// A one-based mix number in `1..=16`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WireMixNumber(u8);

impl WireMixNumber {
    /// The Main mix.
    pub const MAIN: Self = Self(1);

    /// Creates a mix number from its one-based operator-facing value.
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number <= MAX_WIRE_MIX_NUMBER {
            Some(Self(number))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn is_main(self) -> bool {
        self.0 == 1
    }
}

impl fmt::Display for WireMixNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One source a mix can show.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireSecondaryMixSource {
    Input(crate::WireInputId),
    Mix(WireMixNumber),
}

/// Whether a secondary mix keeps a Preview bus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireSecondaryMixMode {
    PreviewProgram,
    OutputOnly,
}

/// One configured secondary mix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireSecondaryMixConfig {
    pub number: WireMixNumber,
    pub name: String,
    pub mode: WireSecondaryMixMode,
    pub program: WireSecondaryMixSource,
    pub preview: Option<WireSecondaryMixSource>,
}

impl WireSecondaryMixConfig {
    /// Whether the configuration is internally consistent: a Preview exists
    /// exactly for a `preview_program` mix, and Program differs from it.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        match self.mode {
            WireSecondaryMixMode::PreviewProgram => {
                self.preview.is_some_and(|preview| preview != self.program)
            }
            WireSecondaryMixMode::OutputOnly => self.preview.is_none(),
        }
    }
}
