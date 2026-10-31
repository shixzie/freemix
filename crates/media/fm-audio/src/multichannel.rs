//! Multichannel layout catalog, sparse gain-matrix routing, and standard
//! downmix/upmix matrices (`AU-003`).
//!
//! Everything here operates on caller-owned planar `f32` buffers. Matrix
//! construction allocates once; [`GainMatrix::process`] validates the whole
//! operation before it writes any output and never allocates.
//!
//! # Channel order
//!
//! Standard layouts use the SMPTE ST 2036-2 / ITU-R BS.775 / WAVE channel
//! order that is common to broadcast embedding and file interchange:
//!
//! | Layout | Order |
//! |---|---|
//! | Mono (1.0) | `M` |
//! | Stereo (2.0) | `L R` |
//! | 2.1 | `L R LFE` |
//! | 5.1 | `L R C LFE Ls Rs` |
//! | 7.1 | `L R C LFE Ls Rs Lrs Rrs` (ITU-R BS.2051 `Lss/Rss` are `Ls/Rs`) |
//! | 16-channel discrete | `Ch1` … `Ch16` |

use core::fmt;

pub use fm_types::DiscreteChannel;
use fm_types::{Channel, ChannelLayout};

use crate::MAX_SAMPLES_PER_BLOCK;
use crate::channel_mapping::{ChannelMapping, ChannelMappingError, ChannelMappingRoute};

/// Maximum channel count of one [`MultichannelLayout`].
///
/// Equal to [`ChannelLayout::MAX_CHANNELS`], so every multichannel layout
/// converts to an `fm-types` layout and back.
pub const MAX_MULTICHANNEL_CHANNELS: usize = ChannelLayout::MAX_CHANNELS;
/// Maximum number of non-zero routes in one [`GainMatrix`].
pub const MAX_GAIN_MATRIX_ROUTES: usize = MAX_MULTICHANNEL_CHANNELS * MAX_MULTICHANNEL_CHANNELS;
/// Largest accepted absolute route gain (+24.08 dB).
pub const MAX_GAIN_MATRIX_GAIN: f32 = 16.0;
/// Largest accepted absolute input sample.
///
/// With at most 16 contributions per output, each bounded by
/// [`MAX_GAIN_MATRIX_GAIN`], every output is bounded by `262_144` and therefore
/// always finite.
pub const MAX_GAIN_MATRIX_INPUT_MAGNITUDE: f32 = 1024.0;

/// Exact `-3 dB` coefficient used by the standard matrices: `1/sqrt(2)` as `f32`
/// (`0.707_106_77`).
pub const MINUS_3_DB: f32 = core::f32::consts::FRAC_1_SQRT_2;

const DISCRETE_LABELS: [&str; MAX_MULTICHANNEL_CHANNELS] = [
    "Ch1", "Ch2", "Ch3", "Ch4", "Ch5", "Ch6", "Ch7", "Ch8", "Ch9", "Ch10", "Ch11", "Ch12", "Ch13",
    "Ch14", "Ch15", "Ch16",
];

/// Semantic label of one channel within a [`MultichannelLayout`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SpeakerLabel {
    Mono,
    Left,
    Right,
    Center,
    LowFrequencyEffects,
    /// Left surround (5.1) or left side surround (7.1).
    LeftSurround,
    /// Right surround (5.1) or right side surround (7.1).
    RightSurround,
    LeftRearSurround,
    RightRearSurround,
    /// Unlabelled discrete channel such as an SDI embedded slot.
    Discrete(DiscreteChannel),
}

impl SpeakerLabel {
    /// Conventional short ITU-R/SMPTE label (`M`, `L`, `R`, `C`, `LFE`, `Ls`,
    /// `Rs`, `Lrs`, `Rrs`, `Ch1`…`Ch16`).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mono => "M",
            Self::Left => "L",
            Self::Right => "R",
            Self::Center => "C",
            Self::LowFrequencyEffects => "LFE",
            Self::LeftSurround => "Ls",
            Self::RightSurround => "Rs",
            Self::LeftRearSurround => "Lrs",
            Self::RightRearSurround => "Rrs",
            Self::Discrete(channel) => DISCRETE_LABELS[channel.number() as usize - 1],
        }
    }

    /// Equivalent `fm-types` channel. The mapping is a bijection.
    #[must_use]
    pub const fn to_channel(self) -> Channel {
        match self {
            Self::Mono => Channel::Mono,
            Self::Left => Channel::Left,
            Self::Right => Channel::Right,
            Self::Center => Channel::Center,
            Self::LowFrequencyEffects => Channel::LowFrequency,
            Self::LeftSurround => Channel::LeftSurround,
            Self::RightSurround => Channel::RightSurround,
            Self::LeftRearSurround => Channel::LeftRearSurround,
            Self::RightRearSurround => Channel::RightRearSurround,
            Self::Discrete(channel) => Channel::Discrete(channel),
        }
    }

    #[must_use]
    pub const fn from_channel(channel: Channel) -> Self {
        match channel {
            Channel::Mono => Self::Mono,
            Channel::Left => Self::Left,
            Channel::Right => Self::Right,
            Channel::Center => Self::Center,
            Channel::LowFrequency => Self::LowFrequencyEffects,
            Channel::LeftSurround => Self::LeftSurround,
            Channel::RightSurround => Self::RightSurround,
            Channel::LeftRearSurround => Self::LeftRearSurround,
            Channel::RightRearSurround => Self::RightRearSurround,
            Channel::Discrete(channel) => Self::Discrete(channel),
        }
    }
}

impl fmt::Display for SpeakerLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Catalogued channel layouts.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StandardLayout {
    Mono,
    Stereo,
    TwoPointOne,
    FivePointOne,
    SevenPointOne,
    Discrete16,
}

impl StandardLayout {
    pub const ALL: [Self; 6] = [
        Self::Mono,
        Self::Stereo,
        Self::TwoPointOne,
        Self::FivePointOne,
        Self::SevenPointOne,
        Self::Discrete16,
    ];

    /// Conventional name: `1.0`, `2.0`, `2.1`, `5.1`, `7.1`, `16.0`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Mono => "1.0",
            Self::Stereo => "2.0",
            Self::TwoPointOne => "2.1",
            Self::FivePointOne => "5.1",
            Self::SevenPointOne => "7.1",
            Self::Discrete16 => "16.0",
        }
    }

    #[must_use]
    pub const fn channel_count(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::TwoPointOne => 3,
            Self::FivePointOne => 6,
            Self::SevenPointOne => 8,
            Self::Discrete16 => 16,
        }
    }

    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "indices and counts are at most 16"
    )]
    pub const fn layout(self) -> MultichannelLayout {
        use SpeakerLabel::{
            Center, Left, LeftRearSurround, LeftSurround, LowFrequencyEffects, Mono, Right,
            RightRearSurround, RightSurround,
        };
        let mut labels = [Mono; MAX_MULTICHANNEL_CHANNELS];
        match self {
            Self::Mono => {}
            Self::Stereo => {
                labels[0] = Left;
                labels[1] = Right;
            }
            Self::TwoPointOne => {
                labels[0] = Left;
                labels[1] = Right;
                labels[2] = LowFrequencyEffects;
            }
            Self::FivePointOne | Self::SevenPointOne => {
                labels[0] = Left;
                labels[1] = Right;
                labels[2] = Center;
                labels[3] = LowFrequencyEffects;
                labels[4] = LeftSurround;
                labels[5] = RightSurround;
                // Unused slots stay `Mono` so derived equality only sees the
                // active prefix.
                if matches!(self, Self::SevenPointOne) {
                    labels[6] = LeftRearSurround;
                    labels[7] = RightRearSurround;
                }
            }
            Self::Discrete16 => {
                let mut index = 0;
                while index < MAX_MULTICHANNEL_CHANNELS {
                    let Some(channel) = DiscreteChannel::new(index as u8 + 1) else {
                        unreachable!()
                    };
                    labels[index] = SpeakerLabel::Discrete(channel);
                    index += 1;
                }
            }
        }
        MultichannelLayout {
            labels,
            count: self.channel_count() as u8,
        }
    }
}

/// An ordered set of 1–16 distinct channel labels.
///
/// Invariant: storage past `count` is always `SpeakerLabel::Mono`, so derived
/// equality and hashing depend only on the active labels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MultichannelLayout {
    labels: [SpeakerLabel; MAX_MULTICHANNEL_CHANNELS],
    count: u8,
}

impl MultichannelLayout {
    /// Validates a custom layout.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty layout, more than 16 channels, or a
    /// repeated label.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "length is validated to be at most 16"
    )]
    pub fn new(labels: &[SpeakerLabel]) -> Result<Self, MultichannelError> {
        if !(1..=MAX_MULTICHANNEL_CHANNELS).contains(&labels.len()) {
            return Err(MultichannelError::ChannelCountOutOfRange {
                actual: labels.len(),
                maximum: MAX_MULTICHANNEL_CHANNELS,
            });
        }
        for (index, label) in labels.iter().enumerate() {
            if labels[index + 1..].contains(label) {
                return Err(MultichannelError::DuplicateLabel(*label));
            }
        }
        let mut stored = [SpeakerLabel::Mono; MAX_MULTICHANNEL_CHANNELS];
        stored[..labels.len()].copy_from_slice(labels);
        Ok(Self {
            labels: stored,
            count: labels.len() as u8,
        })
    }

    /// Converts an `fm-types` layout, preserving channel order.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::new`].
    pub fn from_channel_layout(layout: &ChannelLayout) -> Result<Self, MultichannelError> {
        let mut labels = [SpeakerLabel::Mono; MAX_MULTICHANNEL_CHANNELS];
        let channels = layout.channels();
        if channels.len() > MAX_MULTICHANNEL_CHANNELS {
            return Err(MultichannelError::ChannelCountOutOfRange {
                actual: channels.len(),
                maximum: MAX_MULTICHANNEL_CHANNELS,
            });
        }
        for (slot, channel) in labels.iter_mut().zip(channels) {
            *slot = SpeakerLabel::from_channel(*channel);
        }
        Self::new(&labels[..channels.len()])
    }

    /// Converts to an `fm-types` layout, preserving channel order.
    ///
    /// # Panics
    ///
    /// Never: a layout holds 1..=[`MAX_MULTICHANNEL_CHANNELS`] channels, which
    /// is exactly the range [`ChannelLayout::new`] accepts.
    #[must_use]
    pub fn to_channel_layout(&self) -> ChannelLayout {
        let channels = self
            .labels()
            .iter()
            .map(|label| label.to_channel())
            .collect();
        ChannelLayout::new(channels).expect("multichannel layouts hold 1..=16 channels")
    }

    #[must_use]
    pub fn labels(&self) -> &[SpeakerLabel] {
        &self.labels[..usize::from(self.count)]
    }

    #[must_use]
    pub const fn channel_count(&self) -> usize {
        self.count as usize
    }

    #[must_use]
    pub fn index_of(&self, label: SpeakerLabel) -> Option<usize> {
        self.labels()
            .iter()
            .position(|candidate| *candidate == label)
    }

    /// The catalogue entry this layout equals, if any.
    #[must_use]
    pub fn standard(&self) -> Option<StandardLayout> {
        StandardLayout::ALL
            .into_iter()
            .find(|standard| standard.layout() == *self)
    }
}

impl From<StandardLayout> for MultichannelLayout {
    fn from(value: StandardLayout) -> Self {
        value.layout()
    }
}

/// Which side of a matrix operation an error refers to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MultichannelSide {
    Input,
    Output,
}

/// Errors returned by multichannel layout, matrix, and embedding operations.
#[derive(Clone, Debug, PartialEq)]
pub enum MultichannelError {
    ChannelCountOutOfRange {
        actual: usize,
        maximum: usize,
    },
    DuplicateLabel(SpeakerLabel),
    TooManyRoutes {
        actual: usize,
        maximum: usize,
    },
    RouteChannelOutOfRange {
        route: usize,
        side: MultichannelSide,
        channel: usize,
        channels: usize,
    },
    DuplicateRoute {
        input: usize,
        output: usize,
    },
    InvalidGain {
        route: usize,
        gain: f32,
    },
    UnsupportedConversion {
        from: MultichannelLayout,
        to: MultichannelLayout,
    },
    PlaneCountMismatch {
        side: MultichannelSide,
        expected: usize,
        actual: usize,
    },
    PlaneLengthMismatch {
        side: MultichannelSide,
        plane: usize,
        expected: usize,
        actual: usize,
    },
    SampleCountOutOfRange {
        actual: usize,
        maximum: usize,
    },
    InputOutOfRange {
        channel: usize,
        sample: usize,
    },
    InvalidEmbeddedGroup(u8),
    InvalidEmbeddedChannel(u8),
    InvalidEmbeddedPair(u8),
    EmbeddedOverflow {
        first_slot: usize,
        channels: usize,
    },
    EmbeddedMisaligned {
        first_slot: usize,
    },
    EmbeddedOverlap {
        slot: usize,
    },
    UnknownEmbeddedAssignment(usize),
}

impl fmt::Display for MultichannelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelCountOutOfRange { actual, maximum } => {
                write!(formatter, "channel count {actual} is outside 1..={maximum}")
            }
            Self::DuplicateLabel(label) => write!(formatter, "layout repeats {label}"),
            Self::TooManyRoutes { actual, maximum } => {
                write!(
                    formatter,
                    "matrix has {actual} routes; maximum is {maximum}"
                )
            }
            Self::RouteChannelOutOfRange {
                route,
                side,
                channel,
                channels,
            } => write!(
                formatter,
                "route {route} {side:?} channel {channel} is outside 0..{channels}"
            ),
            Self::DuplicateRoute { input, output } => {
                write!(formatter, "matrix repeats route {input} -> {output}")
            }
            Self::InvalidGain { route, gain } => write!(
                formatter,
                "route {route} gain {gain} is not finite within ±{MAX_GAIN_MATRIX_GAIN}"
            ),
            Self::UnsupportedConversion { from, to } => write!(
                formatter,
                "no standard conversion from {} to {} channels",
                from.channel_count(),
                to.channel_count()
            ),
            Self::PlaneCountMismatch {
                side,
                expected,
                actual,
            } => write!(
                formatter,
                "{side:?} has {actual} planes; expected {expected}"
            ),
            Self::PlaneLengthMismatch {
                side,
                plane,
                expected,
                actual,
            } => write!(
                formatter,
                "{side:?} plane {plane} has {actual} samples; expected {expected}"
            ),
            Self::SampleCountOutOfRange { actual, maximum } => {
                write!(formatter, "{actual} samples exceed maximum {maximum}")
            }
            Self::InputOutOfRange { channel, sample } => write!(
                formatter,
                "input channel {channel} sample {sample} is non-finite or exceeds \
                 {MAX_GAIN_MATRIX_INPUT_MAGNITUDE}"
            ),
            Self::InvalidEmbeddedGroup(group) => {
                write!(formatter, "embedded group {group} is outside 1..=4")
            }
            Self::InvalidEmbeddedChannel(channel) => {
                write!(formatter, "embedded channel {channel} is outside 1..=4")
            }
            Self::InvalidEmbeddedPair(pair) => {
                write!(formatter, "embedded pair {pair} is outside 1..=2")
            }
            Self::EmbeddedOverflow {
                first_slot,
                channels,
            } => write!(
                formatter,
                "{channels} channels starting at slot {first_slot} exceed 16 embedded slots"
            ),
            Self::EmbeddedMisaligned { first_slot } => write!(
                formatter,
                "multichannel assignment must start on an odd (pair-aligned) slot, not {first_slot}"
            ),
            Self::EmbeddedOverlap { slot } => {
                write!(formatter, "embedded slot {slot} is already assigned")
            }
            Self::UnknownEmbeddedAssignment(index) => {
                write!(formatter, "embedded assignment {index} does not exist")
            }
        }
    }
}

impl std::error::Error for MultichannelError {}

/// One sparse matrix contribution: `output[output] += gain * input[input]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatrixRoute {
    pub input: usize,
    pub output: usize,
    pub gain: f32,
}

/// LFE treatment in the 5.1 → stereo downmix.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LfeHandling {
    /// LFE is not mixed (ITU-R BS.775 default).
    #[default]
    Discard,
    /// LFE is mixed into both L and R at [`MINUS_3_DB`].
    MinusThreeDb,
    /// LFE is mixed into both L and R at `1.0`.
    Unity,
}

impl LfeHandling {
    const fn coefficient(self) -> f64 {
        match self {
            Self::Discard => 0.0,
            Self::MinusThreeDb => core::f64::consts::FRAC_1_SQRT_2,
            Self::Unity => 1.0,
        }
    }
}

/// Coefficient normalization for standard downmixes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DownmixNormalization {
    /// Use the nominal coefficients unchanged.
    #[default]
    None,
    /// Divide every coefficient feeding an output by that output's sum of
    /// absolute coefficients when the sum exceeds one, so in-phase full-scale
    /// inputs cannot exceed full scale. Computed in `f64`, then rounded once
    /// to `f32`.
    PreventClipping,
}

/// Options for [`GainMatrix::downmix`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DownmixOptions {
    pub lfe: LfeHandling,
    pub normalization: DownmixNormalization,
}

/// A validated, immutable, sparse N-in → M-out gain matrix.
///
/// Routes are stored sorted by `(output, input)`, so accumulation order is
/// deterministic. Zero-gain routes are dropped at construction. Outputs
/// without routes are written as `+0.0`. The first route into an output is
/// written as `input * gain` (a bit-exact copy when `gain == 1.0`, including
/// signed zeros) and later routes are accumulated in order.
#[derive(Clone, Debug, PartialEq)]
pub struct GainMatrix {
    input_layout: MultichannelLayout,
    output_layout: MultichannelLayout,
    routes: Vec<MatrixRoute>,
}

impl GainMatrix {
    /// Validates a sparse matrix.
    ///
    /// # Errors
    ///
    /// Returns an error for more than [`MAX_GAIN_MATRIX_ROUTES`] routes,
    /// out-of-range channel indices, repeated input/output pairs, or gains
    /// that are non-finite or exceed [`MAX_GAIN_MATRIX_GAIN`] in magnitude.
    pub fn new(
        input_layout: MultichannelLayout,
        output_layout: MultichannelLayout,
        routes: &[MatrixRoute],
    ) -> Result<Self, MultichannelError> {
        if routes.len() > MAX_GAIN_MATRIX_ROUTES {
            return Err(MultichannelError::TooManyRoutes {
                actual: routes.len(),
                maximum: MAX_GAIN_MATRIX_ROUTES,
            });
        }
        let inputs = input_layout.channel_count();
        let outputs = output_layout.channel_count();
        let mut seen = [[false; MAX_MULTICHANNEL_CHANNELS]; MAX_MULTICHANNEL_CHANNELS];
        let mut compiled = Vec::with_capacity(routes.len());
        for (index, route) in routes.iter().enumerate() {
            if route.input >= inputs {
                return Err(MultichannelError::RouteChannelOutOfRange {
                    route: index,
                    side: MultichannelSide::Input,
                    channel: route.input,
                    channels: inputs,
                });
            }
            if route.output >= outputs {
                return Err(MultichannelError::RouteChannelOutOfRange {
                    route: index,
                    side: MultichannelSide::Output,
                    channel: route.output,
                    channels: outputs,
                });
            }
            if !route.gain.is_finite() || route.gain.abs() > MAX_GAIN_MATRIX_GAIN {
                return Err(MultichannelError::InvalidGain {
                    route: index,
                    gain: route.gain,
                });
            }
            if seen[route.input][route.output] {
                return Err(MultichannelError::DuplicateRoute {
                    input: route.input,
                    output: route.output,
                });
            }
            seen[route.input][route.output] = true;
            if route.gain != 0.0 {
                compiled.push(*route);
            }
        }
        compiled.sort_by_key(|route| (route.output, route.input));
        Ok(Self {
            input_layout,
            output_layout,
            routes: compiled,
        })
    }

    /// Unity passthrough of every channel in `layout`.
    #[must_use]
    pub fn identity(layout: MultichannelLayout) -> Self {
        let routes = (0..layout.channel_count())
            .map(|channel| MatrixRoute {
                input: channel,
                output: channel,
                gain: 1.0,
            })
            .collect();
        Self {
            input_layout: layout,
            output_layout: layout,
            routes,
        }
    }

    /// Unity routes between equal labels; output labels absent from the input
    /// are silent. This reorders without mixing.
    #[must_use]
    pub fn matching(input_layout: MultichannelLayout, output_layout: MultichannelLayout) -> Self {
        let routes = output_layout
            .labels()
            .iter()
            .enumerate()
            .filter_map(|(output, label)| {
                input_layout.index_of(*label).map(|input| MatrixRoute {
                    input,
                    output,
                    gain: 1.0,
                })
            })
            .collect();
        Self {
            input_layout,
            output_layout,
            routes,
        }
    }

    /// A standard downmix matrix.
    ///
    /// Nominal coefficients (`g = 1/sqrt(2)`, i.e. `-3 dB`):
    ///
    /// * 5.1 → 2.0 (ITU-R BS.775): `L' = L + g·C + g·Ls + k·LFE`,
    ///   `R' = R + g·C + g·Rs + k·LFE` where `k` is selected by
    ///   [`LfeHandling`] (`0`, `g`, or `1`). With
    ///   [`DownmixNormalization::PreventClipping`] and LFE discarded, the
    ///   coefficients become `1/(1+sqrt(2)) = sqrt(2)-1` for L/R and
    ///   `g/(1+sqrt(2)) = 1-g` for C and surrounds.
    /// * 7.1 → 5.1: `L R C LFE` pass at unity; `Ls' = g·Ls + g·Lrs`,
    ///   `Rs' = g·Rs + g·Rrs` (`0.5` each when normalized).
    /// * 2.0 → 1.0: `M = 0.5·L + 0.5·R` (already normalized).
    /// * 5.1 → 1.0 (composite): the 5.1 → stereo fold then `0.5·(L + R)`:
    ///   `M = 0.5·L + 0.5·R + g·C + (g/2)·Ls + (g/2)·Rs + k·LFE`.
    ///
    /// `options.lfe` only affects 5.1 output mixes.
    ///
    /// # Errors
    ///
    /// Returns [`MultichannelError::UnsupportedConversion`] for any other pair.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "coefficients are computed in f64 and rounded once to f32"
    )]
    pub fn downmix(
        from: StandardLayout,
        to: StandardLayout,
        options: DownmixOptions,
    ) -> Result<Self, MultichannelError> {
        const G: f64 = core::f64::consts::FRAC_1_SQRT_2;
        let mut routes = [(0_usize, 0_usize, 0.0_f64); 12];
        let count = match (from, to) {
            (StandardLayout::FivePointOne, StandardLayout::Stereo) => {
                let k = options.lfe.coefficient();
                let table = [
                    (0, 0, 1.0),
                    (2, 0, G),
                    (3, 0, k),
                    (4, 0, G),
                    (1, 1, 1.0),
                    (2, 1, G),
                    (3, 1, k),
                    (5, 1, G),
                ];
                routes[..table.len()].copy_from_slice(&table);
                table.len()
            }
            (StandardLayout::SevenPointOne, StandardLayout::FivePointOne) => {
                let table = [
                    (0, 0, 1.0),
                    (1, 1, 1.0),
                    (2, 2, 1.0),
                    (3, 3, 1.0),
                    (4, 4, G),
                    (6, 4, G),
                    (5, 5, G),
                    (7, 5, G),
                ];
                routes[..table.len()].copy_from_slice(&table);
                table.len()
            }
            (StandardLayout::Stereo, StandardLayout::Mono) => {
                let table = [(0, 0, 0.5), (1, 0, 0.5)];
                routes[..table.len()].copy_from_slice(&table);
                table.len()
            }
            // The composite of the BS.775 5.1 → stereo fold and 0.5·(L+R):
            // `M = 0.5·L + 0.5·R + g·C + (g/2)·Ls + (g/2)·Rs + k·LFE`.
            (StandardLayout::FivePointOne, StandardLayout::Mono) => {
                let k = options.lfe.coefficient();
                let table = [
                    (0, 0, 0.5),
                    (1, 0, 0.5),
                    (2, 0, G),
                    (3, 0, k),
                    (4, 0, 0.5 * G),
                    (5, 0, 0.5 * G),
                ];
                routes[..table.len()].copy_from_slice(&table);
                table.len()
            }
            _ => {
                return Err(MultichannelError::UnsupportedConversion {
                    from: from.layout(),
                    to: to.layout(),
                });
            }
        };
        let routes = &routes[..count];

        let mut sums = [0.0_f64; MAX_MULTICHANNEL_CHANNELS];
        for (_, output, gain) in routes {
            sums[*output] += gain.abs();
        }
        let mut compiled = [MatrixRoute {
            input: 0,
            output: 0,
            gain: 0.0,
        }; 12];
        for (slot, (input, output, gain)) in compiled.iter_mut().zip(routes) {
            let scale = match options.normalization {
                DownmixNormalization::PreventClipping if sums[*output] > 1.0 => sums[*output],
                _ => 1.0,
            };
            *slot = MatrixRoute {
                input: *input,
                output: *output,
                gain: (gain / scale) as f32,
            };
        }
        Self::new(from.layout(), to.layout(), &compiled[..count])
    }

    /// A Dolby Surround (Lt/Rt) matrix for 5.1 → 2.0.
    ///
    /// `Lt = L + g·C − g·Ls + k·LFE`, `Rt = R + g·C + g·Rs + k·LFE`, with
    /// `g = 1/sqrt(2)` and `k` selected by [`LfeHandling`]. The 90° phase
    /// shift Dolby applies to the surrounds is not modelled; the matrix's
    /// signs carry it. [`DownmixNormalization::PreventClipping`] divides
    /// each output's coefficients by their absolute sum when it exceeds one.
    ///
    /// # Errors
    ///
    /// Returns [`MultichannelError::UnsupportedConversion`] for any input
    /// other than 5.1.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "coefficients are computed in f64 and rounded once to f32"
    )]
    pub fn lt_rt(
        from: StandardLayout,
        options: DownmixOptions,
    ) -> Result<Self, MultichannelError> {
        const G: f64 = core::f64::consts::FRAC_1_SQRT_2;
        if from != StandardLayout::FivePointOne {
            return Err(MultichannelError::UnsupportedConversion {
                from: from.layout(),
                to: StandardLayout::Stereo.layout(),
            });
        }
        let k = options.lfe.coefficient();
        let table = [
            (0, 0, 1.0),
            (2, 0, G),
            (3, 0, k),
            (4, 0, -G),
            (1, 1, 1.0),
            (2, 1, G),
            (3, 1, k),
            (5, 1, G),
        ];
        let mut sums = [0.0_f64; MAX_MULTICHANNEL_CHANNELS];
        for (_, output, gain) in &table {
            sums[*output] += gain.abs();
        }
        let compiled = table.map(|(input, output, gain)| {
            let scale = match options.normalization {
                DownmixNormalization::PreventClipping if sums[output] > 1.0 => sums[output],
                _ => 1.0,
            };
            MatrixRoute {
                input,
                output,
                gain: (gain / scale) as f32,
            }
        });
        Self::new(
            from.layout(),
            StandardLayout::Stereo.layout(),
            &compiled,
        )
    }

    /// Upmix by duplication between catalogued layouts with more output than
    /// input channels.
    ///
    /// Every output label present in the input is copied at unity. Otherwise:
    /// a mono input is duplicated at unity into every non-LFE output; `Ls`
    /// takes `L`; `Rs` takes `R`; `Lrs` takes `Ls` (or `L`); `Rrs` takes `Rs`
    /// (or `R`); `C` and `LFE` stay silent. Discrete 16-channel output is only
    /// reachable from mono.
    ///
    /// # Errors
    ///
    /// Returns [`MultichannelError::UnsupportedConversion`] when the output
    /// does not have more channels, or for a non-mono → discrete request.
    pub fn upmix_duplicate(
        from: StandardLayout,
        to: StandardLayout,
    ) -> Result<Self, MultichannelError> {
        let unsupported = || MultichannelError::UnsupportedConversion {
            from: from.layout(),
            to: to.layout(),
        };
        if to.channel_count() <= from.channel_count()
            || (to == StandardLayout::Discrete16 && from != StandardLayout::Mono)
        {
            return Err(unsupported());
        }
        let input = from.layout();
        let output = to.layout();
        let mut routes = [MatrixRoute {
            input: 0,
            output: 0,
            gain: 0.0,
        }; MAX_MULTICHANNEL_CHANNELS];
        let mut count = 0;
        for (output_index, label) in output.labels().iter().enumerate() {
            let source = input.index_of(*label).or_else(|| {
                if from == StandardLayout::Mono {
                    return (*label != SpeakerLabel::LowFrequencyEffects).then_some(0);
                }
                match label {
                    SpeakerLabel::LeftSurround => input.index_of(SpeakerLabel::Left),
                    SpeakerLabel::RightSurround => input.index_of(SpeakerLabel::Right),
                    SpeakerLabel::LeftRearSurround => input
                        .index_of(SpeakerLabel::LeftSurround)
                        .or_else(|| input.index_of(SpeakerLabel::Left)),
                    SpeakerLabel::RightRearSurround => input
                        .index_of(SpeakerLabel::RightSurround)
                        .or_else(|| input.index_of(SpeakerLabel::Right)),
                    _ => None,
                }
            });
            if let Some(input_index) = source {
                routes[count] = MatrixRoute {
                    input: input_index,
                    output: output_index,
                    gain: 1.0,
                };
                count += 1;
            }
        }
        Self::new(input, output, &routes[..count])
    }

    #[must_use]
    pub const fn input_layout(&self) -> &MultichannelLayout {
        &self.input_layout
    }

    #[must_use]
    pub const fn output_layout(&self) -> &MultichannelLayout {
        &self.output_layout
    }

    /// Retained non-zero routes in `(output, input)` order.
    #[must_use]
    pub fn routes(&self) -> &[MatrixRoute] {
        &self.routes
    }

    /// Expresses this matrix as a semantic [`ChannelMapping`] between the
    /// equivalent `fm-types` layouts, so `AudioBlock`s and Master mixer strips
    /// can use catalogued up/downmixes. Each retained route becomes one
    /// mapping route with the same gain.
    ///
    /// # Errors
    ///
    /// Returns any [`ChannelMappingError`] from [`ChannelMapping::new`]. A
    /// validated matrix always satisfies its layout, route-count, uniqueness,
    /// and coefficient limits, so no error is expected in practice.
    pub fn to_channel_mapping(&self) -> Result<ChannelMapping, ChannelMappingError> {
        let inputs = self.input_layout.labels();
        let outputs = self.output_layout.labels();
        let routes = self
            .routes
            .iter()
            .map(|route| ChannelMappingRoute {
                source: inputs[route.input].to_channel(),
                destination: outputs[route.output].to_channel(),
                coefficient: route.gain,
            })
            .collect();
        ChannelMapping::new(
            self.input_layout.to_channel_layout(),
            self.output_layout.to_channel_layout(),
            routes,
        )
    }

    /// Gain from `input` to `output`, or `0.0` when unrouted.
    #[must_use]
    pub fn gain(&self, input: usize, output: usize) -> f32 {
        self.routes
            .iter()
            .find(|route| route.input == input && route.output == output)
            .map_or(0.0, |route| route.gain)
    }

    /// Renders caller-owned planar input into caller-owned planar output.
    ///
    /// All shape, length, and numeric validation completes before any output
    /// sample is written. Never allocates. Returns the processed sample count.
    ///
    /// # Errors
    ///
    /// Returns an error for plane count or length mismatches, more than
    /// [`MAX_SAMPLES_PER_BLOCK`] samples, or a non-finite input sample or one
    /// whose magnitude exceeds [`MAX_GAIN_MATRIX_INPUT_MAGNITUDE`].
    pub fn process<I, O>(&self, input: &[I], output: &mut [O]) -> Result<usize, MultichannelError>
    where
        I: AsRef<[f32]>,
        O: AsMut<[f32]>,
    {
        let samples = validate_planes(input, self.input_layout.channel_count())?;
        validate_output_planes(output, self.output_layout.channel_count(), samples)?;
        for (channel, plane) in input.iter().enumerate() {
            if let Some(sample) = plane.as_ref().iter().position(|value| {
                !value.is_finite() || value.abs() > MAX_GAIN_MATRIX_INPUT_MAGNITUDE
            }) {
                return Err(MultichannelError::InputOutOfRange { channel, sample });
            }
        }

        let mut routed = [false; MAX_MULTICHANNEL_CHANNELS];
        for route in &self.routes {
            routed[route.output] = true;
        }
        for (plane, routed) in output.iter_mut().zip(routed) {
            if !routed {
                plane.as_mut().fill(0.0);
            }
        }
        let mut previous_output = None;
        for route in &self.routes {
            let source = input[route.input].as_ref();
            let destination = output[route.output].as_mut();
            if previous_output == Some(route.output) {
                for (value, sample) in destination.iter_mut().zip(source) {
                    *value += sample * route.gain;
                }
            } else if route.gain.to_bits() == 1.0_f32.to_bits() {
                destination.copy_from_slice(source);
            } else {
                for (value, sample) in destination.iter_mut().zip(source) {
                    *value = sample * route.gain;
                }
            }
            previous_output = Some(route.output);
        }
        Ok(samples)
    }
}

pub(crate) fn validate_planes<I: AsRef<[f32]>>(
    input: &[I],
    expected: usize,
) -> Result<usize, MultichannelError> {
    if input.len() != expected {
        return Err(MultichannelError::PlaneCountMismatch {
            side: MultichannelSide::Input,
            expected,
            actual: input.len(),
        });
    }
    let samples = input.first().map_or(0, |plane| plane.as_ref().len());
    if samples > MAX_SAMPLES_PER_BLOCK {
        return Err(MultichannelError::SampleCountOutOfRange {
            actual: samples,
            maximum: MAX_SAMPLES_PER_BLOCK,
        });
    }
    for (plane, values) in input.iter().enumerate() {
        if values.as_ref().len() != samples {
            return Err(MultichannelError::PlaneLengthMismatch {
                side: MultichannelSide::Input,
                plane,
                expected: samples,
                actual: values.as_ref().len(),
            });
        }
    }
    Ok(samples)
}

pub(crate) fn validate_output_planes<O: AsMut<[f32]>>(
    output: &mut [O],
    expected: usize,
    samples: usize,
) -> Result<(), MultichannelError> {
    if output.len() != expected {
        return Err(MultichannelError::PlaneCountMismatch {
            side: MultichannelSide::Output,
            expected,
            actual: output.len(),
        });
    }
    for (plane, values) in output.iter_mut().enumerate() {
        let actual = values.as_mut().len();
        if actual != samples {
            return Err(MultichannelError::PlaneLengthMismatch {
                side: MultichannelSide::Output,
                plane,
                expected: samples,
                actual,
            });
        }
    }
    Ok(())
}
