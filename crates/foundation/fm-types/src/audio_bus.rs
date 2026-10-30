//! Master plus seven auxiliary buses A–G (AU-002): the bus numbering and
//! the per-strip bus assignment shared by the project, the engine, and the
//! wire.
//!
//! Every input strip carries [`AudioSends`]: whether it feeds Master and each
//! of aux buses A–G, like the bus buttons on a vMix strip, and for every
//! enabled send its level in milli-dB and whether it taps before or after the
//! strip fader. A new strip feeds Master only, at unity, post-fader.
//!
//! [`AudioBusMask`] is the enable-only view used by surfaces that toggle bus
//! keys; converting from it yields unity post-fader sends.

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

/// Where an aux send taps the strip (AU-002).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SendTap {
    /// Before the strip fader; the strip's mute still applies.
    PreFader,
    /// After the strip fader.
    #[default]
    PostFader,
}

impl SendTap {
    /// The wire and persistence token: `pre` or `post`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::PreFader => "pre",
            Self::PostFader => "post",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "pre" => Some(Self::PreFader),
            "post" => Some(Self::PostFader),
            _ => None,
        }
    }
}

/// One strip's send to one aux bus (AU-002).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BusSendState {
    pub enabled: bool,
    pub level_millidb: i32,
    pub tap: SendTap,
}

impl BusSendState {
    /// Lowest accepted send level, -96 dBFS.
    pub const MIN_LEVEL_MILLIDB: i32 = -96_000;
    /// Highest accepted send level, +24 dBFS.
    pub const MAX_LEVEL_MILLIDB: i32 = 24_000;
    /// An enabled unity post-fader send.
    pub const UNITY: Self = Self {
        enabled: true,
        level_millidb: 0,
        tap: SendTap::PostFader,
    };
    /// A disabled unity post-fader send.
    pub const DISABLED: Self = Self {
        enabled: false,
        level_millidb: 0,
        tap: SendTap::PostFader,
    };

    /// Whether `level` is an accepted send level.
    #[must_use]
    pub const fn level_is_valid(level: i32) -> bool {
        level >= Self::MIN_LEVEL_MILLIDB && level <= Self::MAX_LEVEL_MILLIDB
    }
}

impl Default for BusSendState {
    fn default() -> Self {
        Self::UNITY
    }
}

/// Every send of one strip: Master (a plain enable; the strip fader is its
/// level) plus aux buses A–G, each with its own level and tap (AU-002).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AudioSends {
    master: bool,
    aux: [BusSendState; AUX_BUS_COUNT],
}

impl AudioSends {
    /// Master only: a new strip's sends.
    pub const MASTER: Self = Self {
        master: true,
        aux: [BusSendState::DISABLED; AUX_BUS_COUNT],
    };

    /// Whether the strip feeds Master.
    #[must_use]
    pub const fn master(self) -> bool {
        self.master
    }

    /// The send to aux bus `bus`.
    #[must_use]
    pub const fn aux(self, bus: AuxBusNumber) -> BusSendState {
        self.aux[bus.index()]
    }

    /// This send set with aux bus `bus`'s send replaced.
    #[must_use]
    pub const fn with_aux_send(mut self, bus: AuxBusNumber, send: BusSendState) -> Self {
        self.aux[bus.index()] = send;
        self
    }

    /// This send set with Master enabled or disabled.
    #[must_use]
    pub const fn with_master(mut self, on: bool) -> Self {
        self.master = on;
        self
    }

    /// This send set with aux bus `bus` enabled or disabled, keeping its
    /// level and tap.
    #[must_use]
    pub const fn with_aux(mut self, bus: AuxBusNumber, on: bool) -> Self {
        self.aux[bus.index()].enabled = on;
        self
    }

    /// The enable-only view of these sends.
    #[must_use]
    pub fn mask(self) -> AudioBusMask {
        let mut mask = AudioBusMask::NONE.with_master(self.master);
        for bus in AuxBusNumber::ALL {
            mask = mask.with_aux(bus, self.aux(bus).enabled);
        }
        mask
    }

    /// Unity post-fader sends for every bus `mask` enables.
    #[must_use]
    pub fn from_mask(mask: AudioBusMask) -> Self {
        let mut sends = Self::MASTER.with_master(mask.master());
        for bus in AuxBusNumber::ALL {
            sends = sends.with_aux(bus, mask.aux(bus));
        }
        sends
    }

    /// Validates every aux send level.
    ///
    /// # Errors
    ///
    /// Returns [`AudioSendsError`] for a level outside
    /// [`BusSendState::MIN_LEVEL_MILLIDB`]`..=`[`BusSendState::MAX_LEVEL_MILLIDB`].
    pub fn validate(self) -> Result<(), AudioSendsError> {
        for bus in AuxBusNumber::ALL {
            if !BusSendState::level_is_valid(self.aux(bus).level_millidb) {
                return Err(AudioSendsError { bus });
            }
        }
        Ok(())
    }

    /// Whether these are a new strip's sends: Master only, no aux bus.
    #[must_use]
    pub fn is_default(self) -> bool {
        self == Self::MASTER
    }

    /// The canonical token: `M` when Master is fed, then an entry for every
    /// enabled aux bus in order, or `-` for none.
    ///
    /// An aux entry is the bus letter, `@`, the level in milli-dB, and `p`
    /// when it taps pre-fader. A new strip is `M`; Master plus pre-fader
    /// bus A at -6 dB is `M,A@-6000p`.
    #[must_use]
    pub fn token(self) -> String {
        let mut token = String::with_capacity(64);
        if self.master {
            token.push('M');
        }
        for bus in AuxBusNumber::ALL {
            let send = self.aux(bus);
            if !send.enabled {
                continue;
            }
            if !token.is_empty() {
                token.push(',');
            }
            token.push(bus.letter());
            token.push('@');
            token.push_str(&send.level_millidb.to_string());
            if send.tap == SendTap::PreFader {
                token.push('p');
            }
        }
        if token.is_empty() {
            "-".to_owned()
        } else {
            token
        }
    }

    /// Parses exactly the canonical [`Self::token`]: `M` first, then aux
    /// entries in bus order, each bus at most once, levels in range, and `p`
    /// only for pre-fader.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        if token == "-" {
            return Some(Self::MASTER.with_master(false));
        }
        let mut sends = Self::MASTER.with_master(false);
        let mut previous: Option<u8> = None;
        for entry in token.split(',') {
            if entry == "M" {
                if previous.is_some() {
                    return None;
                }
                previous = Some(0);
                sends = sends.with_master(true);
                continue;
            }
            let (letter, rest) = entry.split_once('@')?;
            if letter.len() != 1 {
                return None;
            }
            let bus = AuxBusNumber::from_letter(letter.chars().next()?)?;
            if previous.is_some_and(|previous| bus.get() <= previous) {
                return None;
            }
            previous = Some(bus.get());
            let (level, tap) = match rest.strip_suffix('p') {
                Some(level) => (level, SendTap::PreFader),
                None => (rest, SendTap::PostFader),
            };
            let level: i32 = level.parse().ok()?;
            if !BusSendState::level_is_valid(level) {
                return None;
            }
            sends = sends.with_aux_send(
                bus,
                BusSendState {
                    enabled: true,
                    level_millidb: level,
                    tap,
                },
            );
        }
        Some(sends)
    }
}

impl Default for AudioSends {
    fn default() -> Self {
        Self::MASTER
    }
}

/// An out-of-range aux send level.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioSendsError {
    pub bus: AuxBusNumber,
}

impl fmt::Display for AudioSendsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} send level is out of range", self.bus)
    }
}

impl std::error::Error for AudioSendsError {}

#[cfg(test)]
mod send_tests {
    use super::*;

    #[test]
    fn sends_round_trip_through_the_canonical_token() {
        let a = AuxBusNumber::new(1).unwrap();
        let g = AuxBusNumber::new(7).unwrap();
        assert_eq!(AudioSends::MASTER.token(), "M");
        assert_eq!(AudioSends::from_token("M"), Some(AudioSends::MASTER));
        assert_eq!(
            AudioSends::MASTER.with_master(false).token(),
            "-",
            "no bus at all"
        );
        let sends = AudioSends::MASTER
            .with_aux_send(
                a,
                BusSendState {
                    enabled: true,
                    level_millidb: -6_000,
                    tap: SendTap::PreFader,
                },
            )
            .with_aux(g, true)
            .with_master(false);
        assert_eq!(sends.token(), "A@-6000p,G@0");
        assert_eq!(AudioSends::from_token("A@-6000p,G@0"), Some(sends));
        assert_eq!(sends.mask().token(), "AG");
        assert_eq!(
            AudioSends::from_mask(sends.mask()),
            AudioSends::MASTER
                .with_master(false)
                .with_aux(a, true)
                .with_aux(g, true)
        );
        assert!(AudioSends::MASTER.with_aux(a, true).aux(a).enabled);
        assert_eq!(AudioSends::MASTER.with_aux(a, true).aux(a).level_millidb, 0);
        assert!(!AudioSends::MASTER.aux(a).enabled);
        assert!(!AudioSends::MASTER.with_aux(a, false).aux(a).enabled);
        for bad in [
            "",
            "M,",
            "M,M",
            "A",
            "A@",
            "A@0@",
            "A@96001",
            "A@-96001",
            "M@0",
            "M,A@0,A@1",
            "A@0,M",
            "M,",
            "M,H@0",
            "M,A0",
            "m",
            "M, G@0",
        ] {
            assert_eq!(AudioSends::from_token(bad), None, "{bad}");
        }
    }

    #[test]
    fn send_levels_are_bounded() {
        let a = AuxBusNumber::new(1).unwrap();
        let mut sends = AudioSends::MASTER;
        sends.validate().unwrap();
        sends = sends.with_aux_send(
            a,
            BusSendState {
                enabled: true,
                level_millidb: BusSendState::MIN_LEVEL_MILLIDB,
                tap: SendTap::PostFader,
            },
        );
        sends.validate().unwrap();
        let invalid = sends.with_aux_send(
            a,
            BusSendState {
                enabled: true,
                level_millidb: BusSendState::MAX_LEVEL_MILLIDB + 1,
                tap: SendTap::PostFader,
            },
        );
        assert_eq!(invalid.validate(), Err(AudioSendsError { bus: a }));
        assert_eq!(SendTap::from_token("pre"), Some(SendTap::PreFader));
        assert_eq!(SendTap::from_token("post"), Some(SendTap::PostFader));
        assert_eq!(SendTap::from_token("PRE"), None);
    }
}
