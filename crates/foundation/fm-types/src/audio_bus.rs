//! Master plus seven auxiliary buses A–G (AU-002): the bus numbering and
//! the per-strip bus assignment shared by the project, the engine, and the
//! wire.
//!
//! Every input strip carries an [`AudioBusMask`]: whether it feeds Master
//! and each of aux buses A–G, like the bus buttons on a vMix strip. A new
//! strip feeds Master only.

use core::fmt;

/// Number of aux buses (A–G) in every project.
pub const AUX_BUS_COUNT: usize = 7;

/// One aux bus, numbered `1..=AUX_BUS_COUNT` and lettered A–G.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AuxBusNumber(u8);

impl AuxBusNumber {
    /// Every aux bus in ascending order.
    pub const ALL: [Self; AUX_BUS_COUNT] = [
        Self(1),
        Self(2),
        Self(3),
        Self(4),
        Self(5),
        Self(6),
        Self(7),
    ];

    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number as usize <= AUX_BUS_COUNT {
            Some(Self(number))
        } else {
            None
        }
    }

    /// The bus for `letter` (`A` through `G`).
    #[must_use]
    pub const fn from_letter(letter: char) -> Option<Self> {
        match letter {
            'A'..='G' => Some(Self(letter as u8 - b'A' + 1)),
            _ => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize - 1
    }

    /// The bus letter, `A` through `G`.
    #[must_use]
    pub const fn letter(self) -> char {
        (b'A' + self.0 - 1) as char
    }
}

impl fmt::Display for AuxBusNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "bus {}", self.letter())
    }
}

/// The buses one strip feeds: bit 0 is Master and bit `n` is aux bus `n`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AudioBusMask(u8);

impl AudioBusMask {
    /// Master only: a new strip's assignment.
    pub const MASTER: Self = Self(1);
    /// No bus at all.
    pub const NONE: Self = Self(0);

    /// The mask with exactly `bits` (every value is a valid assignment).
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether the strip feeds Master.
    #[must_use]
    pub const fn master(self) -> bool {
        self.0 & 1 != 0
    }

    /// Whether the strip feeds aux bus `bus`.
    #[must_use]
    pub const fn aux(self, bus: AuxBusNumber) -> bool {
        self.0 & (1 << bus.0) != 0
    }

    /// This mask with Master on or off.
    #[must_use]
    pub const fn with_master(self, on: bool) -> Self {
        if on {
            Self(self.0 | 1)
        } else {
            Self(self.0 & !1)
        }
    }

    /// This mask with aux bus `bus` on or off.
    #[must_use]
    pub const fn with_aux(self, bus: AuxBusNumber, on: bool) -> Self {
        let bit = 1 << bus.0;
        if on {
            Self(self.0 | bit)
        } else {
            Self(self.0 & !bit)
        }
    }

    /// The persisted and wire token: `M` for Master, then the letter of each
    /// aux bus in order (`MAC`), or `-` for none.
    #[must_use]
    pub fn token(self) -> String {
        if self.0 == 0 {
            return "-".to_owned();
        }
        let mut token = String::with_capacity(8);
        if self.master() {
            token.push('M');
        }
        for bus in AuxBusNumber::ALL {
            if self.aux(bus) {
                token.push(bus.letter());
            }
        }
        token
    }

    /// Parses exactly the canonical [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        if token == "-" {
            return Some(Self::NONE);
        }
        let mut mask = Self::NONE;
        let mut chars = token.chars().peekable();
        if chars.peek() == Some(&'M') {
            chars.next();
            mask = mask.with_master(true);
        }
        let mut previous = 0;
        for letter in chars {
            let bus = AuxBusNumber::from_letter(letter)?;
            if bus.get() <= previous {
                return None;
            }
            previous = bus.get();
            mask = mask.with_aux(bus, true);
        }
        (mask != Self::NONE).then_some(mask)
    }
}

impl Default for AudioBusMask {
    fn default() -> Self {
        Self::MASTER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_mask_tokens_are_canonical() {
        let a = AuxBusNumber::new(1).unwrap();
        let c = AuxBusNumber::new(3).unwrap();
        let g = AuxBusNumber::new(7).unwrap();
        assert_eq!(AudioBusMask::MASTER.token(), "M");
        assert_eq!(AudioBusMask::NONE.token(), "-");
        let mask = AudioBusMask::MASTER.with_aux(a, true).with_aux(c, true);
        assert_eq!(mask.token(), "MAC");
        assert_eq!(AudioBusMask::from_token("MAC"), Some(mask));
        let aux_only = AudioBusMask::NONE.with_aux(g, true);
        assert_eq!(aux_only.token(), "G");
        assert!(!aux_only.master());
        assert!(aux_only.aux(g));
        for bits in 0..=u8::MAX {
            let mask = AudioBusMask::from_bits(bits);
            assert_eq!(AudioBusMask::from_token(&mask.token()), Some(mask));
        }
        for bad in ["", "CA", "MM", "AA", "m", "H", "-A", "AM"] {
            assert_eq!(AudioBusMask::from_token(bad), None, "{bad}");
        }
    }

    #[test]
    fn bus_letters_round_trip() {
        for bus in AuxBusNumber::ALL {
            assert_eq!(AuxBusNumber::from_letter(bus.letter()), Some(bus));
        }
        assert_eq!(AuxBusNumber::new(0), None);
        assert_eq!(AuxBusNumber::new(8), None);
    }
}
