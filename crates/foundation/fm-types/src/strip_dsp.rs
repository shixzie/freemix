//! Per-strip native DSP settings (AU-005).
//!
//! Every input's Master strip, and the Master bus itself, carries one
//! [`StripDsp`] chain processed in a fixed order:
//!
//! ```text
//! delay -> polarity -> high-pass -> EQ (5 bands) -> gate -> expander -> compressor -> limiter
//! ```
//!
//! Each stage has its own enable switch (the polarity stage is the invert
//! switch itself) and the whole chain has a bypass.
//! Every value is an integer in a fixed unit (hertz, millidecibels,
//! thousandths, microseconds), so a chain compares, hashes, persists, and
//! travels on the wire exactly. [`StripDsp::validate`] checks every bound;
//! [`StripDsp::code`] and [`StripDsp::from_code`] are the canonical compact
//! text form (107 bytes packed big-endian as unpadded base64url, 143
//! characters) shared by the protocol, the CLI, and the Studio clipboard.
//!
//! The default chain has every stage disabled and is therefore transparent;
//! its parameters are sensible starting points for when an operator enables
//! a stage. [`StripDspPreset`] supplies voice, music, and Master starting
//! chains.

use core::fmt;

/// How many EQ bands a chain has.
pub const STRIP_EQ_BAND_COUNT: usize = 5;

/// High-pass corner bounds, in hertz.
pub const MIN_HIGH_PASS_HZ: u32 = 20;
pub const MAX_HIGH_PASS_HZ: u32 = 2_000;
/// EQ band frequency bounds, in hertz.
pub const MIN_EQ_FREQUENCY_HZ: u32 = 20;
pub const MAX_EQ_FREQUENCY_HZ: u32 = 20_000;
/// EQ band gain bound, in millidecibels (either sign).
pub const MAX_EQ_GAIN_MILLIDB: i32 = 24_000;
/// EQ band Q bounds, in thousandths.
pub const MIN_EQ_Q_MILLI: u32 = 100;
pub const MAX_EQ_Q_MILLI: u32 = 10_000;
/// Gate threshold bounds, in millidecibels.
pub const MIN_GATE_THRESHOLD_MILLIDB: i32 = -80_000;
/// Gate range bound (attenuation when closed), in millidecibels.
pub const MAX_GATE_RANGE_MILLIDB: u32 = 80_000;
/// Gate hold bound, in microseconds.
pub const MAX_GATE_HOLD_US: u32 = 1_000_000;
/// Gate release bound, in microseconds.
pub const MAX_GATE_RELEASE_US: u32 = 5_000_000;
/// Expander threshold lower bound, in millidecibels.
pub const MIN_EXPANDER_THRESHOLD_MILLIDB: i32 = -80_000;
/// Expander ratio bounds, in thousandths (1:1 to 20:1 below the threshold).
pub const MIN_EXPANDER_RATIO_MILLI: u32 = 1_000;
pub const MAX_EXPANDER_RATIO_MILLI: u32 = 20_000;
/// Expander range bound (deepest attenuation), in millidecibels.
pub const MAX_EXPANDER_RANGE_MILLIDB: u32 = 80_000;
/// Compressor threshold lower bound, in millidecibels.
pub const MIN_COMPRESSOR_THRESHOLD_MILLIDB: i32 = -60_000;
/// Compressor ratio bounds, in thousandths (1:1 to 20:1).
pub const MIN_COMPRESSOR_RATIO_MILLI: u32 = 1_000;
pub const MAX_COMPRESSOR_RATIO_MILLI: u32 = 20_000;
/// Compressor knee and makeup bound, in millidecibels.
pub const MAX_COMPRESSOR_KNEE_MILLIDB: u32 = 24_000;
pub const MAX_COMPRESSOR_MAKEUP_MILLIDB: u32 = 24_000;
/// Limiter ceiling lower bound, in millidecibels.
pub const MIN_LIMITER_CEILING_MILLIDB: i32 = -60_000;
/// Attack and release bounds shared by every dynamics stage, in
/// microseconds (0.1 ms to 1 s; the gate and expander releases reach 5 s).
pub const MIN_TIME_US: u32 = 100;
pub const MAX_TIME_US: u32 = 1_000_000;

/// Delay stage bound, in microseconds (one second).
pub const MAX_DELAY_US: u32 = 1_000_000;

/// Length of [`StripDsp::code`] in characters.
pub const STRIP_DSP_CODE_LEN: usize = 143;
const PACKED_BYTES: usize = 107;

/// The shape of one EQ band.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EqBandKind {
    /// A low shelf: boosts or cuts everything below the frequency.
    LowShelf,
    /// A bell around the frequency.
    #[default]
    Peak,
    /// A high shelf: boosts or cuts everything above the frequency.
    HighShelf,
}

impl EqBandKind {
    pub const ALL: [Self; 3] = [Self::LowShelf, Self::Peak, Self::HighShelf];

    /// The token: `low_shelf`, `peak`, or `high_shelf`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::LowShelf => "low_shelf",
            Self::Peak => "peak",
            Self::HighShelf => "high_shelf",
        }
    }

    /// The kind a token names.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.token() == token)
    }

    const fn index(self) -> u8 {
        match self {
            Self::LowShelf => 0,
            Self::Peak => 1,
            Self::HighShelf => 2,
        }
    }

    const fn from_index(index: u8) -> Option<Self> {
        match index {
            0 => Some(Self::LowShelf),
            1 => Some(Self::Peak),
            2 => Some(Self::HighShelf),
            _ => None,
        }
    }
}

/// The delay stage: holds the strip back by a fixed time, for example to
/// line a microphone up with a camera's late picture. The live chain
/// realizes it to the nearest sample.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StripDelay {
    pub enabled: bool,
    /// The delay, in microseconds (`0..=`[`MAX_DELAY_US`]).
    pub delay_us: u32,
}

/// The 12 dB/octave high-pass (low-cut) filter.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StripHighPass {
    pub enabled: bool,
    pub frequency_hz: u32,
}

/// One EQ band.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StripEqBand {
    pub enabled: bool,
    pub kind: EqBandKind,
    pub frequency_hz: u32,
    pub gain_millidb: i32,
    /// Bandwidth (peak) or slope (shelf) in thousandths.
    pub q_milli: u32,
}

/// The parametric EQ: a section switch and five bands.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StripEq {
    pub enabled: bool,
    pub bands: [StripEqBand; STRIP_EQ_BAND_COUNT],
}

/// The noise gate (downward expander with hold).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StripGate {
    pub enabled: bool,
    pub threshold_millidb: i32,
    /// How far the closed gate attenuates, in millidecibels.
    pub range_millidb: u32,
    pub attack_us: u32,
    pub hold_us: u32,
    pub release_us: u32,
}

/// The downward expander: below the threshold the level falls `ratio`
/// times faster than the input, attenuating by at most `range`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StripExpander {
    pub enabled: bool,
    pub threshold_millidb: i32,
    pub ratio_milli: u32,
    pub attack_us: u32,
    pub release_us: u32,
    /// The deepest attenuation, in millidecibels.
    pub range_millidb: u32,
}

/// The feed-forward compressor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StripCompressor {
    pub enabled: bool,
    pub threshold_millidb: i32,
    pub ratio_milli: u32,
    pub attack_us: u32,
    pub release_us: u32,
    pub knee_millidb: u32,
    pub makeup_millidb: u32,
}

/// The peak limiter with a hard ceiling.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StripLimiter {
    pub enabled: bool,
    pub ceiling_millidb: i32,
    pub release_us: u32,
}

/// One strip's complete DSP chain.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StripDsp {
    /// Bypasses the whole chain without losing its settings.
    pub bypass: bool,
    /// Delays the strip, ahead of every other stage.
    pub delay: StripDelay,
    /// Inverts the strip's polarity (every channel), ahead of the filters.
    pub invert_polarity: bool,
    pub high_pass: StripHighPass,
    pub eq: StripEq,
    pub gate: StripGate,
    pub expander: StripExpander,
    pub compressor: StripCompressor,
    pub limiter: StripLimiter,
}

impl Default for StripDsp {
    fn default() -> Self {
        Self::DEFAULT
    }
}

const fn band(kind: EqBandKind, frequency_hz: u32) -> StripEqBand {
    StripEqBand {
        enabled: true,
        kind,
        frequency_hz,
        gain_millidb: 0,
        q_milli: 707,
    }
}

impl StripDsp {
    /// Every stage off; parameters at their starting points.
    pub const DEFAULT: Self = Self {
        bypass: false,
        delay: StripDelay {
            enabled: false,
            delay_us: 0,
        },
        invert_polarity: false,
        high_pass: StripHighPass {
            enabled: false,
            frequency_hz: 80,
        },
        eq: StripEq {
            enabled: false,
            bands: [
                band(EqBandKind::LowShelf, 100),
                StripEqBand {
                    q_milli: 1_000,
                    ..band(EqBandKind::Peak, 250)
                },
                StripEqBand {
                    q_milli: 1_000,
                    ..band(EqBandKind::Peak, 1_000)
                },
                StripEqBand {
                    q_milli: 1_000,
                    ..band(EqBandKind::Peak, 4_000)
                },
                band(EqBandKind::HighShelf, 10_000),
            ],
        },
        gate: StripGate {
            enabled: false,
            threshold_millidb: -50_000,
            range_millidb: 40_000,
            attack_us: 1_000,
            hold_us: 50_000,
            release_us: 200_000,
        },
        expander: StripExpander {
            enabled: false,
            threshold_millidb: -40_000,
            ratio_milli: 2_000,
            attack_us: 1_000,
            release_us: 100_000,
            range_millidb: 20_000,
        },
        compressor: StripCompressor {
            enabled: false,
            threshold_millidb: -18_000,
            ratio_milli: 3_000,
            attack_us: 10_000,
            release_us: 150_000,
            knee_millidb: 6_000,
            makeup_millidb: 0,
        },
        limiter: StripLimiter {
            enabled: false,
            ceiling_millidb: -1_000,
            release_us: 50_000,
        },
    };

    /// Whether this is exactly [`Self::DEFAULT`].
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::DEFAULT
    }

    /// Whether any stage would change the signal: the chain is not
    /// bypassed and at least one stage is enabled.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        !self.bypass
            && (self.delay.enabled
                || self.invert_polarity
                || self.high_pass.enabled
                || self.eq.enabled
                || self.gate.enabled
                || self.expander.enabled
                || self.compressor.enabled
                || self.limiter.enabled)
    }

    /// Whether a dynamics stage (gate, expander, compressor, or limiter) is
    /// running and so reports gain reduction.
    #[must_use]
    pub const fn has_active_dynamics(&self) -> bool {
        !self.bypass
            && (self.gate.enabled
                || self.expander.enabled
                || self.compressor.enabled
                || self.limiter.enabled)
    }

    /// Checks every bound.
    ///
    /// # Errors
    ///
    /// Returns the first out-of-range field.
    pub fn validate(&self) -> Result<(), StripDspError> {
        let check = |ok: bool, field: &'static str| {
            if ok {
                Ok(())
            } else {
                Err(StripDspError { field })
            }
        };
        check(self.delay.delay_us <= MAX_DELAY_US, "delay.delay_us")?;
        check(
            (MIN_HIGH_PASS_HZ..=MAX_HIGH_PASS_HZ).contains(&self.high_pass.frequency_hz),
            "high_pass.frequency_hz",
        )?;
        for band in &self.eq.bands {
            check(
                (MIN_EQ_FREQUENCY_HZ..=MAX_EQ_FREQUENCY_HZ).contains(&band.frequency_hz),
                "eq.frequency_hz",
            )?;
            check(
                (-MAX_EQ_GAIN_MILLIDB..=MAX_EQ_GAIN_MILLIDB).contains(&band.gain_millidb),
                "eq.gain_millidb",
            )?;
            check(
                (MIN_EQ_Q_MILLI..=MAX_EQ_Q_MILLI).contains(&band.q_milli),
                "eq.q_milli",
            )?;
        }
        let gate = &self.gate;
        check(
            (MIN_GATE_THRESHOLD_MILLIDB..=0).contains(&gate.threshold_millidb),
            "gate.threshold_millidb",
        )?;
        check(
            gate.range_millidb <= MAX_GATE_RANGE_MILLIDB,
            "gate.range_millidb",
        )?;
        check(
            (MIN_TIME_US..=MAX_TIME_US).contains(&gate.attack_us),
            "gate.attack_us",
        )?;
        check(gate.hold_us <= MAX_GATE_HOLD_US, "gate.hold_us")?;
        check(
            (MIN_TIME_US..=MAX_GATE_RELEASE_US).contains(&gate.release_us),
            "gate.release_us",
        )?;
        let expander = &self.expander;
        check(
            (MIN_EXPANDER_THRESHOLD_MILLIDB..=0).contains(&expander.threshold_millidb),
            "expander.threshold_millidb",
        )?;
        check(
            (MIN_EXPANDER_RATIO_MILLI..=MAX_EXPANDER_RATIO_MILLI).contains(&expander.ratio_milli),
            "expander.ratio_milli",
        )?;
        check(
            (MIN_TIME_US..=MAX_TIME_US).contains(&expander.attack_us),
            "expander.attack_us",
        )?;
        check(
            (MIN_TIME_US..=MAX_GATE_RELEASE_US).contains(&expander.release_us),
            "expander.release_us",
        )?;
        check(
            expander.range_millidb <= MAX_EXPANDER_RANGE_MILLIDB,
            "expander.range_millidb",
        )?;
        let compressor = &self.compressor;
        check(
            (MIN_COMPRESSOR_THRESHOLD_MILLIDB..=0).contains(&compressor.threshold_millidb),
            "compressor.threshold_millidb",
        )?;
        check(
            (MIN_COMPRESSOR_RATIO_MILLI..=MAX_COMPRESSOR_RATIO_MILLI)
                .contains(&compressor.ratio_milli),
            "compressor.ratio_milli",
        )?;
        check(
            (MIN_TIME_US..=MAX_TIME_US).contains(&compressor.attack_us),
            "compressor.attack_us",
        )?;
        check(
            (MIN_TIME_US..=MAX_TIME_US).contains(&compressor.release_us),
            "compressor.release_us",
        )?;
        check(
            compressor.knee_millidb <= MAX_COMPRESSOR_KNEE_MILLIDB,
            "compressor.knee_millidb",
        )?;
        check(
            compressor.makeup_millidb <= MAX_COMPRESSOR_MAKEUP_MILLIDB,
            "compressor.makeup_millidb",
        )?;
        check(
            (MIN_LIMITER_CEILING_MILLIDB..=0).contains(&self.limiter.ceiling_millidb),
            "limiter.ceiling_millidb",
        )?;
        check(
            (MIN_TIME_US..=MAX_TIME_US).contains(&self.limiter.release_us),
            "limiter.release_us",
        )
    }

    /// The canonical compact text form: the chain packed big-endian into 107
    /// bytes and written as unpadded base64url ([`STRIP_DSP_CODE_LEN`]
    /// characters, letters, digits, `-`, and `_` only).
    ///
    /// The layout is a 16-bit flag word (bit 0 bypass, 1 high-pass, 2 EQ,
    /// 3 through 7 the bands, 8 gate, 9 compressor, 10 limiter, 11 polarity
    /// invert, 12 expander, 13 delay), the high-pass frequency (u16), five bands (kind
    /// u8 `0` low shelf, `1` peak, `2` high shelf; frequency u16; gain i16; Q
    /// u16), the gate (threshold i32, range u32, attack u32, hold u32,
    /// release u32), the expander (threshold i32, ratio u16, attack u32,
    /// release u32, range u32), the compressor (threshold i32, ratio u16, attack u32, release u32, knee
    /// u16, makeup u16), the limiter (ceiling i32, release u32), and the
    /// delay (microseconds u32).
    ///
    /// # Panics
    ///
    /// Panics when the chain is out of bounds; validate first.
    #[must_use]
    pub fn code(&self) -> String {
        assert!(
            self.validate().is_ok(),
            "strip DSP code of an invalid chain"
        );
        let mut bytes = Vec::with_capacity(PACKED_BYTES);
        let mut flags = u16::from(self.bypass)
            | u16::from(self.high_pass.enabled) << 1
            | u16::from(self.eq.enabled) << 2;
        for (index, band) in self.eq.bands.iter().enumerate() {
            flags |= u16::from(band.enabled) << (3 + index);
        }
        flags |= u16::from(self.gate.enabled) << 8
            | u16::from(self.compressor.enabled) << 9
            | u16::from(self.limiter.enabled) << 10
            | u16::from(self.invert_polarity) << 11
            | u16::from(self.expander.enabled) << 12
            | u16::from(self.delay.enabled) << 13;
        let narrow = |value: u32| u16::try_from(value).expect("validated value fits u16");
        bytes.extend_from_slice(&flags.to_be_bytes());
        bytes.extend_from_slice(&narrow(self.high_pass.frequency_hz).to_be_bytes());
        for band in &self.eq.bands {
            bytes.push(band.kind.index());
            bytes.extend_from_slice(&narrow(band.frequency_hz).to_be_bytes());
            bytes.extend_from_slice(
                &i16::try_from(band.gain_millidb)
                    .expect("validated gain fits i16")
                    .to_be_bytes(),
            );
            bytes.extend_from_slice(&narrow(band.q_milli).to_be_bytes());
        }
        let gate = &self.gate;
        bytes.extend_from_slice(&gate.threshold_millidb.to_be_bytes());
        for value in [
            gate.range_millidb,
            gate.attack_us,
            gate.hold_us,
            gate.release_us,
        ] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        let expander = &self.expander;
        bytes.extend_from_slice(&expander.threshold_millidb.to_be_bytes());
        bytes.extend_from_slice(&narrow(expander.ratio_milli).to_be_bytes());
        bytes.extend_from_slice(&expander.attack_us.to_be_bytes());
        bytes.extend_from_slice(&expander.release_us.to_be_bytes());
        bytes.extend_from_slice(&expander.range_millidb.to_be_bytes());
        let compressor = &self.compressor;
        bytes.extend_from_slice(&compressor.threshold_millidb.to_be_bytes());
        bytes.extend_from_slice(&narrow(compressor.ratio_milli).to_be_bytes());
        bytes.extend_from_slice(&compressor.attack_us.to_be_bytes());
        bytes.extend_from_slice(&compressor.release_us.to_be_bytes());
        bytes.extend_from_slice(&narrow(compressor.knee_millidb).to_be_bytes());
        bytes.extend_from_slice(&narrow(compressor.makeup_millidb).to_be_bytes());
        bytes.extend_from_slice(&self.limiter.ceiling_millidb.to_be_bytes());
        bytes.extend_from_slice(&self.limiter.release_us.to_be_bytes());
        bytes.extend_from_slice(&self.delay.delay_us.to_be_bytes());
        debug_assert_eq!(bytes.len(), PACKED_BYTES);
        crate::base64url::encode(&bytes)
    }

    /// Parses [`Self::code`] strictly: exact length, canonical base64url,
    /// no unknown flag bits or band kinds, and every bound checked.
    ///
    /// # Errors
    ///
    /// Returns [`StripDspError`] naming `code` or the out-of-range field.
    pub fn from_code(code: &str) -> Result<Self, StripDspError> {
        let invalid = StripDspError { field: "code" };
        if code.len() != STRIP_DSP_CODE_LEN {
            return Err(invalid);
        }
        let bytes = crate::base64url::decode(code).ok_or(invalid)?;
        if bytes.len() != PACKED_BYTES {
            return Err(invalid);
        }
        let mut reader = Reader { bytes: &bytes };
        let flags = reader.u16();
        if flags >> 14 != 0 {
            return Err(invalid);
        }
        let flag = |bit: u32| flags & (1 << bit) != 0;
        let high_pass = StripHighPass {
            enabled: flag(1),
            frequency_hz: u32::from(reader.u16()),
        };
        let mut bands = [StripEqBand {
            enabled: false,
            kind: EqBandKind::Peak,
            frequency_hz: 0,
            gain_millidb: 0,
            q_milli: 0,
        }; STRIP_EQ_BAND_COUNT];
        for (bit, band) in (3_u32..).zip(bands.iter_mut()) {
            *band = StripEqBand {
                enabled: flag(bit),
                kind: EqBandKind::from_index(reader.u8()).ok_or(invalid)?,
                frequency_hz: u32::from(reader.u16()),
                gain_millidb: i32::from(reader.i16()),
                q_milli: u32::from(reader.u16()),
            };
        }
        let gate = StripGate {
            enabled: flag(8),
            threshold_millidb: reader.i32(),
            range_millidb: reader.u32(),
            attack_us: reader.u32(),
            hold_us: reader.u32(),
            release_us: reader.u32(),
        };
        let expander = StripExpander {
            enabled: flag(12),
            threshold_millidb: reader.i32(),
            ratio_milli: u32::from(reader.u16()),
            attack_us: reader.u32(),
            release_us: reader.u32(),
            range_millidb: reader.u32(),
        };
        let compressor = StripCompressor {
            enabled: flag(9),
            threshold_millidb: reader.i32(),
            ratio_milli: u32::from(reader.u16()),
            attack_us: reader.u32(),
            release_us: reader.u32(),
            knee_millidb: u32::from(reader.u16()),
            makeup_millidb: u32::from(reader.u16()),
        };
        let limiter = StripLimiter {
            enabled: flag(10),
            ceiling_millidb: reader.i32(),
            release_us: reader.u32(),
        };
        let delay = StripDelay {
            enabled: flag(13),
            delay_us: reader.u32(),
        };
        let chain = Self {
            bypass: flag(0),
            delay,
            invert_polarity: flag(11),
            high_pass,
            eq: StripEq {
                enabled: flag(2),
                bands,
            },
            gate,
            expander,
            compressor,
            limiter,
        };
        chain.validate()?;
        if chain.code() != code {
            return Err(invalid);
        }
        Ok(chain)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let (head, rest) = self.bytes.split_at(N);
        self.bytes = rest;
        head.try_into().expect("split_at returned N bytes")
    }

    fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }

    fn u16(&mut self) -> u16 {
        u16::from_be_bytes(self.take())
    }

    fn i16(&mut self) -> i16 {
        i16::from_be_bytes(self.take())
    }

    fn u32(&mut self) -> u32 {
        u32::from_be_bytes(self.take())
    }

    fn i32(&mut self) -> i32 {
        i32::from_be_bytes(self.take())
    }
}

/// An out-of-range or malformed chain field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StripDspError {
    /// The dotted field name, or `code` for a malformed code.
    pub field: &'static str,
}

impl fmt::Display for StripDspError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.field == "code" {
            formatter.write_str("strip DSP code is malformed")
        } else {
            write!(formatter, "strip DSP {} is out of range", self.field)
        }
    }
}

impl std::error::Error for StripDspError {}

/// Which chain an edit targets: one input's strip or the Master bus.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StripDspTarget {
    Master,
    Input(crate::InputId),
}

/// A starting chain an operator loads onto a strip.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StripDspPreset {
    /// Flat: every stage off.
    Flat,
    /// A spoken voice: low cut, gentle gate, mud cut, presence, and
    /// medium compression.
    Voice,
    /// A podcast or dialogue host: tighter gate and firmer compression.
    Podcast,
    /// Music beds and playback: rumble cut and gentle glue compression.
    Music,
    /// A Master bus: light bus compression and a -1 dBFS limiter.
    Master,
}

impl StripDspPreset {
    pub const ALL: [Self; 5] = [
        Self::Flat,
        Self::Voice,
        Self::Podcast,
        Self::Music,
        Self::Master,
    ];

    /// The token: `flat`, `voice`, `podcast`, `music`, or `master`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Voice => "voice",
            Self::Podcast => "podcast",
            Self::Music => "music",
            Self::Master => "master",
        }
    }

    /// The preset a token names.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.token() == token)
    }

    /// The preset's chain.
    #[must_use]
    pub fn chain(self) -> StripDsp {
        let mut chain = StripDsp::DEFAULT;
        match self {
            Self::Flat => {}
            Self::Voice | Self::Podcast => {
                let podcast = self == Self::Podcast;
                chain.high_pass = StripHighPass {
                    enabled: true,
                    frequency_hz: 100,
                };
                chain.eq.enabled = true;
                chain.eq.bands[0].gain_millidb = -2_000;
                chain.eq.bands[0].frequency_hz = 150;
                chain.eq.bands[1].gain_millidb = -3_000;
                chain.eq.bands[1].frequency_hz = 300;
                chain.eq.bands[3].gain_millidb = 3_000;
                chain.eq.bands[3].frequency_hz = 3_500;
                chain.eq.bands[4].gain_millidb = 2_000;
                chain.eq.bands[4].frequency_hz = 10_000;
                chain.gate.enabled = true;
                chain.gate.threshold_millidb = if podcast { -45_000 } else { -55_000 };
                chain.gate.range_millidb = if podcast { 30_000 } else { 20_000 };
                chain.compressor.enabled = true;
                chain.compressor.threshold_millidb = if podcast { -22_000 } else { -18_000 };
                chain.compressor.ratio_milli = if podcast { 4_000 } else { 3_000 };
                chain.compressor.attack_us = 5_000;
                chain.compressor.release_us = 120_000;
                chain.compressor.makeup_millidb = if podcast { 4_000 } else { 2_000 };
                chain.limiter.enabled = podcast;
            }
            Self::Music => {
                chain.high_pass = StripHighPass {
                    enabled: true,
                    frequency_hz: 30,
                };
                chain.compressor.enabled = true;
                chain.compressor.threshold_millidb = -14_000;
                chain.compressor.ratio_milli = 2_000;
                chain.compressor.attack_us = 30_000;
                chain.compressor.release_us = 250_000;
                chain.compressor.knee_millidb = 10_000;
            }
            Self::Master => {
                chain.compressor.enabled = true;
                chain.compressor.threshold_millidb = -10_000;
                chain.compressor.ratio_milli = 2_000;
                chain.compressor.attack_us = 20_000;
                chain.compressor.release_us = 200_000;
                chain.compressor.knee_millidb = 6_000;
                chain.limiter.enabled = true;
                chain.limiter.ceiling_millidb = -1_000;
                chain.limiter.release_us = 50_000;
            }
        }
        chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid_transparent_and_round_trips() {
        let chain = StripDsp::DEFAULT;
        assert!(chain.validate().is_ok());
        assert!(!chain.is_active());
        let code = chain.code();
        assert_eq!(code.len(), STRIP_DSP_CODE_LEN);
        assert!(
            code.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        );
        assert_eq!(StripDsp::from_code(&code), Ok(chain));
    }

    #[test]
    fn every_preset_is_valid_and_round_trips() {
        for preset in StripDspPreset::ALL {
            let chain = preset.chain();
            assert!(chain.validate().is_ok(), "{preset:?}");
            assert_eq!(StripDsp::from_code(&chain.code()), Ok(chain));
            assert_eq!(StripDspPreset::from_token(preset.token()), Some(preset));
        }
        assert!(StripDspPreset::Master.chain().limiter.enabled);
    }

    #[test]
    fn extremes_round_trip() {
        let mut chain = StripDspPreset::Voice.chain();
        chain.bypass = true;
        chain.eq.bands[2] = StripEqBand {
            enabled: false,
            kind: EqBandKind::HighShelf,
            frequency_hz: MAX_EQ_FREQUENCY_HZ,
            gain_millidb: -MAX_EQ_GAIN_MILLIDB,
            q_milli: MAX_EQ_Q_MILLI,
        };
        chain.gate.threshold_millidb = MIN_GATE_THRESHOLD_MILLIDB;
        chain.gate.release_us = MAX_GATE_RELEASE_US;
        chain.invert_polarity = true;
        chain.expander = StripExpander {
            enabled: true,
            threshold_millidb: MIN_EXPANDER_THRESHOLD_MILLIDB,
            ratio_milli: MAX_EXPANDER_RATIO_MILLI,
            attack_us: MIN_TIME_US,
            release_us: MAX_GATE_RELEASE_US,
            range_millidb: MAX_EXPANDER_RANGE_MILLIDB,
        };
        chain.compressor.ratio_milli = MAX_COMPRESSOR_RATIO_MILLI;
        chain.limiter.ceiling_millidb = MIN_LIMITER_CEILING_MILLIDB;
        assert_eq!(StripDsp::from_code(&chain.code()), Ok(chain));
    }

    #[test]
    fn malformed_codes_and_out_of_range_values_are_refused() {
        let code = StripDsp::DEFAULT.code();
        assert!(StripDsp::from_code(&code[1..]).is_err());
        assert!(StripDsp::from_code(&format!("{code}A")).is_err());
        let mut bad = code.clone().into_bytes();
        bad[0] = b'+';
        assert!(StripDsp::from_code(core::str::from_utf8(&bad).unwrap()).is_err());
        // An unknown flag bit is refused.
        let mut unknown = String::from("_");
        unknown.push_str(&code[1..]);
        assert!(StripDsp::from_code(&unknown).is_err());

        let mut chain = StripDsp::DEFAULT;
        chain.high_pass.frequency_hz = 10;
        assert_eq!(
            chain.validate(),
            Err(StripDspError {
                field: "high_pass.frequency_hz"
            })
        );
        let mut chain = StripDsp::DEFAULT;
        chain.compressor.ratio_milli = 999;
        assert!(chain.validate().is_err());
        let mut chain = StripDsp::DEFAULT;
        chain.eq.bands[4].gain_millidb = 24_001;
        assert!(chain.validate().is_err());
        let mut chain = StripDsp::DEFAULT;
        chain.expander.ratio_milli = 20_001;
        assert_eq!(
            chain.validate(),
            Err(StripDspError {
                field: "expander.ratio_milli"
            })
        );
        let mut chain = StripDsp::DEFAULT;
        chain.expander.range_millidb = 80_001;
        assert!(chain.validate().is_err());
    }

    #[test]
    fn polarity_and_expander_make_a_chain_active() {
        let mut chain = StripDsp::DEFAULT;
        chain.invert_polarity = true;
        assert!(chain.is_active());
        assert!(!chain.has_active_dynamics());
        let decoded = StripDsp::from_code(&chain.code()).unwrap();
        assert!(decoded.invert_polarity);
        let mut chain = StripDsp::DEFAULT;
        chain.expander.enabled = true;
        assert!(chain.is_active());
        assert!(chain.has_active_dynamics());
        assert!(StripDsp::from_code(&chain.code()).unwrap().expander.enabled);
    }

    #[test]
    fn the_delay_stage_is_bounded_activates_the_chain_and_round_trips() {
        let mut chain = StripDsp::DEFAULT;
        chain.delay = StripDelay {
            enabled: true,
            delay_us: MAX_DELAY_US,
        };
        assert!(chain.is_active());
        assert!(!chain.has_active_dynamics());
        assert_eq!(StripDsp::from_code(&chain.code()), Ok(chain));
        chain.delay.enabled = false;
        chain.delay.delay_us = 40_000;
        assert!(!chain.is_active());
        assert_eq!(StripDsp::from_code(&chain.code()), Ok(chain));
        chain.delay.delay_us = MAX_DELAY_US + 1;
        assert_eq!(
            chain.validate(),
            Err(StripDspError {
                field: "delay.delay_us"
            })
        );
    }
}
