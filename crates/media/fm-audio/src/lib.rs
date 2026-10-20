//! Deterministic, sample-accurate reference audio processing.
//!
//! This crate deliberately contains no device I/O, threads, or asynchronous
//! work. Its planar `f32` path is suitable for simulation and as a reference
//! against which later real-time backends can be tested.

use std::collections::BTreeMap;
use std::fmt;

use fm_types::{AudioFormat, ChannelLayout, FrameRate, InputId, SampleFormat, SampleRate};

mod automix;
mod bus_matrix;
mod channel_mapping;
mod clock_recalibration;
mod dsp;
mod embedded_audio;
mod gain_smoother;
mod live_source;
mod live_strip_dsp;
mod loudness;
mod monitor;
mod multichannel;
mod resampler;
mod sample_delay;
mod source_sync;
mod synchronizer;

pub use automix::{
    AutomixChannelSettings, AutomixChannelTelemetry, AutomixConfig, AutomixError, AutomixTelemetry,
    Automixer, MAX_AUTOMIX_CHANNELS, MAX_AUTOMIX_SMOOTHING_SAMPLES, MAX_AUTOMIX_WEIGHT,
};
pub use bus_matrix::{
    BUS_COUNT, BusDestination, BusMatrix, BusMatrixError, BusMatrixTelemetry, BusSend,
    BusStripInput, DestinationId, MAX_BUS_CHANNELS, MAX_BUS_DESTINATIONS, MAX_BUS_INPUT_MAGNITUDE,
    MAX_BUS_NAME_BYTES, MAX_BUS_STRIPS, MixBus, SendTap,
};
pub use channel_mapping::{
    ChannelMapping, ChannelMappingError, ChannelMappingRoute, ChannelMappingSide,
    MAX_CHANNEL_MAPPING_BYTES, MAX_CHANNEL_MAPPING_CHANNELS, MAX_CHANNEL_MAPPING_ROUTES,
};
pub use clock_recalibration::{
    ClockRecalibrationError, ClockRecalibrationPolicy, ClockRecalibrationTelemetry,
    ClockRecalibrationUpdate, MAX_CLOCK_DRIFT_PPM, MAX_CLOCK_RECALIBRATION_OBSERVATIONS,
};
pub use dsp::{
    Biquad, BiquadCoefficients, BiquadKind, BiquadSettings, Compressor, DspError, DspMeter,
    DspMeterReading, Expander, Gate, Limiter, MAX_EQ_BANDS, ParametricEq, Polarity, StripDsp,
};
pub use embedded_audio::{
    EMBEDDED_CHANNELS, EMBEDDED_CHANNELS_PER_GROUP, EMBEDDED_GROUPS, EmbeddedAssignment,
    EmbeddedAudioMap, EmbeddedPair, EmbeddedSlot, deembed_group, deembed_mono, deembed_pair,
};
pub use gain_smoother::{
    GainSmoother, GainSmootherConfig, GainSmootherError, GainSmootherTelemetry,
    MAX_GAIN_SMOOTHER_RAMP_SAMPLES,
};
pub use live_source::{
    AV_SMOOTHING_SECONDS, AV_TOLERANCE_FRAMES, LivePull, LiveSourceClock, LiveSourceConfig,
    LiveSourceTelemetry, MIN_LIVE_LATENCY_FRAMES, MIN_LIVE_TARGET_SAMPLES, PictureAction,
};
pub use live_strip_dsp::{
    DspDynamicsReading, InPlacePlanes, LiveDspIo, LiveStripDsp, SUB_BLOCK as LIVE_DSP_SUB_BLOCK,
};
pub use loudness::{ABSOLUTE_GATE_LUFS, LoudnessMeter, LoudnessReading};
pub use monitor::{
    MAX_MONITOR_AUX_BUSES, MAX_MONITOR_CHANNELS, MAX_MONITOR_LEVEL_DB, MAX_MONITOR_STRIPS,
    MAX_TALKBACK_GAIN_DB, MAX_TALKBACK_ROUTES, MONITOR_DIM_DB, MonitorBus, MonitorDeviceId,
    MonitorError, MonitorSource, MonitorTap, MonitorTelemetry, TalkbackDestination, TalkbackFeed,
    TalkbackRoute,
};
pub use multichannel::{
    DiscreteChannel, DownmixNormalization, DownmixOptions, GainMatrix, LfeHandling,
    MAX_GAIN_MATRIX_GAIN, MAX_GAIN_MATRIX_INPUT_MAGNITUDE, MAX_GAIN_MATRIX_ROUTES,
    MAX_MULTICHANNEL_CHANNELS, MINUS_3_DB, MatrixRoute, MultichannelError, MultichannelLayout,
    MultichannelSide, SpeakerLabel, StandardLayout,
};
pub use resampler::{
    ENGINE_SAMPLE_RATE, ENGINE_SAMPLE_RATE_HZ, MAX_RESAMPLER_DECIMATION, MAX_RESAMPLER_RATE_HZ,
    MIN_RESAMPLER_RATE_HZ, ResamplerError, SINC_CUTOFF_FRACTION, SINC_KAISER_BETA, SINC_PHASES,
    SINC_ZERO_CROSSINGS, SincKernel,
};
pub use sample_delay::{
    MAX_SAMPLE_DELAY_BYTES, MAX_SAMPLE_DELAY_SAMPLES, SampleDelay, SampleDelayError,
    SampleDelaySide,
};
pub use source_sync::{
    DiscontinuityPolicy, DriftControlConfig, DriftController, JumpAction, MAX_DECLICK_SAMPLES,
    MAX_DRIFT_CORRECTION_PPM, MAX_SOURCE_SYNC_CAPACITY_SAMPLES, MAX_SOURCE_SYNC_PULL_SAMPLES,
    MAX_SOURCE_SYNC_PUSH_SAMPLES, PullReport, PushContinuity, PushReport, SourceSync,
    SourceSyncConfig, SourceSyncError, SourceSyncState, SourceSyncTelemetry,
};
pub use synchronizer::{
    AudioCadenceOrigin, AudioRenderPlan, AudioSilenceSpan, AudioSynchronizerError,
    AudioSynchronizerLimits, AudioSynchronizerTelemetry, BufferLimit, ClockMappedAudioSynchronizer,
    MAX_SYNCHRONIZER_BLOCKS, MAX_SYNCHRONIZER_BYTES, MAX_SYNCHRONIZER_OUTPUT_SAMPLES,
    MAX_SYNCHRONIZER_SAMPLES, MasterAudioInterval, SynchronizerDiscontinuity, SynchronizerLimit,
};

/// Maximum number of channels accepted by a block or mixer.
pub const MAX_CHANNELS: usize = 32;
/// Maximum number of samples per channel accepted in one operation.
pub const MAX_SAMPLES_PER_BLOCK: usize = 48_000;
/// Maximum duration of a scheduled gain ramp, in samples.
pub const MAX_RAMP_SAMPLES: usize = 48_000;
/// Maximum bytes of sample-delay history retained by one [`MasterMixer`].
///
/// The limit is four times the maximum retained by one [`SampleDelay`], which
/// prevents an unbounded number of independently delayed logical strips.
pub const MAX_MASTER_MIXER_DELAY_BYTES: usize = MAX_SAMPLE_DELAY_BYTES * 4;
/// Lowest non-silent gain accepted by [`Gain::from_db`].
pub const MIN_GAIN_DB: f32 = -120.0;
/// Highest gain accepted by [`Gain::from_db`].
pub const MAX_GAIN_DB: f32 = 24.0;

/// Errors returned by the bounded reference audio path.
#[derive(Clone, Debug, PartialEq)]
pub enum AudioError {
    AudioBlock(fm_frame::AudioBlockError),
    ChannelMapping(ChannelMappingError),
    UnsupportedSampleFormat(SampleFormat),
    ChannelCountOutOfRange(usize),
    SampleCountOutOfRange(usize),
    PlaneCountMismatch {
        expected: usize,
        actual: usize,
    },
    PlaneLengthMismatch {
        plane: usize,
        expected: usize,
        actual: usize,
    },
    MeterCountMismatch {
        expected: usize,
        actual: usize,
    },
    NonFiniteSample {
        channel: usize,
        sample: usize,
    },
    InvalidGainDb(f32),
    InvalidLinearGain(f32),
    InvalidFrequency(f32),
    InvalidAmplitude(f32),
    ChannelIndexOutOfRange {
        channel: usize,
        channels: usize,
    },
    MappingLayoutMismatch,
    FormatMismatch,
    SampleCountMismatch {
        expected: usize,
        actual: usize,
    },
    TimingSampleCountMismatch {
        samples: usize,
        sample_rate_hz: u32,
        duration_nanos: u64,
    },
    DuplicateInput(InputId),
    UnknownInput(InputId),
    RampTooLong(usize),
    InvalidSourceGain {
        start_numerator: u32,
        end_numerator: u32,
        denominator: u32,
    },
    CadenceBlockTooLarge(u128),
    /// Making `parent` the parent of `group` would nest a group inside
    /// itself.
    GroupCycle {
        group: MixerGroup,
        parent: MixerGroup,
    },
    /// A strip or Master DSP chain was refused (AU-005).
    Dsp(DspError),
    /// A render longer than an active strip DSP chain's scratch, which is
    /// sized at configuration time and never grows on the audio thread
    /// ([`MasterMixer::set_dsp_block_capacity`]).
    DspBlockOverCapacity {
        samples: usize,
        capacity: usize,
    },
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AudioBlock(error) => write!(formatter, "canonical audio block error: {error}"),
            Self::ChannelMapping(error) => write!(formatter, "channel mapping error: {error}"),
            Self::UnsupportedSampleFormat(format) => {
                write!(formatter, "unsupported audio sample format: {format:?}")
            }
            Self::ChannelCountOutOfRange(count) => {
                write!(
                    formatter,
                    "channel count {count} is outside 1..={MAX_CHANNELS}"
                )
            }
            Self::SampleCountOutOfRange(count) => write!(
                formatter,
                "sample count {count} exceeds the per-block limit of {MAX_SAMPLES_PER_BLOCK}"
            ),
            Self::PlaneCountMismatch { expected, actual } => {
                write!(formatter, "expected {expected} planes, received {actual}")
            }
            Self::PlaneLengthMismatch {
                plane,
                expected,
                actual,
            } => write!(
                formatter,
                "plane {plane} has {actual} samples; expected {expected}"
            ),
            Self::MeterCountMismatch { expected, actual } => write!(
                formatter,
                "meter buffer has {actual} channels; expected {expected}"
            ),
            Self::NonFiniteSample { channel, sample } => {
                write!(
                    formatter,
                    "sample {sample} in channel {channel} is not finite"
                )
            }
            Self::InvalidGainDb(db) => write!(formatter, "invalid gain in dB: {db}"),
            Self::InvalidLinearGain(gain) => write!(formatter, "invalid linear gain: {gain}"),
            Self::InvalidFrequency(frequency) => {
                write!(formatter, "invalid sine frequency: {frequency}")
            }
            Self::InvalidAmplitude(amplitude) => {
                write!(formatter, "invalid impulse amplitude: {amplitude}")
            }
            Self::ChannelIndexOutOfRange { channel, channels } => write!(
                formatter,
                "channel index {channel} is outside a {channels}-channel layout"
            ),
            Self::MappingLayoutMismatch => {
                formatter.write_str("channel mapping does not match the configured layouts")
            }
            Self::FormatMismatch => formatter.write_str("audio format or layout mismatch"),
            Self::SampleCountMismatch { expected, actual } => write!(
                formatter,
                "audio block has {actual} samples per channel; expected {expected}"
            ),
            Self::TimingSampleCountMismatch {
                samples,
                sample_rate_hz,
                duration_nanos,
            } => write!(
                formatter,
                "audio timing duration {duration_nanos} ns does not represent {samples} samples at {sample_rate_hz} Hz"
            ),
            Self::DuplicateInput(id) => write!(formatter, "duplicate audio input {id}"),
            Self::UnknownInput(id) => write!(formatter, "unknown audio input {id}"),
            Self::RampTooLong(samples) => write!(
                formatter,
                "gain ramp of {samples} samples exceeds the limit of {MAX_RAMP_SAMPLES}"
            ),
            Self::InvalidSourceGain {
                start_numerator,
                end_numerator,
                denominator,
            } => write!(
                formatter,
                "source gain {start_numerator}/{denominator}..{end_numerator}/{denominator} is outside 0.0..=1.0"
            ),
            Self::CadenceBlockTooLarge(samples) => write!(
                formatter,
                "video cadence requires up to {samples} audio samples in one block"
            ),
            Self::GroupCycle { group, parent } => write!(
                formatter,
                "group {} cannot nest inside group {}: groups must not form a cycle",
                group.number(),
                parent.number()
            ),
            Self::Dsp(error) => write!(formatter, "strip DSP error: {error}"),
            Self::DspBlockOverCapacity { samples, capacity } => write!(
                formatter,
                "a render of {samples} samples exceeds the strip DSP block capacity of {capacity}"
            ),
        }
    }
}

impl std::error::Error for AudioError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AudioBlock(error) => Some(error),
            Self::ChannelMapping(error) => Some(error),
            _ => None,
        }
    }
}

impl From<fm_frame::AudioBlockError> for AudioError {
    fn from(value: fm_frame::AudioBlockError) -> Self {
        Self::AudioBlock(value)
    }
}

impl From<ChannelMappingError> for AudioError {
    fn from(value: ChannelMappingError) -> Self {
        Self::ChannelMapping(value)
    }
}

/// Errors returned while configuring retained per-input Master delay.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MasterMixerDelayError {
    UnknownInput(InputId),
    SampleDelay(SampleDelayError),
    ByteCountOverflow,
    BudgetExceeded {
        requested: usize,
        retained: usize,
        maximum: usize,
    },
}

impl fmt::Display for MasterMixerDelayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownInput(id) => write!(formatter, "unknown audio input {id}"),
            Self::SampleDelay(error) => write!(formatter, "input sample delay error: {error}"),
            Self::ByteCountOverflow => {
                formatter.write_str("Master mixer delay byte accounting overflow")
            }
            Self::BudgetExceeded {
                requested,
                retained,
                maximum,
            } => write!(
                formatter,
                "input sample delay needs {requested} bytes with {retained} already retained; Master mixer limit is {maximum} bytes"
            ),
        }
    }
}

impl std::error::Error for MasterMixerDelayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SampleDelay(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SampleDelayError> for MasterMixerDelayError {
    fn from(value: SampleDelayError) -> Self {
        Self::SampleDelay(value)
    }
}

/// A validated gain value.
///
/// Silence is represented by zero linear gain and negative infinity in dB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gain(f32);

impl Gain {
    pub const UNITY: Self = Self(1.0);
    pub const SILENCE: Self = Self(0.0);

    /// Creates a gain from decibels.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidGainDb`] for NaN, positive infinity, or a
    /// finite value outside [`MIN_GAIN_DB`] through [`MAX_GAIN_DB`]. Negative
    /// infinity is accepted as silence.
    pub fn from_db(db: f32) -> Result<Self, AudioError> {
        if db == f32::NEG_INFINITY {
            return Ok(Self::SILENCE);
        }
        if !db.is_finite() || !(MIN_GAIN_DB..=MAX_GAIN_DB).contains(&db) {
            return Err(AudioError::InvalidGainDb(db));
        }
        Ok(Self(10.0_f32.powf(db / 20.0)))
    }

    /// Creates a gain from a linear multiplier.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidLinearGain`] unless the value corresponds
    /// to the accepted dB range, or is exactly zero.
    pub fn from_linear(linear: f32) -> Result<Self, AudioError> {
        if linear == 0.0 {
            return Ok(Self::SILENCE);
        }
        if !linear.is_finite() || linear < 0.0 {
            return Err(AudioError::InvalidLinearGain(linear));
        }
        let db = 20.0 * linear.log10();
        if !(MIN_GAIN_DB..=MAX_GAIN_DB).contains(&db) {
            return Err(AudioError::InvalidLinearGain(linear));
        }
        Ok(Self(linear))
    }

    #[must_use]
    pub const fn linear(self) -> f32 {
        self.0
    }

    #[must_use]
    pub fn db(self) -> f32 {
        if self.0 == 0.0 {
            f32::NEG_INFINITY
        } else {
            20.0 * self.0.log10()
        }
    }
}

/// Exact stereo balance in one-hundredth of a percent.
///
/// Balance attenuates the opposite destination channel: full left mutes Right,
/// full right mutes Left, and Center leaves both at unity. Other destination
/// channel labels are unchanged.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Balance(i32);

impl Balance {
    pub const MIN_BASIS_POINTS: i32 = -10_000;
    pub const MAX_BASIS_POINTS: i32 = 10_000;
    pub const CENTER: Self = Self(0);

    #[must_use]
    pub const fn from_basis_points(value: i32) -> Option<Self> {
        if value >= Self::MIN_BASIS_POINTS && value <= Self::MAX_BASIS_POINTS {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn basis_points(self) -> i32 {
        self.0
    }

    #[allow(clippy::cast_precision_loss)]
    fn normalized(self) -> f32 {
        self.0 as f32 / Self::MAX_BASIS_POINTS as f32
    }
}

/// Exact mono pan position in one-hundredth of a percent.
///
/// Pan *positions* a mono source in the stereo image, while [`Balance`]
/// *trims* the two sides of a signal that is already stereo. A strip has both:
/// pan acts only when the strip's source layout is mono, and balance then
/// acts on the destination channels exactly as it does for any strip.
///
/// # Pan law
///
/// Pan uses the -3 dB constant-power (sine/cosine) law, referenced to 0 dB at
/// center. For the normalized position `p` in `-1.0..=1.0`:
///
/// ```text
/// theta  = (p + 1) * pi / 4
/// gain_L = sqrt(2) * cos(theta)
/// gain_R = sqrt(2) * sin(theta)
/// ```
///
/// - Center leaves both sides at unity, so a centered mono strip keeps its
///   level and a project that never pans is unaffected.
/// - `gain_L^2 + gain_R^2 = 2` at every position: the acoustic power is
///   constant across the sweep, so a source does not dip or swell as it
///   moves.
/// - A hard pan is +3 dB (`sqrt(2)`) on the panned side and silence on the
///   other; equivalently, the center sits 3 dB below a hard pan.
///
/// Destination labels other than Left and Right (center, LFE, surrounds)
/// are unchanged.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Pan(i32);

impl Pan {
    pub const MIN_BASIS_POINTS: i32 = -10_000;
    pub const MAX_BASIS_POINTS: i32 = 10_000;
    pub const CENTER: Self = Self(0);

    #[must_use]
    pub const fn from_basis_points(value: i32) -> Option<Self> {
        if value >= Self::MIN_BASIS_POINTS && value <= Self::MAX_BASIS_POINTS {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn basis_points(self) -> i32 {
        self.0
    }

    #[allow(clippy::cast_precision_loss)]
    fn normalized(self) -> f32 {
        self.0 as f32 / Self::MAX_BASIS_POINTS as f32
    }

    /// The exact pan-law gains `(left, right)` at this position.
    #[must_use]
    pub fn gains(self) -> (f32, f32) {
        pan_law(self.normalized())
    }
}

/// The documented [`Pan`] law at normalized position `position`.
fn pan_law(position: f32) -> (f32, f32) {
    let theta = (f64::from(position) + 1.0) * std::f64::consts::FRAC_PI_4;
    #[allow(clippy::cast_possible_truncation)]
    (
        (std::f64::consts::SQRT_2 * theta.cos()) as f32,
        (std::f64::consts::SQRT_2 * theta.sin()) as f32,
    )
}

/// The pan-law gain of `gains` (`(left, right)`) toward `destination`.
const fn pan_gain(gains: (f32, f32), destination: fm_types::Channel) -> f32 {
    match destination {
        fm_types::Channel::Left => gains.0,
        fm_types::Channel::Right => gains.1,
        _ => 1.0,
    }
}

/// Number of VCA-style strip groups on the Master mixer.
pub const MIXER_GROUP_COUNT: usize = 4;

/// One VCA-style strip group, numbered `1..=MIXER_GROUP_COUNT`.
///
/// A group carries no audio of its own. It scales, mutes, and solos the
/// strips assigned to it: a member's effective gain is its own fader times
/// the group fader (their dB values add), a member is audible only while
/// neither the strip nor its group is muted, and soloing a group solos every
/// member. Groups nest: a group may have a parent group, and a member then
/// also follows every ancestor (the effective gain is the product of the
/// strip fader and every group fader on the chain; any muted ancestor mutes
/// and any soloed ancestor solos). Parents never form a cycle, so a chain is
/// at most [`MIXER_GROUP_COUNT`] groups deep. Balance, pan, delay, and
/// follow-video remain per strip.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MixerGroup(u8);

impl MixerGroup {
    /// Every group in ascending order.
    pub const ALL: [Self; MIXER_GROUP_COUNT] = [Self(1), Self(2), Self(3), Self(4)];

    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number as usize <= MIXER_GROUP_COUNT {
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
    pub const fn index(self) -> usize {
        self.0 as usize - 1
    }
}

/// User-visible state of one [`MixerGroup`] fader.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupState {
    pub gain: Gain,
    pub muted: bool,
}

impl Default for GroupState {
    fn default() -> Self {
        Self {
            gain: Gain::UNITY,
            muted: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct GroupStrip {
    state: GroupState,
    ramp: GainRamp,
    soloed: bool,
    parent: Option<MixerGroup>,
}

impl GroupStrip {
    const fn unity() -> Self {
        Self {
            state: GroupState {
                gain: Gain::UNITY,
                muted: false,
            },
            ramp: GainRamp::immediate(Gain::UNITY),
            soloed: false,
            parent: None,
        }
    }
}

/// How a soloed strip is heard (AU-001).
///
/// - [`SoloMode::InPlace`] (solo-in-place, the default) gates Program: while
///   any strip is soloed, every other strip is silent in Master, and the
///   monitor bus follows Master.
/// - [`SoloMode::Pfl`] (pre-fade listen) leaves Program untouched and routes
///   each soloed strip's pre-fader signal (after delay and channel mapping,
///   before the fader, group faders, balance, pan, mute, follow-video, and
///   source envelopes) to the monitor bus.
/// - [`SoloMode::Afl`] (after-fade listen) leaves Program untouched and
///   routes each soloed strip's post-fader signal (strip and group faders,
///   balance, and pan applied; mute, follow-video, and source envelopes not)
///   to the monitor bus.
///
/// In PFL and AFL the monitor bus follows Master while nothing is soloed.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SoloMode {
    #[default]
    InPlace,
    Pfl,
    Afl,
}

impl SoloMode {
    /// Every mode in display order.
    pub const ALL: [Self; 3] = [Self::InPlace, Self::Pfl, Self::Afl];

    /// Whether a solo is heard on the monitor bus rather than gating Program.
    #[must_use]
    pub const fn listens(self) -> bool {
        !matches!(self, Self::InPlace)
    }
}

/// Which part of a strip one mixing pass renders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StripTap {
    /// The full Program contribution.
    Program,
    /// Pre-fader listen: unity gain, no balance, pan, or source envelope.
    PreFader,
    /// After-fader listen: faders, balance, and pan, no source envelope.
    AfterFader,
}

/// An immutable, owned planar `f32` DSP/generator block.
///
/// Timed media interchange uses [`fm_frame::AudioBlock`]; this type is for
/// deterministic signal generation and untimed DSP calls.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioBlock {
    format: AudioFormat,
    planes: Vec<Vec<f32>>,
    samples: usize,
}

impl AudioBlock {
    /// Validates and owns a set of channel planes.
    ///
    /// # Errors
    ///
    /// Returns an error for non-`F32` formats, excessive dimensions,
    /// non-rectangular planes, non-finite samples, or a plane/layout mismatch.
    pub fn from_planar(format: AudioFormat, planes: Vec<Vec<f32>>) -> Result<Self, AudioError> {
        validate_format(&format)?;
        let channels = format.channels.channels().len();
        if planes.len() != channels {
            return Err(AudioError::PlaneCountMismatch {
                expected: channels,
                actual: planes.len(),
            });
        }
        let samples = planes.first().map_or(0, Vec::len);
        validate_sample_count(samples)?;
        for (channel, plane) in planes.iter().enumerate() {
            if plane.len() != samples {
                return Err(AudioError::PlaneLengthMismatch {
                    plane: channel,
                    expected: samples,
                    actual: plane.len(),
                });
            }
            if let Some(sample) = plane.iter().position(|value| !value.is_finite()) {
                return Err(AudioError::NonFiniteSample { channel, sample });
            }
        }
        Ok(Self {
            format,
            planes,
            samples,
        })
    }

    /// Creates a zero-filled block.
    ///
    /// # Errors
    ///
    /// Returns an error when the format or requested block size is unsupported.
    pub fn silence(format: AudioFormat, samples: usize) -> Result<Self, AudioError> {
        validate_format(&format)?;
        validate_sample_count(samples)?;
        let channels = format.channels.channels().len();
        Ok(Self {
            format,
            planes: vec![vec![0.0; samples]; channels],
            samples,
        })
    }

    #[must_use]
    pub const fn format(&self) -> &AudioFormat {
        &self.format
    }

    #[must_use]
    pub const fn samples(&self) -> usize {
        self.samples
    }

    #[must_use]
    pub fn channels(&self) -> usize {
        self.planes.len()
    }

    #[must_use]
    pub fn planes(&self) -> &[Vec<f32>] {
        &self.planes
    }

    #[must_use]
    pub fn plane(&self, channel: usize) -> Option<&[f32]> {
        self.planes.get(channel).map(Vec::as_slice)
    }
}

/// Deterministic source of planar blocks.
pub trait AudioGenerator {
    /// Generates the next contiguous block.
    ///
    /// # Errors
    ///
    /// Returns an error when `samples` exceeds [`MAX_SAMPLES_PER_BLOCK`].
    fn generate(&mut self, samples: usize) -> Result<AudioBlock, AudioError>;
}

/// A deterministic silence source.
#[derive(Clone, Debug)]
pub struct SilenceGenerator {
    format: AudioFormat,
}

impl SilenceGenerator {
    /// Creates a silence generator.
    ///
    /// # Errors
    ///
    /// Returns an error when `format` is not a supported planar float format.
    pub fn new(format: AudioFormat) -> Result<Self, AudioError> {
        validate_format(&format)?;
        Ok(Self { format })
    }
}

impl AudioGenerator for SilenceGenerator {
    fn generate(&mut self, samples: usize) -> Result<AudioBlock, AudioError> {
        AudioBlock::silence(self.format.clone(), samples)
    }
}

/// A phase-continuous deterministic sine source.
#[derive(Clone, Debug)]
pub struct SineGenerator {
    format: AudioFormat,
    frequency_hz: f32,
    amplitude: Gain,
    phase_radians: f32,
    sample_cursor: u64,
}

impl SineGenerator {
    /// Creates a sine generator whose channels carry identical samples.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported format or a frequency outside zero
    /// through the inclusive Nyquist frequency.
    #[allow(clippy::cast_precision_loss)]
    pub fn new(
        format: AudioFormat,
        frequency_hz: f32,
        amplitude: Gain,
    ) -> Result<Self, AudioError> {
        validate_format(&format)?;
        let nyquist = format.sample_rate.hertz() as f32 / 2.0;
        if !frequency_hz.is_finite() || !(0.0..=nyquist).contains(&frequency_hz) {
            return Err(AudioError::InvalidFrequency(frequency_hz));
        }
        Ok(Self {
            format,
            frequency_hz,
            amplitude,
            phase_radians: 0.0,
            sample_cursor: 0,
        })
    }

    pub fn reset(&mut self) {
        self.phase_radians = 0.0;
        self.sample_cursor = 0;
    }

    #[must_use]
    pub const fn sample_cursor(&self) -> u64 {
        self.sample_cursor
    }
}

impl AudioGenerator for SineGenerator {
    #[allow(clippy::cast_precision_loss)]
    fn generate(&mut self, samples: usize) -> Result<AudioBlock, AudioError> {
        validate_sample_count(samples)?;
        let channels = self.format.channels.channels().len();
        let mut plane = Vec::with_capacity(samples);
        let phase_increment =
            std::f32::consts::TAU * self.frequency_hz / self.format.sample_rate.hertz() as f32;
        for _ in 0..samples {
            plane.push(self.phase_radians.sin() * self.amplitude.linear());
            self.phase_radians =
                (self.phase_radians + phase_increment).rem_euclid(std::f32::consts::TAU);
        }
        self.sample_cursor = self.sample_cursor.saturating_add(samples as u64);
        AudioBlock::from_planar(self.format.clone(), vec![plane; channels])
    }
}

/// A deterministic one-shot impulse source.
#[derive(Clone, Debug)]
pub struct ImpulseGenerator {
    format: AudioFormat,
    channel: usize,
    impulse_sample: u64,
    amplitude: f32,
    sample_cursor: u64,
}

impl ImpulseGenerator {
    /// Creates a source with one impulse at an absolute sample offset.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported format, channel, or non-finite
    /// amplitude.
    pub fn new(
        format: AudioFormat,
        channel: usize,
        impulse_sample: u64,
        amplitude: f32,
    ) -> Result<Self, AudioError> {
        validate_format(&format)?;
        let channels = format.channels.channels().len();
        if channel >= channels {
            return Err(AudioError::ChannelIndexOutOfRange { channel, channels });
        }
        if !amplitude.is_finite() {
            return Err(AudioError::InvalidAmplitude(amplitude));
        }
        Ok(Self {
            format,
            channel,
            impulse_sample,
            amplitude,
            sample_cursor: 0,
        })
    }

    pub fn reset(&mut self) {
        self.sample_cursor = 0;
    }
}

impl AudioGenerator for ImpulseGenerator {
    fn generate(&mut self, samples: usize) -> Result<AudioBlock, AudioError> {
        let mut block = AudioBlock::silence(self.format.clone(), samples)?;
        let end = self.sample_cursor.saturating_add(samples as u64);
        if (self.sample_cursor..end).contains(&self.impulse_sample) {
            let offset = usize::try_from(self.impulse_sample - self.sample_cursor)
                .expect("impulse offset is bounded by the block size");
            block.planes[self.channel][offset] = self.amplitude;
        }
        self.sample_cursor = end;
        Ok(block)
    }
}

/// User-visible state of one input strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputState {
    pub gain: Gain,
    pub balance: Balance,
    /// Mono source position; see [`Pan`] for the law and when it applies.
    pub pan: Pan,
    pub muted: bool,
    pub soloed: bool,
    pub follow_video: bool,
    /// The VCA-style group this strip belongs to, if any.
    pub group: Option<MixerGroup>,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            gain: Gain::UNITY,
            balance: Balance::CENTER,
            pan: Pan::CENTER,
            muted: false,
            soloed: false,
            follow_video: false,
            group: None,
        }
    }
}

/// Exact rational source gain endpoints for one Master sample interval.
///
/// The first rendered sample advances one of `samples` equal steps from the
/// start endpoint and the final sample is exactly the end endpoint. This is
/// the same endpoint convention used by input-strip gain ramps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceGain {
    start_numerator: u32,
    end_numerator: u32,
    denominator: u32,
}

impl SourceGain {
    pub const UNITY: Self = Self {
        start_numerator: 1,
        end_numerator: 1,
        denominator: 1,
    };

    /// Creates a linear source gain from exact rational endpoints.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidSourceGain`] for a zero denominator or an
    /// endpoint greater than one.
    pub const fn new(
        start_numerator: u32,
        end_numerator: u32,
        denominator: u32,
    ) -> Result<Self, AudioError> {
        if denominator == 0 || start_numerator > denominator || end_numerator > denominator {
            return Err(AudioError::InvalidSourceGain {
                start_numerator,
                end_numerator,
                denominator,
            });
        }
        Ok(Self {
            start_numerator,
            end_numerator,
            denominator,
        })
    }

    #[must_use]
    pub const fn start_numerator(self) -> u32 {
        self.start_numerator
    }

    #[must_use]
    pub const fn end_numerator(self) -> u32 {
        self.end_numerator
    }

    #[must_use]
    pub const fn denominator(self) -> u32 {
        self.denominator
    }

    #[allow(clippy::cast_possible_truncation)]
    fn at_sample(self, sample: usize, samples: usize) -> f32 {
        let denominator = f64::from(self.denominator);
        let start = f64::from(self.start_numerator) / denominator;
        let end = f64::from(self.end_numerator) / denominator;
        let step = f64::from(u32::try_from(sample + 1).expect("sample count is bounded"));
        let steps = f64::from(u32::try_from(samples).expect("sample count is bounded"));
        (start + (end - start) * (step / steps)) as f32
    }
}

#[derive(Clone, Copy, Debug)]
struct GainRamp {
    current: f32,
    target: f32,
    remaining: u16,
}

/// A per-sample ramp of a normalized balance or pan position.
#[derive(Clone, Copy, Debug)]
struct PositionRamp {
    current: f32,
    target: f32,
    remaining: u16,
}

impl PositionRamp {
    const fn immediate(value: f32) -> Self {
        Self {
            current: value,
            target: value,
            remaining: 0,
        }
    }

    fn set(&mut self, target: f32, samples: usize) {
        self.target = target;
        self.remaining = u16::try_from(samples).expect("validated ramp length fits in u16");
        if samples == 0 {
            self.current = self.target;
        }
    }

    fn next(&mut self) -> f32 {
        if self.remaining != 0 {
            self.current += (self.target - self.current) / f32::from(self.remaining);
            self.remaining -= 1;
            if self.remaining == 0 {
                self.current = self.target;
            }
        }
        self.current
    }
}

impl GainRamp {
    const fn immediate(gain: Gain) -> Self {
        Self {
            current: gain.linear(),
            target: gain.linear(),
            remaining: 0,
        }
    }

    fn set(&mut self, target: Gain, samples: usize) {
        self.target = target.linear();
        self.remaining = u16::try_from(samples).expect("validated ramp length fits in u16");
        if samples == 0 {
            self.current = self.target;
        }
    }

    fn next(&mut self) -> f32 {
        if self.remaining != 0 {
            self.current += (self.target - self.current) / f32::from(self.remaining);
            self.remaining -= 1;
            if self.remaining == 0 {
                self.current = self.target;
            }
        }
        self.current
    }
}

#[derive(Clone, Debug)]
struct InputStrip {
    format: AudioFormat,
    mapping: ChannelMapping,
    state: InputState,
    ramp: GainRamp,
    balance_ramp: PositionRamp,
    pan_ramp: PositionRamp,
    delay: SampleDelay,
    /// The strip's DSP chain (AU-005), created by the first non-default
    /// chain and kept so later edits crossfade.
    dsp: Option<Box<StripDspSlot>>,
}

/// One strip's live DSP chain and its processed-sample scratch.
#[derive(Clone, Debug)]
struct StripDspSlot {
    live: LiveStripDsp,
    /// `live` as a render found it, restored when the render is rejected
    /// after the chain ran (preallocated: the copy never allocates).
    checkpoint: LiveStripDsp,
    /// Processed samples, channel-major with stride `capacity`.
    scratch: Vec<f32>,
    capacity: usize,
    /// Whether the last render routed the processed samples.
    active: bool,
}

impl StripDspSlot {
    fn new(live: LiveStripDsp, capacity: usize) -> Self {
        let channels = live.channels();
        Self {
            checkpoint: live.clone(),
            live,
            scratch: vec![0.0; channels * capacity],
            capacity,
            active: false,
        }
    }

    /// The processed sample `sample` of source channel `channel`, when the
    /// last render routed processed samples.
    fn sample(&self, channel: usize, sample: usize) -> Option<f32> {
        self.active
            .then(|| self.scratch[channel * self.capacity + sample])
    }
}

/// Feeds a strip's delayed input into its DSP and its scratch.
struct StripDspIo<'a, B> {
    delay: &'a SampleDelay,
    block: Option<&'a B>,
    scratch: &'a mut [f32],
    capacity: usize,
}

impl<B: MixerBlockView> LiveDspIo for StripDspIo<'_, B> {
    fn read(&mut self, channel: usize, start: usize, buffer: &mut [f32]) {
        let input = self.block.map(|block| block.planes()[channel].as_slice());
        for (offset, value) in buffer.iter_mut().enumerate() {
            *value = self.delay.preview_sample(channel, start + offset, input);
        }
    }

    fn write(&mut self, channel: usize, start: usize, buffer: &[f32]) {
        let begin = channel * self.capacity + start;
        self.scratch[begin..begin + buffer.len()].copy_from_slice(buffer);
    }
}

/// Default processed-sample capacity per strip DSP chain: 100 ms at 48 kHz.
pub const DEFAULT_DSP_BLOCK_CAPACITY: usize = 4_800;

impl InputStrip {
    /// Whether pan applies: the strip's source is mono.
    fn pans(&self) -> bool {
        self.format.channels.channels().len() == 1
    }
}

/// The per-sample ramps one strip steps while it is mixed.
#[derive(Clone, Copy)]
struct StripRamps {
    gain: GainRamp,
    balance: PositionRamp,
    pan: PositionRamp,
    /// The fader ramps of the strip's group and every ancestor group.
    groups: [Option<GainRamp>; MIXER_GROUP_COUNT],
}

impl StripRamps {
    /// Advances every ramp one sample and returns `(gain, balance, pan)`,
    /// where `gain` already includes every group fader on the chain.
    fn next(&mut self) -> (f32, f32, f32) {
        let groups = self
            .groups
            .iter_mut()
            .flatten()
            .fold(1.0, |gain, ramp| gain * ramp.next());
        (
            self.gain.next() * groups,
            self.balance.next(),
            self.pan.next(),
        )
    }
}

/// A strip's group membership resolved along the parent chain.
#[derive(Clone, Copy)]
struct ResolvedGroups {
    muted: bool,
    soloed: bool,
    ramps: [Option<GainRamp>; MIXER_GROUP_COUNT],
}

/// Master summing behavior when a sample exceeds the normalized range.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ClippingPolicy {
    /// Preserve the mathematical sum, including values outside `-1.0..=1.0`.
    Allow,
    /// Hard-clip the Master output to `-1.0..=1.0`.
    #[default]
    Clamp,
}

/// Peak and RMS values for one channel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChannelMeter {
    pub peak: f32,
    pub rms: f32,
}

/// Meter readings in channel-layout order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeterReadings {
    channels: Vec<ChannelMeter>,
}

impl MeterReadings {
    #[must_use]
    pub fn channels(&self) -> &[ChannelMeter] {
        &self.channels
    }
}

/// Meter readings for one configured input strip.
#[derive(Clone, Debug, PartialEq)]
pub struct InputMeterReadings {
    pub input: InputId,
    pub meters: MeterReadings,
}

/// The monitor (headphone) bus of one render, in Master channel order.
///
/// It carries Master unless [`Self::solo`] is set, in which case it carries
/// the sum of the soloed strips' PFL or AFL taps (see [`SoloMode`]).
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorOutput {
    pub planes: Vec<Vec<f32>>,
    pub meters: MeterReadings,
    pub solo: bool,
}

/// Result of one Master bus render.
#[derive(Clone, Debug, PartialEq)]
pub struct MasterOutput {
    pub block: AudioBlock,
    pub meters: MeterReadings,
    pub input_meters: Vec<InputMeterReadings>,
    pub monitor: MonitorOutput,
}

/// Result of one timed Master bus render using the canonical media block.
#[derive(Clone, Debug, PartialEq)]
pub struct TimedMasterOutput {
    pub block: fm_frame::AudioBlock,
    pub meters: MeterReadings,
    pub input_meters: Vec<InputMeterReadings>,
    pub monitor: MonitorOutput,
}

/// Caller-owned meter slices for
/// [`MasterMixer::mix_planar_timed_into_with_monitor`].
pub struct MixerMeterSlices<'a> {
    /// Master, in Master channel-layout order.
    pub master: &'a mut [ChannelMeter],
    /// Flat, ordered first by [`InputId`] then by Master channel order.
    pub inputs: &'a mut [ChannelMeter],
    /// The monitor bus, in Master channel-layout order.
    pub monitor: &'a mut [ChannelMeter],
    /// One loudness meter per input strip in [`InputId`] order, each in the
    /// Master channel layout (AU-005). A strip's meter hears exactly what
    /// its peak/RMS meter does: its post-fader contribution to Program, or
    /// silence while it is not audible.
    pub loudness: Option<&'a mut [LoudnessMeter]>,
}

/// Audio summed into (or replacing) the Program bus after the strips and
/// before the Master DSP chain, clipping, and the Master meters, so it is
/// processed and metered like the strips (a stinger clip's own audio).
#[derive(Clone, Copy, Debug)]
pub struct MasterBusInsert<'a> {
    /// One plane per Master channel, each at least the render's length.
    pub planes: &'a [Vec<f32>],
    /// Replaces the summed strips instead of adding to them.
    pub replace: bool,
}

/// One borrowed planar source for allocation-free Master mixing.
#[derive(Clone, Copy, Debug)]
pub struct PlanarAudioSource<'a> {
    pub input: InputId,
    pub sample_rate: SampleRate,
    pub channel_layout: &'a ChannelLayout,
    pub planes: &'a [Vec<f32>],
    pub samples: usize,
    pub source_gain: SourceGain,
}

trait MixerBlockView {
    fn sample_rate(&self) -> SampleRate;
    fn channel_layout(&self) -> &ChannelLayout;
    fn sample_count(&self) -> usize;
    fn planes(&self) -> &[Vec<f32>];
}

trait MixerSubmission<B> {
    fn input(&self) -> InputId;
    fn block(&self) -> &B;
    fn source_gain(&self) -> SourceGain;
}

impl<B> MixerSubmission<B> for (InputId, &B) {
    fn input(&self) -> InputId {
        self.0
    }

    fn block(&self) -> &B {
        self.1
    }

    fn source_gain(&self) -> SourceGain {
        SourceGain::UNITY
    }
}

impl<B> MixerSubmission<B> for (InputId, &B, SourceGain) {
    fn input(&self) -> InputId {
        self.0
    }

    fn block(&self) -> &B {
        self.1
    }

    fn source_gain(&self) -> SourceGain {
        self.2
    }
}

impl MixerBlockView for AudioBlock {
    fn sample_rate(&self) -> SampleRate {
        self.format.sample_rate
    }

    fn channel_layout(&self) -> &ChannelLayout {
        &self.format.channels
    }

    fn sample_count(&self) -> usize {
        self.samples
    }

    fn planes(&self) -> &[Vec<f32>] {
        &self.planes
    }
}

impl MixerBlockView for fm_frame::AudioBlock {
    fn sample_rate(&self) -> SampleRate {
        self.sample_rate()
    }

    fn channel_layout(&self) -> &ChannelLayout {
        self.channel_layout()
    }

    fn sample_count(&self) -> usize {
        self.sample_count()
    }

    fn planes(&self) -> &[Vec<f32>] {
        self.planes()
    }
}

impl MixerBlockView for PlanarAudioSource<'_> {
    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    fn channel_layout(&self) -> &ChannelLayout {
        self.channel_layout
    }

    fn sample_count(&self) -> usize {
        self.samples
    }

    fn planes(&self) -> &[Vec<f32>] {
        self.planes
    }
}

impl<'a> MixerSubmission<PlanarAudioSource<'a>> for PlanarAudioSource<'a> {
    fn input(&self) -> InputId {
        self.input
    }

    fn block(&self) -> &PlanarAudioSource<'a> {
        self
    }

    fn source_gain(&self) -> SourceGain {
        self.source_gain
    }
}

struct MixedMaster {
    planes: Vec<Vec<f32>>,
    meters: MeterReadings,
    input_meters: Vec<InputMeterReadings>,
    monitor: MonitorOutput,
}

struct RenderMeters {
    master: MeterReadings,
    inputs: Vec<InputMeterReadings>,
}

/// Optional per-render additions: strip loudness meters and a Master bus
/// insert.
#[derive(Default)]
struct RenderExtras<'a> {
    loudness: Option<&'a mut [LoudnessMeter]>,
    insert: Option<MasterBusInsert<'a>>,
}

/// Where one render writes the monitor bus and its meters.
struct MonitorTarget<'a> {
    planes: &'a mut [Vec<f32>],
    meters: Option<&'a mut [ChannelMeter]>,
}

#[derive(Clone, Copy, Default)]
struct ChannelMeterAccumulator {
    sample: f32,
    peak: f32,
    squares: f64,
}

impl ChannelMeterAccumulator {
    fn begin_sample(&mut self) {
        self.sample = 0.0;
    }

    fn add(&mut self, contribution: f32) -> bool {
        self.sample += contribution;
        self.sample.is_finite()
    }

    fn end_sample(&mut self) {
        self.peak = self.peak.max(self.sample.abs());
        self.squares += f64::from(self.sample) * f64::from(self.sample);
    }

    #[allow(clippy::cast_possible_truncation)]
    fn finish(self, samples: usize) -> ChannelMeter {
        if samples == 0 {
            return ChannelMeter::default();
        }
        let sample_count = f64::from(
            u32::try_from(samples).expect("audio sample count is bounded below u32::MAX"),
        );
        ChannelMeter {
            peak: self.peak,
            rms: (self.squares / sample_count).sqrt() as f32,
        }
    }
}

/// Deterministic planar float mixer with one Master bus.
#[derive(Clone, Debug)]
pub struct MasterMixer {
    format: AudioFormat,
    inputs: BTreeMap<InputId, InputStrip>,
    groups: [GroupStrip; MIXER_GROUP_COUNT],
    solo_mode: SoloMode,
    clipping_policy: ClippingPolicy,
    retained_delay_bytes: usize,
    /// The Master bus DSP chain (AU-005), after the strips are summed.
    master_dsp: Option<Box<LiveStripDsp>>,
    /// The Master chain as a render found it, restored when the render is
    /// rejected; present exactly when `master_dsp` is.
    master_checkpoint: Option<Box<LiveStripDsp>>,
    /// Samples each strip DSP scratch holds without growing.
    dsp_block_capacity: usize,
}

impl MasterMixer {
    /// Creates a Master bus.
    ///
    /// # Errors
    ///
    /// Returns an error unless `format` is bounded planar `F32` with a
    /// representable, unique semantic channel layout.
    pub fn new(format: AudioFormat) -> Result<Self, AudioError> {
        validate_format(&format)?;
        channel_mapping::validate_layout(ChannelMappingSide::Destination, &format.channels)?;
        Ok(Self {
            format,
            inputs: BTreeMap::new(),
            groups: [GroupStrip::unity(); MIXER_GROUP_COUNT],
            solo_mode: SoloMode::InPlace,
            clipping_policy: ClippingPolicy::default(),
            retained_delay_bytes: 0,
            master_dsp: None,
            master_checkpoint: None,
            dsp_block_capacity: DEFAULT_DSP_BLOCK_CAPACITY,
        })
    }

    /// Sets how many samples a strip DSP chain processes per render without
    /// allocating (the longest block the caller renders). Existing chains
    /// grow to it now. A longer render through an active chain is refused
    /// ([`AudioError::DspBlockOverCapacity`]): the scratch never grows on
    /// the audio thread.
    pub fn set_dsp_block_capacity(&mut self, samples: usize) {
        self.dsp_block_capacity = samples.clamp(1, MAX_SAMPLES_PER_BLOCK);
        let capacity = self.dsp_block_capacity;
        for strip in self.inputs.values_mut() {
            if let Some(slot) = &mut strip.dsp
                && slot.capacity < capacity
            {
                **slot = StripDspSlot::new(slot.live.clone(), capacity);
            }
        }
    }

    /// The DSP chain of `id`'s strip; `None` for an unknown input or a
    /// strip that never had one (the default chain).
    #[must_use]
    pub fn input_dsp(&self, id: InputId) -> Option<&fm_types::strip_dsp::StripDsp> {
        self.inputs
            .get(&id)?
            .dsp
            .as_ref()
            .map(|slot| slot.live.settings())
    }

    /// Retargets `id`'s strip DSP chain (AU-005); the change glides or
    /// crossfades from the next rendered sample. The first non-default chain
    /// allocates the strip's chain and scratch; later edits do not allocate.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::UnknownInput`] or [`AudioError::Dsp`] for an
    /// out-of-range chain. Nothing changes on error.
    pub fn set_input_dsp(
        &mut self,
        id: InputId,
        settings: &fm_types::strip_dsp::StripDsp,
    ) -> Result<(), AudioError> {
        let rate = self.format.sample_rate.hertz();
        let capacity = self.dsp_block_capacity;
        let strip = self
            .inputs
            .get_mut(&id)
            .ok_or(AudioError::UnknownInput(id))?;
        match &mut strip.dsp {
            Some(slot) => slot.live.set_settings(settings).map_err(AudioError::Dsp),
            None if settings.is_default() => Ok(()),
            None => {
                #[allow(clippy::cast_precision_loss)]
                let live = LiveStripDsp::new(
                    rate as f32,
                    strip.format.channels.channels().len(),
                    settings,
                )
                .map_err(AudioError::Dsp)?;
                strip.dsp = Some(Box::new(StripDspSlot::new(live, capacity)));
                Ok(())
            }
        }
    }

    /// The Master bus DSP chain; `None` until a non-default chain is set.
    #[must_use]
    pub fn master_dsp(&self) -> Option<&fm_types::strip_dsp::StripDsp> {
        self.master_dsp.as_ref().map(|live| live.settings())
    }

    /// Retargets the Master bus DSP chain (AU-005), which processes the
    /// summed Program bus before clipping, the Master meters, and the
    /// monitor bus that follows Master.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Dsp`] for an out-of-range chain; nothing
    /// changes.
    pub fn set_master_dsp(
        &mut self,
        settings: &fm_types::strip_dsp::StripDsp,
    ) -> Result<(), AudioError> {
        match &mut self.master_dsp {
            Some(live) => live.set_settings(settings).map_err(AudioError::Dsp),
            None if settings.is_default() => Ok(()),
            None => {
                #[allow(clippy::cast_precision_loss)]
                let live = LiveStripDsp::new(
                    self.format.sample_rate.hertz() as f32,
                    self.format.channels.channels().len(),
                    settings,
                )
                .map_err(AudioError::Dsp)?;
                self.master_checkpoint = Some(Box::new(live.clone()));
                self.master_dsp = Some(Box::new(live));
                Ok(())
            }
        }
    }

    /// The gain reduction `id`'s strip DSP applied during the last render.
    #[must_use]
    pub fn input_dynamics(&self, id: InputId) -> Option<DspDynamicsReading> {
        self.inputs
            .get(&id)?
            .dsp
            .as_ref()
            .map(|slot| slot.live.dynamics())
    }

    /// The gain reduction the Master DSP applied during the last render.
    #[must_use]
    pub fn master_dynamics(&self) -> Option<DspDynamicsReading> {
        self.master_dsp.as_ref().map(|live| live.dynamics())
    }

    #[must_use]
    pub const fn format(&self) -> &AudioFormat {
        &self.format
    }

    #[must_use]
    pub const fn clipping_policy(&self) -> ClippingPolicy {
        self.clipping_policy
    }

    #[must_use]
    pub const fn retained_delay_bytes(&self) -> usize {
        self.retained_delay_bytes
    }

    pub fn set_clipping_policy(&mut self, policy: ClippingPolicy) {
        self.clipping_policy = policy;
    }

    #[must_use]
    pub const fn solo_mode(&self) -> SoloMode {
        self.solo_mode
    }

    /// Chooses how a solo is heard; takes effect at the next rendered
    /// sample.
    pub fn set_solo_mode(&mut self, mode: SoloMode) {
        self.solo_mode = mode;
    }

    /// Whether `group` itself is soloed (not counting its ancestors).
    #[must_use]
    pub const fn group_soloed(&self, group: MixerGroup) -> bool {
        self.groups[group.index()].soloed
    }

    /// Solos or unsolos `group`, and with it every member strip and every
    /// member of a nested child group; takes effect at the next rendered
    /// sample.
    pub fn set_group_soloed(&mut self, group: MixerGroup, soloed: bool) {
        self.groups[group.index()].soloed = soloed;
    }

    /// The group `group` is nested in, if any.
    #[must_use]
    pub const fn group_parent(&self, group: MixerGroup) -> Option<MixerGroup> {
        self.groups[group.index()].parent
    }

    /// Nests `group` inside `parent`, or makes it top-level.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::GroupCycle`] when `parent` is `group` itself or
    /// already nested (at any depth) inside `group`. Nothing changes on
    /// error.
    pub fn set_group_parent(
        &mut self,
        group: MixerGroup,
        parent: Option<MixerGroup>,
    ) -> Result<(), AudioError> {
        if let Some(parent) = parent
            && self
                .group_chain(Some(parent))
                .any(|ancestor| ancestor == group)
        {
            return Err(AudioError::GroupCycle { group, parent });
        }
        self.groups[group.index()].parent = parent;
        Ok(())
    }

    /// `first` and then each ancestor, bounded by [`MIXER_GROUP_COUNT`].
    fn group_chain(&self, first: Option<MixerGroup>) -> impl Iterator<Item = MixerGroup> + '_ {
        let mut next = first;
        (0..MIXER_GROUP_COUNT).map_while(move |_| {
            let group = next?;
            next = self.groups[group.index()].parent;
            Some(group)
        })
    }

    fn resolve_groups(&self, first: Option<MixerGroup>) -> ResolvedGroups {
        let mut resolved = ResolvedGroups {
            muted: false,
            soloed: false,
            ramps: [None; MIXER_GROUP_COUNT],
        };
        for (slot, group) in self.group_chain(first).enumerate() {
            let strip = &self.groups[group.index()];
            resolved.muted |= strip.state.muted;
            resolved.soloed |= strip.soloed;
            resolved.ramps[slot] = Some(strip.ramp);
        }
        resolved
    }

    /// Whether `id`'s strip is soloed by itself or through any group on its
    /// chain.
    #[must_use]
    pub fn input_effectively_soloed(&self, id: InputId) -> Option<bool> {
        self.inputs
            .get(&id)
            .map(|strip| strip.state.soloed || self.resolve_groups(strip.state.group).soloed)
    }

    /// The effective linear fader gain of `id`'s strip right now: its own
    /// fader times every group fader on its chain, mid-ramp included.
    #[must_use]
    pub fn current_effective_linear_gain(&self, id: InputId) -> Option<f32> {
        self.inputs.get(&id).map(|strip| {
            self.group_chain(strip.state.group)
                .fold(strip.ramp.current, |gain, group| {
                    gain * self.groups[group.index()].ramp.current
                })
        })
    }

    /// Adds one configured input atomically.
    ///
    /// # Errors
    ///
    /// Returns an error for a duplicate ID, format incompatibility, or channel
    /// mapping layout mismatch. The mixer is unchanged on error.
    pub fn add_input(
        &mut self,
        id: InputId,
        format: AudioFormat,
        mapping: ChannelMapping,
        state: InputState,
    ) -> Result<(), AudioError> {
        if self.inputs.contains_key(&id) {
            return Err(AudioError::DuplicateInput(id));
        }
        validate_format(&format)?;
        if format.sample_rate != self.format.sample_rate {
            return Err(AudioError::FormatMismatch);
        }
        if mapping.source_layout() != &format.channels
            || mapping.destination_layout() != &self.format.channels
        {
            return Err(AudioError::MappingLayoutMismatch);
        }
        let channels = format.channels.channels().len();
        let delay = SampleDelay::zero(channels);
        let ramp = GainRamp::immediate(state.gain);
        let balance_ramp = PositionRamp::immediate(state.balance.normalized());
        let pan_ramp = PositionRamp::immediate(state.pan.normalized());
        self.inputs.insert(
            id,
            InputStrip {
                format,
                mapping,
                state,
                ramp,
                balance_ramp,
                pan_ramp,
                delay,
                dsp: None,
            },
        );
        Ok(())
    }

    /// Adds a strip that reuses another configured input's format and mapping.
    ///
    /// This supports independently controlled logical inputs that share one
    /// physical PCM source. The mixer is unchanged on error.
    ///
    /// # Errors
    ///
    /// Returns an error when `source` is unknown or `id` is already configured.
    pub fn add_input_alias(
        &mut self,
        id: InputId,
        source: InputId,
        state: InputState,
    ) -> Result<(), AudioError> {
        let source = self
            .inputs
            .get(&source)
            .ok_or(AudioError::UnknownInput(source))?;
        let format = source.format.clone();
        let mapping = source.mapping.clone();
        self.add_input(id, format, mapping, state)
    }

    /// Removes an input.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::UnknownInput`] when `id` is not configured.
    pub fn remove_input(&mut self, id: InputId) -> Result<(), AudioError> {
        let strip = self.inputs.get(&id).ok_or(AudioError::UnknownInput(id))?;
        let released = strip.delay.retained_bytes();
        let retained = self
            .retained_delay_bytes
            .checked_sub(released)
            .ok_or(AudioError::FormatMismatch)?;
        let Some(_) = self.inputs.remove(&id) else {
            return Err(AudioError::UnknownInput(id));
        };
        self.retained_delay_bytes = retained;
        Ok(())
    }

    #[must_use]
    pub fn input_state(&self, id: InputId) -> Option<InputState> {
        self.inputs.get(&id).map(|strip| strip.state)
    }

    #[must_use]
    pub fn current_linear_gain(&self, id: InputId) -> Option<f32> {
        self.inputs.get(&id).map(|strip| strip.ramp.current)
    }

    #[must_use]
    pub fn current_normalized_balance(&self, id: InputId) -> Option<f32> {
        self.inputs.get(&id).map(|strip| strip.balance_ramp.current)
    }

    #[must_use]
    pub fn current_normalized_pan(&self, id: InputId) -> Option<f32> {
        self.inputs.get(&id).map(|strip| strip.pan_ramp.current)
    }

    #[must_use]
    pub const fn group_state(&self, group: MixerGroup) -> GroupState {
        self.groups[group.index()].state
    }

    /// The group fader's current linear gain, mid-ramp included.
    #[must_use]
    pub const fn current_group_linear_gain(&self, group: MixerGroup) -> f32 {
        self.groups[group.index()].ramp.current
    }

    /// Replaces one group's fader and mute and schedules its linear gain
    /// ramp. A mute change takes effect at the next rendered sample, like a
    /// strip mute.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::RampTooLong`] for an excessive ramp. No state is
    /// changed on error.
    pub fn set_group_state(
        &mut self,
        group: MixerGroup,
        state: GroupState,
        ramp_samples: usize,
    ) -> Result<(), AudioError> {
        if ramp_samples > MAX_RAMP_SAMPLES {
            return Err(AudioError::RampTooLong(ramp_samples));
        }
        let strip = &mut self.groups[group.index()];
        strip.ramp.set(state.gain, ramp_samples);
        strip.state = state;
        Ok(())
    }

    #[must_use]
    pub fn input_delay_samples(&self, id: InputId) -> Option<usize> {
        self.inputs
            .get(&id)
            .map(|strip| strip.delay.delay_samples())
    }

    /// Reconfigures one strip's exact raw-planar delay.
    ///
    /// A changed delay starts with leading silence. Allocation, bounds, and the
    /// per-mixer retained-byte budget are checked before the strip is mutated.
    /// Setting the existing value preserves its history.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown input, an out-of-range delay, allocation
    /// failure, byte-accounting overflow, or mixer delay-budget exhaustion.
    pub fn set_input_delay(
        &mut self,
        id: InputId,
        delay_samples: usize,
    ) -> Result<(), MasterMixerDelayError> {
        let strip = self
            .inputs
            .get(&id)
            .ok_or(MasterMixerDelayError::UnknownInput(id))?;
        if strip.delay.delay_samples() == delay_samples {
            return Ok(());
        }
        let channels = strip.format.channels.channels().len();
        let current = delay_bytes(channels, strip.delay.delay_samples())?;
        let requested = delay_bytes(channels, delay_samples)?;
        let retained = self
            .retained_delay_bytes
            .checked_sub(current)
            .and_then(|retained| retained.checked_add(requested))
            .ok_or(MasterMixerDelayError::ByteCountOverflow)?;
        if retained > MAX_MASTER_MIXER_DELAY_BYTES {
            return Err(MasterMixerDelayError::BudgetExceeded {
                requested,
                retained: self.retained_delay_bytes - current,
                maximum: MAX_MASTER_MIXER_DELAY_BYTES,
            });
        }
        let delay = SampleDelay::new(channels, delay_samples)?;
        let Some(strip) = self.inputs.get_mut(&id) else {
            return Err(MasterMixerDelayError::UnknownInput(id));
        };
        strip.delay = delay;
        self.retained_delay_bytes = retained;
        Ok(())
    }

    /// Replaces input state and schedules linear gain, balance, and pan
    /// ramps.
    ///
    /// Mute, follow-video, and group changes take effect at the next rendered
    /// sample.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown input or excessive ramp. No state is
    /// changed on error.
    pub fn set_input_state(
        &mut self,
        id: InputId,
        state: InputState,
        ramp_samples: usize,
    ) -> Result<(), AudioError> {
        if ramp_samples > MAX_RAMP_SAMPLES {
            return Err(AudioError::RampTooLong(ramp_samples));
        }
        let strip = self
            .inputs
            .get_mut(&id)
            .ok_or(AudioError::UnknownInput(id))?;
        strip.ramp.set(state.gain, ramp_samples);
        strip
            .balance_ramp
            .set(state.balance.normalized(), ramp_samples);
        strip.pan_ramp.set(state.pan.normalized(), ramp_samples);
        strip.state = state;
        Ok(())
    }

    /// Copies mutable strip and clipping state from an identically configured mixer.
    ///
    /// This performs no allocation and is intended for reusable transactional
    /// mixer pairs.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::FormatMismatch`] unless both mixers have identical
    /// formats and configured input strips.
    pub fn copy_runtime_state_from(&mut self, other: &Self) -> Result<(), AudioError> {
        if self.format != other.format || self.inputs.len() != other.inputs.len() {
            return Err(AudioError::FormatMismatch);
        }
        for (id, strip) in &mut self.inputs {
            let source = other.inputs.get(id).ok_or(AudioError::FormatMismatch)?;
            if strip.format != source.format
                || strip.mapping != source.mapping
                || strip.delay.channels() != source.delay.channels()
                || strip.delay.delay_samples() != source.delay.delay_samples()
                || strip.dsp.is_some() != source.dsp.is_some()
                || strip
                    .dsp
                    .as_ref()
                    .zip(source.dsp.as_ref())
                    .is_some_and(|(own, other)| own.live.channels() != other.live.channels())
            {
                return Err(AudioError::FormatMismatch);
            }
        }
        if self.master_dsp.is_some() != other.master_dsp.is_some() {
            return Err(AudioError::FormatMismatch);
        }
        for (id, strip) in &mut self.inputs {
            let Some(source) = other.inputs.get(id) else {
                return Err(AudioError::FormatMismatch);
            };
            strip.state = source.state;
            strip.ramp = source.ramp;
            strip.balance_ramp = source.balance_ramp;
            strip.pan_ramp = source.pan_ramp;
            strip.delay.copy_runtime_state_from(&source.delay);
            if let (Some(own), Some(other)) = (&mut strip.dsp, &source.dsp) {
                own.live
                    .copy_state_from(&other.live)
                    .map_err(|_| AudioError::FormatMismatch)?;
            }
        }
        if let (Some(own), Some(other)) = (&mut self.master_dsp, &other.master_dsp) {
            own.copy_state_from(other)
                .map_err(|_| AudioError::FormatMismatch)?;
        }
        self.groups = other.groups;
        self.solo_mode = other.solo_mode;
        self.retained_delay_bytes = other.retained_delay_bytes;
        self.clipping_policy = other.clipping_policy;
        Ok(())
    }

    /// Clears gain/balance/pan-ramp and delay history while preserving
    /// configured strip and group state, mappings, delay lengths, and
    /// clipping policy.
    pub fn reset_runtime_state(&mut self) {
        for strip in self.inputs.values_mut() {
            strip.ramp = GainRamp::immediate(strip.state.gain);
            strip.balance_ramp = PositionRamp::immediate(strip.state.balance.normalized());
            strip.pan_ramp = PositionRamp::immediate(strip.state.pan.normalized());
            strip.delay.reset();
            if let Some(slot) = &mut strip.dsp {
                slot.live.reset();
            }
        }
        for group in &mut self.groups {
            group.ramp = GainRamp::immediate(group.state.gain);
        }
        if let Some(live) = &mut self.master_dsp {
            live.reset();
        }
    }

    /// Renders submitted blocks into the Master bus.
    ///
    /// Inputs not submitted for this call contribute silence. Every submitted
    /// block is validated before output or ramp state is changed, so all errors
    /// are transactional.
    ///
    /// # Errors
    ///
    /// Returns an error for excessive sample count, duplicate/unknown inputs,
    /// or any block whose exact format or sample count does not match.
    pub fn mix(
        &mut self,
        samples: usize,
        blocks: &[(InputId, &AudioBlock)],
        active_video_input: Option<InputId>,
    ) -> Result<MasterOutput, AudioError> {
        let active_video_inputs = active_video_input.as_slice();
        let mixed = self.mix_block_views(samples, blocks, active_video_inputs)?;
        let block = AudioBlock::from_planar(self.format.clone(), mixed.planes)?;
        Ok(MasterOutput {
            block,
            meters: mixed.meters,
            input_meters: mixed.input_meters,
            monitor: mixed.monitor,
        })
    }

    /// Renders canonical timed blocks into the Master bus.
    ///
    /// Input timing is not compared with `output_timing`; callers select or
    /// slice inputs for the requested interval. The output carries
    /// `output_timing` unchanged. Inputs not submitted contribute silence.
    /// Every submission is validated before output allocation or ramp state is
    /// changed, so validation errors are transactional.
    ///
    /// # Errors
    ///
    /// Returns an error for excessive sample count, duplicate/unknown inputs,
    /// a sample-rate, channel-layout, or sample-count mismatch, non-finite
    /// samples, or failure to construct the canonical output block.
    pub fn mix_timed(
        &mut self,
        output_timing: fm_frame::MediaTiming,
        samples: usize,
        blocks: &[(InputId, &fm_frame::AudioBlock)],
        active_video_input: Option<InputId>,
    ) -> Result<TimedMasterOutput, AudioError> {
        let active_video_inputs = active_video_input.as_slice();
        validate_sample_count(samples)?;
        validate_canonical_output(&self.format, samples)?;
        validate_timed_duration(output_timing, self.format.sample_rate, samples)?;
        let mixed = self.mix_block_views(samples, blocks, active_video_inputs)?;
        self.timed_output(output_timing, mixed)
    }

    /// Renders timed blocks with independent linear source envelopes.
    ///
    /// Source gain is multiplied by the persistent input-strip and channel-map
    /// gains. Every ID in `active_video_inputs` satisfies follow-video for this
    /// interval. All submissions are validated before output or strip-ramp
    /// state changes, so failures are transactional.
    ///
    /// # Errors
    ///
    /// Returns the same validation and construction errors as
    /// [`Self::mix_timed`].
    pub fn mix_timed_with_source_gains(
        &mut self,
        output_timing: fm_frame::MediaTiming,
        samples: usize,
        blocks: &[(InputId, &fm_frame::AudioBlock, SourceGain)],
        active_video_inputs: &[InputId],
    ) -> Result<TimedMasterOutput, AudioError> {
        validate_sample_count(samples)?;
        validate_canonical_output(&self.format, samples)?;
        validate_timed_duration(output_timing, self.format.sample_rate, samples)?;
        let mixed = self.mix_block_views(samples, blocks, active_video_inputs)?;
        self.timed_output(output_timing, mixed)
    }

    /// Mixes borrowed planar sources into caller-owned preallocated planes.
    ///
    /// Only the first `samples` values of each source and output plane are used.
    /// Structural validation completes before output changes. A numeric error
    /// while summing can leave the output prefix partially rendered, but strip
    /// ramps and delay histories remain unchanged.
    ///
    /// # Errors
    ///
    /// Returns the same validation errors as [`Self::mix_timed_with_source_gains`]
    /// plus output plane shape errors.
    pub fn mix_planar_timed_into(
        &mut self,
        output_timing: fm_frame::MediaTiming,
        samples: usize,
        sources: &[PlanarAudioSource<'_>],
        active_video_inputs: &[InputId],
        output: &mut [Vec<f32>],
    ) -> Result<(), AudioError> {
        validate_sample_count(samples)?;
        validate_canonical_output(&self.format, samples)?;
        validate_timed_duration(output_timing, self.format.sample_rate, samples)?;
        self.mix_block_views_into(
            samples,
            sources,
            active_video_inputs,
            output,
            false,
            None,
            None,
            RenderExtras::default(),
        )
        .map(drop)
    }

    /// Mixes borrowed planar sources and writes meters into caller-owned slices.
    ///
    /// `meters.0` uses Master channel-layout order. `meters.1` is flat, ordered
    /// first by [`InputId`] and then by Master channel-layout order. Both
    /// slices must have their exact required length. All structural validation
    /// completes before output or runtime state changes.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::mix_planar_timed_into`] plus
    /// [`AudioError::MeterCountMismatch`] for either meter slice.
    pub fn mix_planar_timed_into_with_meters(
        &mut self,
        output_timing: fm_frame::MediaTiming,
        samples: usize,
        sources: &[PlanarAudioSource<'_>],
        active_video_inputs: &[InputId],
        output: &mut [Vec<f32>],
        meters: (&mut [ChannelMeter], &mut [ChannelMeter]),
    ) -> Result<(), AudioError> {
        validate_sample_count(samples)?;
        validate_canonical_output(&self.format, samples)?;
        validate_timed_duration(output_timing, self.format.sample_rate, samples)?;
        self.mix_block_views_into(
            samples,
            sources,
            active_video_inputs,
            output,
            false,
            Some(meters),
            None,
            RenderExtras::default(),
        )
        .map(drop)
    }

    /// Like [`Self::mix_planar_timed_into_with_meters`], and also renders the
    /// monitor bus into caller-owned `monitor` planes (one per Master
    /// channel, at least `samples` long) with its meters, the strips'
    /// loudness meters when given, and an optional [`MasterBusInsert`].
    ///
    /// Returns whether the monitor bus carries a PFL/AFL solo (`true`) or
    /// follows Master (`false`).
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::mix_planar_timed_into_with_meters`]
    /// plus plane-shape and meter-count errors for the monitor bus, the
    /// loudness meters, and the insert, and non-finite insert samples.
    #[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
    pub fn mix_planar_timed_into_with_monitor(
        &mut self,
        output_timing: fm_frame::MediaTiming,
        samples: usize,
        sources: &[PlanarAudioSource<'_>],
        active_video_inputs: &[InputId],
        output: &mut [Vec<f32>],
        monitor: &mut [Vec<f32>],
        meters: MixerMeterSlices<'_>,
        insert: Option<MasterBusInsert<'_>>,
    ) -> Result<bool, AudioError> {
        validate_sample_count(samples)?;
        validate_canonical_output(&self.format, samples)?;
        validate_timed_duration(output_timing, self.format.sample_rate, samples)?;
        let channels = self.format.channels.channels().len();
        if meters.monitor.len() != channels {
            return Err(AudioError::MeterCountMismatch {
                expected: channels,
                actual: meters.monitor.len(),
            });
        }
        if let Some(loudness) = &meters.loudness
            && (loudness.len() != self.inputs.len()
                || loudness.iter().any(|meter| meter.channels() != channels))
        {
            return Err(AudioError::MeterCountMismatch {
                expected: self.inputs.len(),
                actual: loudness.len(),
            });
        }
        if let Some(insert) = insert {
            validate_mixer_output_planes(samples, channels, insert.planes)?;
            validate_finite_sample_prefix(insert.planes, samples)?;
        }
        let solo = self.monitor_carries_solo();
        self.mix_block_views_into(
            samples,
            sources,
            active_video_inputs,
            output,
            false,
            Some((meters.master, meters.inputs)),
            Some(MonitorTarget {
                planes: monitor,
                meters: Some(meters.monitor),
            }),
            RenderExtras {
                loudness: meters.loudness,
                insert,
            },
        )?;
        Ok(solo)
    }

    /// Whether the next render's monitor bus carries a PFL/AFL solo rather
    /// than Master.
    #[must_use]
    pub fn monitor_carries_solo(&self) -> bool {
        self.solo_mode.listens() && self.any_effectively_soloed()
    }

    fn any_effectively_soloed(&self) -> bool {
        self.inputs
            .values()
            .any(|strip| strip.state.soloed || self.resolve_groups(strip.state.group).soloed)
    }

    fn timed_output(
        &self,
        output_timing: fm_frame::MediaTiming,
        mixed: MixedMaster,
    ) -> Result<TimedMasterOutput, AudioError> {
        let block = fm_frame::AudioBlock::new(
            output_timing,
            self.format.sample_rate,
            self.format.channels.clone(),
            mixed.planes,
        )?;
        Ok(TimedMasterOutput {
            block,
            meters: mixed.meters,
            input_meters: mixed.input_meters,
            monitor: mixed.monitor,
        })
    }

    fn mix_block_views<B: MixerBlockView, S: MixerSubmission<B>>(
        &mut self,
        samples: usize,
        blocks: &[S],
        active_video_inputs: &[InputId],
    ) -> Result<MixedMaster, AudioError> {
        let channels = self.format.channels.channels().len();
        let mut output = vec![vec![0.0; samples]; channels];
        let mut monitor = vec![vec![0.0; samples]; channels];
        let solo = self.monitor_carries_solo();
        let meters = self
            .mix_block_views_into(
                samples,
                blocks,
                active_video_inputs,
                &mut output,
                true,
                None,
                Some(MonitorTarget {
                    planes: &mut monitor,
                    meters: None,
                }),
                RenderExtras::default(),
            )?
            .expect("metering was requested");
        let monitor_meters = measure_planes(&monitor);
        Ok(MixedMaster {
            planes: output,
            meters: meters.master,
            input_meters: meters.inputs,
            monitor: MonitorOutput {
                planes: monitor,
                meters: monitor_meters,
                solo,
            },
        })
    }

    /// Renders like [`Self::render_block_views_into`], transactionally for
    /// the DSP chains too: a render rejected after the strip or Master
    /// chains ran (a non-finite sample found later) restores them, so a
    /// rejected block never advances their filter and dynamics state.
    #[allow(clippy::too_many_arguments)]
    fn mix_block_views_into<B: MixerBlockView, S: MixerSubmission<B>>(
        &mut self,
        samples: usize,
        blocks: &[S],
        active_video_inputs: &[InputId],
        output: &mut [Vec<f32>],
        measure_output: bool,
        meter_output: Option<(&mut [ChannelMeter], &mut [ChannelMeter])>,
        monitor: Option<MonitorTarget<'_>>,
        extras: RenderExtras<'_>,
    ) -> Result<Option<RenderMeters>, AudioError> {
        self.checkpoint_dsp();
        let rendered = self.render_block_views_into(
            samples,
            blocks,
            active_video_inputs,
            output,
            measure_output,
            meter_output,
            monitor,
            extras,
        );
        if rendered.is_err() {
            self.restore_dsp();
        }
        rendered
    }

    /// Saves every DSP chain's state into its preallocated checkpoint.
    fn checkpoint_dsp(&mut self) {
        for strip in self.inputs.values_mut() {
            if let Some(slot) = &mut strip.dsp {
                let StripDspSlot {
                    live, checkpoint, ..
                } = &mut **slot;
                // Same channel count by construction: never fails.
                let _ = checkpoint.copy_state_from(live);
            }
        }
        if let (Some(live), Some(checkpoint)) = (&self.master_dsp, &mut self.master_checkpoint) {
            let _ = checkpoint.copy_state_from(live);
        }
    }

    /// Restores every DSP chain from its checkpoint.
    fn restore_dsp(&mut self) {
        for strip in self.inputs.values_mut() {
            if let Some(slot) = &mut strip.dsp {
                let StripDspSlot {
                    live, checkpoint, ..
                } = &mut **slot;
                let _ = live.copy_state_from(checkpoint);
            }
        }
        if let (Some(live), Some(checkpoint)) = (&mut self.master_dsp, &self.master_checkpoint) {
            let _ = live.copy_state_from(checkpoint);
        }
    }

    #[allow(
        clippy::needless_range_loop,
        clippy::too_many_arguments,
        clippy::too_many_lines
    )]
    fn render_block_views_into<B: MixerBlockView, S: MixerSubmission<B>>(
        &mut self,
        samples: usize,
        blocks: &[S],
        active_video_inputs: &[InputId],
        output: &mut [Vec<f32>],
        measure_output: bool,
        mut meter_output: Option<(&mut [ChannelMeter], &mut [ChannelMeter])>,
        mut monitor: Option<MonitorTarget<'_>>,
        extras: RenderExtras<'_>,
    ) -> Result<Option<RenderMeters>, AudioError> {
        let RenderExtras {
            mut loudness,
            insert,
        } = extras;
        validate_sample_count(samples)?;
        validate_mixer_submissions(samples, blocks, &self.inputs)?;
        self.validate_dsp_capacity(samples)?;
        let channels = self.format.channels.channels().len();
        validate_mixer_output_planes(samples, channels, output)?;
        if let Some(monitor) = &monitor {
            validate_mixer_output_planes(samples, channels, monitor.planes)?;
        }
        if let Some((master_meters, input_meters)) = &meter_output {
            for (expected, actual) in [
                (channels, master_meters.len()),
                (self.inputs.len() * channels, input_meters.len()),
            ] {
                if actual != expected {
                    return Err(AudioError::MeterCountMismatch { expected, actual });
                }
            }
        }
        for plane in output.iter_mut() {
            plane[..samples].fill(0.0);
        }
        let any_soloed = self.any_effectively_soloed();
        let listen = self.solo_mode.listens() && any_soloed;
        let tap = match self.solo_mode {
            SoloMode::Pfl => StripTap::PreFader,
            SoloMode::InPlace | SoloMode::Afl => StripTap::AfterFader,
        };
        if listen && let Some(monitor) = &mut monitor {
            for plane in monitor.planes.iter_mut() {
                plane[..samples].fill(0.0);
            }
        }
        self.process_strip_dsp(samples, blocks);
        let metering = measure_output || meter_output.is_some();
        let mut allocated_input_meters =
            measure_output.then(|| Vec::with_capacity(self.inputs.len()));
        for (input_index, (id, strip)) in self.inputs.iter().enumerate() {
            let block = blocks.iter().find_map(|submission| {
                (submission.input() == *id).then(|| (submission.block(), submission.source_gain()))
            });
            let groups = self.resolve_groups(strip.state.group);
            let soloed = strip.state.soloed || groups.soloed;
            let audible = !strip.state.muted
                && !groups.muted
                && (listen || !any_soloed || soloed)
                && (!strip.state.follow_video || active_video_inputs.contains(id));
            let mut meter_channels =
                metering.then(|| [ChannelMeterAccumulator::default(); MAX_CHANNELS]);
            let ramps = StripRamps {
                gain: strip.ramp,
                balance: strip.balance_ramp,
                pan: strip.pan_ramp,
                groups: groups.ramps,
            };
            let mut strip_loudness = loudness
                .as_deref_mut()
                .and_then(|meters| meters.get_mut(input_index))
                .filter(|_| metering);
            if audible {
                mix_audible_strip_samples(
                    samples,
                    &self.format.channels,
                    block,
                    strip,
                    &mut { ramps },
                    StripTap::Program,
                    meter_channels
                        .as_mut()
                        .map(|meters| (&mut meters[..channels], strip_loudness.as_deref_mut())),
                    output,
                )?;
            } else if let Some(meter) = strip_loudness {
                meter.push_silence(samples);
            }
            if listen
                && soloed
                && let Some(monitor) = &mut monitor
            {
                mix_audible_strip_samples(
                    samples,
                    &self.format.channels,
                    block,
                    strip,
                    &mut { ramps },
                    tap,
                    None,
                    monitor.planes,
                )?;
            }
            if metering {
                let readings = meter_channels.expect("input metering was requested");
                if let Some(input_meters) = &mut allocated_input_meters {
                    let channels = readings[..channels]
                        .iter()
                        .copied()
                        .map(|meter| meter.finish(samples))
                        .collect();
                    input_meters.push(InputMeterReadings {
                        input: *id,
                        meters: MeterReadings { channels },
                    });
                }
                if let Some((_, input_meters)) = &mut meter_output {
                    let start = input_index * channels;
                    for (output, meter) in input_meters[start..start + channels]
                        .iter_mut()
                        .zip(readings)
                    {
                        *output = meter.finish(samples);
                    }
                }
            }
        }

        if let Some(insert) = insert {
            for (target, source) in output.iter_mut().zip(insert.planes) {
                if insert.replace {
                    target[..samples].copy_from_slice(&source[..samples]);
                } else {
                    for (target, source) in target[..samples].iter_mut().zip(&source[..samples]) {
                        *target += *source;
                    }
                }
            }
        }
        if let Some(live) = &mut self.master_dsp
            && !live.is_transparent()
        {
            validate_finite_sample_prefix(output, samples)?;
            live.process(samples, &mut InPlacePlanes(output));
        }
        if self.clipping_policy == ClippingPolicy::Clamp {
            for plane in output.iter_mut() {
                for sample in &mut plane[..samples] {
                    *sample = sample.clamp(-1.0, 1.0);
                }
            }
        }
        validate_finite_sample_prefix(output, samples)?;
        if let Some(monitor) = &mut monitor {
            if listen {
                if self.clipping_policy == ClippingPolicy::Clamp {
                    for plane in monitor.planes.iter_mut() {
                        for sample in &mut plane[..samples] {
                            *sample = sample.clamp(-1.0, 1.0);
                        }
                    }
                }
                validate_finite_sample_prefix(monitor.planes, samples)?;
            } else {
                for (target, source) in monitor.planes.iter_mut().zip(output.iter()) {
                    target[..samples].copy_from_slice(&source[..samples]);
                }
            }
            if let Some(meters) = &mut monitor.meters {
                measure_plane_prefix_into(monitor.planes, samples, meters);
            }
        }
        if let Some((master_meters, _)) = &mut meter_output {
            measure_plane_prefix_into(output, samples, master_meters);
        }
        let meters = allocated_input_meters.map(|inputs| RenderMeters {
            master: measure_plane_prefix(output, samples),
            inputs,
        });
        self.commit_render(samples, blocks);
        Ok(meters)
    }

    /// Refuses a render of `samples` longer than an active strip chain's
    /// scratch holds.
    fn validate_dsp_capacity(&self, samples: usize) -> Result<(), AudioError> {
        for strip in self.inputs.values() {
            if let Some(slot) = &strip.dsp
                && slot.capacity < samples
                && !slot.live.is_transparent()
            {
                return Err(AudioError::DspBlockOverCapacity {
                    samples,
                    capacity: slot.capacity,
                });
            }
        }
        Ok(())
    }

    /// Runs every strip's DSP over its delayed input into the strip's
    /// scratch, once per render, so the Program and monitor taps read the
    /// same processed samples. A transparent chain is skipped and its strip
    /// routes the delayed input directly. Never allocates: a render longer
    /// than a chain's scratch was refused before
    /// ([`Self::validate_dsp_capacity`]).
    fn process_strip_dsp<B: MixerBlockView, S: MixerSubmission<B>>(
        &mut self,
        samples: usize,
        blocks: &[S],
    ) {
        for (id, strip) in &mut self.inputs {
            let Some(slot) = &mut strip.dsp else {
                continue;
            };
            slot.active = !slot.live.is_transparent();
            if !slot.active {
                continue;
            }
            debug_assert!(slot.capacity >= samples, "validated before the render");
            let block = blocks
                .iter()
                .find(|submission| submission.input() == *id)
                .map(MixerSubmission::block);
            let capacity = slot.capacity;
            let mut io = StripDspIo {
                delay: &strip.delay,
                block,
                scratch: &mut slot.scratch,
                capacity,
            };
            slot.live.process(samples, &mut io);
        }
    }

    fn commit_render<B: MixerBlockView, S: MixerSubmission<B>>(
        &mut self,
        samples: usize,
        blocks: &[S],
    ) {
        for (id, strip) in &mut self.inputs {
            let block = blocks
                .iter()
                .find(|submission| submission.input() == *id)
                .map(MixerSubmission::block);
            let channels = strip.format.channels.channels().len();
            for channel in 0..channels {
                for sample in 0..samples {
                    let input = block.map_or(0.0, |block| block.planes()[channel][sample]);
                    strip.delay.commit_sample(channel, sample, input);
                }
            }
            strip.delay.advance(samples);
            for _ in 0..samples {
                strip.ramp.next();
                strip.balance_ramp.next();
                strip.pan_ramp.next();
            }
        }
        for group in &mut self.groups {
            for _ in 0..samples {
                group.ramp.next();
            }
        }
    }
}

fn balance_gain(balance: f32, destination: fm_types::Channel) -> f32 {
    match destination {
        fm_types::Channel::Left => 1.0 - balance.max(0.0),
        fm_types::Channel::Right => 1.0 + balance.min(0.0),
        _ => 1.0,
    }
}

/// Validates every submitted block against its configured strip and `samples`.
///
/// # Errors
///
/// Returns [`AudioError::DuplicateInput`] for a repeated input,
/// [`AudioError::UnknownInput`] for an unconfigured strip,
/// [`AudioError::FormatMismatch`] or [`AudioError::PlaneCountMismatch`] on a
/// layout disagreement, [`AudioError::SampleCountMismatch`] on a length
/// disagreement, and non-finite sample errors from the prefix validation.
fn validate_mixer_submissions<B: MixerBlockView, S: MixerSubmission<B>>(
    samples: usize,
    blocks: &[S],
    inputs: &BTreeMap<InputId, InputStrip>,
) -> Result<(), AudioError> {
    for (index, submission) in blocks.iter().enumerate() {
        let id = submission.input();
        let block = submission.block();
        if blocks[..index]
            .iter()
            .any(|previous| previous.input() == id)
        {
            return Err(AudioError::DuplicateInput(id));
        }
        let strip = inputs.get(&id).ok_or(AudioError::UnknownInput(id))?;
        if block.sample_rate() != strip.format.sample_rate
            || block.channel_layout() != &strip.format.channels
        {
            return Err(AudioError::FormatMismatch);
        }
        if block.sample_count() != samples {
            return Err(AudioError::SampleCountMismatch {
                expected: samples,
                actual: block.sample_count(),
            });
        }
        let expected_planes = strip.format.channels.channels().len();
        if block.planes().len() != expected_planes {
            return Err(AudioError::PlaneCountMismatch {
                expected: expected_planes,
                actual: block.planes().len(),
            });
        }
        validate_finite_sample_prefix(block.planes(), samples)?;
    }
    Ok(())
}

/// Validates that `output` has one plane per Master channel of at least
/// `samples` samples.
///
/// # Errors
///
/// Returns [`AudioError::PlaneCountMismatch`] and
/// [`AudioError::PlaneLengthMismatch`].
fn validate_mixer_output_planes(
    samples: usize,
    channels: usize,
    output: &[Vec<f32>],
) -> Result<(), AudioError> {
    if output.len() != channels {
        return Err(AudioError::PlaneCountMismatch {
            expected: channels,
            actual: output.len(),
        });
    }
    for (plane, values) in output.iter().enumerate() {
        if values.len() < samples {
            return Err(AudioError::PlaneLengthMismatch {
                plane,
                expected: samples,
                actual: values.len(),
            });
        }
    }
    Ok(())
}

/// Mixes one strip's `tap` into `output` while stepping its ramps.
///
/// Steps a copy of the strip's ramps (and its groups' fader ramps) once per
/// sample; the mixer's own ramps advance in `commit_render` whether or not a
/// strip is audible, so later blocks observe identical ramp positions.
///
/// # Errors
///
/// Returns [`AudioError::NonFiniteSample`] when a contribution or metered
/// accumulation is not finite.
#[allow(clippy::needless_range_loop, clippy::too_many_arguments)]
fn mix_audible_strip_samples<B: MixerBlockView>(
    samples: usize,
    master_layout: &ChannelLayout,
    block: Option<(&B, SourceGain)>,
    strip: &InputStrip,
    ramps: &mut StripRamps,
    tap: StripTap,
    mut meter_channels: Option<(&mut [ChannelMeterAccumulator], Option<&mut LoudnessMeter>)>,
    output: &mut [Vec<f32>],
) -> Result<(), AudioError> {
    let source_gain = match tap {
        StripTap::Program => block.map_or(SourceGain::UNITY, |(_, source_gain)| source_gain),
        StripTap::PreFader | StripTap::AfterFader => SourceGain::UNITY,
    };
    let pans = strip.pans();
    let master_channels = master_layout.channels().len();
    for sample in 0..samples {
        if let Some((meter_channels, _)) = &mut meter_channels {
            for meter in &mut meter_channels[..master_channels] {
                meter.begin_sample();
            }
        }
        let (strip_gain, balance, pan) = match (tap, ramps.next()) {
            (StripTap::PreFader, _) => (1.0, 0.0, 0.0),
            (StripTap::Program | StripTap::AfterFader, values) => values,
        };
        // Centered (and every non-mono strip) is exactly unity.
        let pan = if pans && pan != 0.0 {
            pan_law(pan)
        } else {
            (1.0, 1.0)
        };
        let source_gain = source_gain.at_sample(sample, samples);
        for (source, destination, coefficient) in strip.mapping.compiled_routes() {
            let input = block.map(|(block, _)| block.planes()[source].as_slice());
            let delayed = strip
                .dsp
                .as_ref()
                .and_then(|slot| slot.sample(source, sample))
                .unwrap_or_else(|| strip.delay.preview_sample(source, sample, input));
            let label = master_layout.channels()[destination];
            let balance_gain = balance_gain(balance, label) * pan_gain(pan, label);
            let contribution = delayed * strip_gain * balance_gain * source_gain * coefficient;
            let mapped = output[destination][sample] + contribution;
            if !mapped.is_finite() {
                return Err(AudioError::NonFiniteSample {
                    channel: destination,
                    sample,
                });
            }
            output[destination][sample] = mapped;
            if let Some((meter_channels, _)) = &mut meter_channels
                && !meter_channels[destination].add(contribution)
            {
                return Err(AudioError::NonFiniteSample {
                    channel: destination,
                    sample,
                });
            }
        }
        if let Some((meter_channels, loudness)) = &mut meter_channels {
            let mut frame = [0.0_f32; MAX_CHANNELS];
            for (meter, value) in meter_channels[..master_channels].iter_mut().zip(&mut frame) {
                meter.end_sample();
                *value = meter.sample;
            }
            if let Some(loudness) = loudness {
                loudness.push_frame(&frame[..master_channels]);
            }
        }
    }
    Ok(())
}

/// Measures sample peak and unweighted RMS independently for each channel.
#[must_use]
pub fn measure(block: &AudioBlock) -> MeterReadings {
    measure_planes(&block.planes)
}

#[allow(clippy::cast_possible_truncation)]
fn measure_planes(planes: &[Vec<f32>]) -> MeterReadings {
    let channels = planes
        .iter()
        .map(|plane| {
            if plane.is_empty() {
                return ChannelMeter::default();
            }
            let mut peak = 0.0_f32;
            let mut squares = 0.0_f64;
            for sample in plane {
                peak = peak.max(sample.abs());
                squares += f64::from(*sample) * f64::from(*sample);
            }
            let sample_count = f64::from(
                u32::try_from(plane.len()).expect("audio plane length is bounded below u32::MAX"),
            );
            ChannelMeter {
                peak,
                rms: (squares / sample_count).sqrt() as f32,
            }
        })
        .collect();
    MeterReadings { channels }
}

/// Allocates integer audio samples against rational video frames without drift.
///
/// The allocation after `n` frames is exactly
/// `floor(n * sample_rate * frame_rate.denominator / frame_rate.numerator)`.
#[derive(Clone, Debug)]
pub struct FrameSampleAllocator {
    samples_per_frame_numerator: u128,
    frame_rate_numerator: u128,
    remainder: u128,
    allocated_samples: u128,
    allocated_frames: u64,
}

impl FrameSampleAllocator {
    /// Creates a cumulative rational sample allocator.
    ///
    /// # Errors
    ///
    /// Returns an error if any individual frame could exceed the block bound.
    pub fn new(sample_rate: SampleRate, frame_rate: FrameRate) -> Result<Self, AudioError> {
        let samples_per_frame_numerator =
            u128::from(sample_rate.hertz()) * u128::from(frame_rate.denominator());
        let frame_rate_numerator = u128::from(frame_rate.numerator());
        let maximum = samples_per_frame_numerator.div_ceil(frame_rate_numerator);
        if maximum > MAX_SAMPLES_PER_BLOCK as u128 {
            return Err(AudioError::CadenceBlockTooLarge(maximum));
        }
        Ok(Self {
            samples_per_frame_numerator,
            frame_rate_numerator,
            remainder: 0,
            allocated_samples: 0,
            allocated_frames: 0,
        })
    }

    /// Returns the exact sample allocation for the next video frame.
    pub fn next_samples(&mut self) -> usize {
        self.remainder += self.samples_per_frame_numerator;
        let samples = self.remainder / self.frame_rate_numerator;
        self.remainder %= self.frame_rate_numerator;
        self.allocated_samples += samples;
        self.allocated_frames = self.allocated_frames.saturating_add(1);
        samples as usize
    }

    #[must_use]
    pub const fn allocated_samples(&self) -> u128 {
        self.allocated_samples
    }

    #[must_use]
    pub const fn allocated_frames(&self) -> u64 {
        self.allocated_frames
    }

    pub fn reset(&mut self) {
        self.remainder = 0;
        self.allocated_samples = 0;
        self.allocated_frames = 0;
    }
}

fn validate_format(format: &AudioFormat) -> Result<(), AudioError> {
    if format.sample_format != SampleFormat::F32 {
        return Err(AudioError::UnsupportedSampleFormat(format.sample_format));
    }
    validate_channel_count(format.channels.channels().len())
}

fn validate_channel_count(channels: usize) -> Result<(), AudioError> {
    if !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(AudioError::ChannelCountOutOfRange(channels));
    }
    Ok(())
}

fn validate_sample_count(samples: usize) -> Result<(), AudioError> {
    if samples > MAX_SAMPLES_PER_BLOCK {
        return Err(AudioError::SampleCountOutOfRange(samples));
    }
    Ok(())
}

fn delay_bytes(channels: usize, delay_samples: usize) -> Result<usize, MasterMixerDelayError> {
    if delay_samples > MAX_SAMPLE_DELAY_SAMPLES {
        return Err(SampleDelayError::DelayOutOfRange {
            actual: delay_samples,
            maximum: MAX_SAMPLE_DELAY_SAMPLES,
        }
        .into());
    }
    channels
        .checked_mul(delay_samples)
        .and_then(|samples| samples.checked_mul(size_of::<f32>()))
        .ok_or(MasterMixerDelayError::ByteCountOverflow)
}

fn validate_finite_sample_prefix(planes: &[Vec<f32>], samples: usize) -> Result<(), AudioError> {
    for (channel, plane) in planes.iter().enumerate() {
        if plane.len() < samples {
            return Err(AudioError::PlaneLengthMismatch {
                plane: channel,
                expected: samples,
                actual: plane.len(),
            });
        }
        if let Some(sample) = plane[..samples].iter().position(|value| !value.is_finite()) {
            return Err(AudioError::NonFiniteSample { channel, sample });
        }
    }
    Ok(())
}

#[allow(clippy::cast_possible_truncation)]
fn measure_plane_prefix(planes: &[Vec<f32>], samples: usize) -> MeterReadings {
    let mut channels = vec![ChannelMeter::default(); planes.len()];
    measure_plane_prefix_into(planes, samples, &mut channels);
    MeterReadings { channels }
}

#[allow(clippy::cast_possible_truncation)]
fn measure_plane_prefix_into(planes: &[Vec<f32>], samples: usize, meters: &mut [ChannelMeter]) {
    for (meter, plane) in meters.iter_mut().zip(planes) {
        let mut accumulator = ChannelMeterAccumulator::default();
        for sample in &plane[..samples] {
            accumulator.begin_sample();
            accumulator.add(*sample);
            accumulator.end_sample();
        }
        *meter = accumulator.finish(samples);
    }
}

fn validate_canonical_output(format: &AudioFormat, samples: usize) -> Result<(), AudioError> {
    if format.sample_rate.hertz() > fm_frame::AudioBlock::MAX_SAMPLE_RATE_HZ {
        return Err(fm_frame::AudioBlockError::SampleRateTooHigh {
            actual: format.sample_rate.hertz(),
            maximum: fm_frame::AudioBlock::MAX_SAMPLE_RATE_HZ,
        }
        .into());
    }
    let channels = format.channels.channels();
    if channels
        .iter()
        .enumerate()
        .any(|(index, channel)| channels[index + 1..].contains(channel))
    {
        return Err(fm_frame::AudioBlockError::DuplicateChannel.into());
    }
    if samples == 0 {
        return Err(fm_frame::AudioBlockError::ZeroSamples.into());
    }
    Ok(())
}

fn validate_timed_duration(
    timing: fm_frame::MediaTiming,
    sample_rate: SampleRate,
    samples: usize,
) -> Result<(), AudioError> {
    let numerator = (samples as u128) * 1_000_000_000_u128;
    let denominator = u128::from(sample_rate.hertz());
    let floor = numerator / denominator;
    let ceil = numerator.div_ceil(denominator);
    let duration = u128::from(timing.duration().as_nanos());
    if duration == floor || duration == ceil {
        return Ok(());
    }
    Err(AudioError::TimingSampleCountMismatch {
        samples,
        sample_rate_hz: sample_rate.hertz(),
        duration_nanos: timing.duration().as_nanos(),
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use fm_frame::{
        ClockDomainId, MediaFlags, MediaTiming, NormalizedDuration, NormalizedTimestamp,
        OriginalTimestamp, SequenceNumber,
    };
    use fm_types::{Channel, ChannelLayout};
    use fm_types::{MediaTimestamp, TimeBase};

    use super::*;

    fn stereo_format() -> AudioFormat {
        AudioFormat {
            sample_rate: SampleRate::new(48_000).unwrap(),
            sample_format: SampleFormat::F32,
            channels: ChannelLayout::stereo(),
        }
    }

    fn mono_format() -> AudioFormat {
        AudioFormat {
            sample_rate: SampleRate::new(48_000).unwrap(),
            sample_format: SampleFormat::F32,
            channels: ChannelLayout::new(vec![Channel::Mono]).unwrap(),
        }
    }

    fn input_id(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn timing(sequence: u64) -> MediaTiming {
        MediaTiming::new(
            OriginalTimestamp::new(
                MediaTimestamp::new(i64::try_from(sequence).unwrap()),
                TimeBase::new(1, 48_000).unwrap(),
            ),
            NormalizedTimestamp::from_nanos(i64::try_from(sequence).unwrap() * 1_000_000),
            NormalizedDuration::from_nanos(1_000_000).unwrap(),
            ClockDomainId::new(NonZeroU128::new(7).unwrap()),
            SequenceNumber::new(sequence),
        )
        .unwrap()
    }

    fn timing_for_samples(sequence: u64, samples: usize) -> MediaTiming {
        let samples = u64::try_from(samples).unwrap();
        let start_sample = sequence.checked_mul(samples).unwrap();
        let end_sample = start_sample.checked_add(samples).unwrap();
        let start_nanos = start_sample.checked_mul(1_000_000_000).unwrap() / 48_000;
        let end_nanos = end_sample.checked_mul(1_000_000_000).unwrap() / 48_000;
        MediaTiming::new(
            OriginalTimestamp::new(
                MediaTimestamp::new(i64::try_from(start_sample).unwrap()),
                TimeBase::new(1, 48_000).unwrap(),
            ),
            NormalizedTimestamp::from_nanos(i64::try_from(start_nanos).unwrap()),
            NormalizedDuration::from_nanos(end_nanos - start_nanos).unwrap(),
            ClockDomainId::new(NonZeroU128::new(7).unwrap()),
            SequenceNumber::new(sequence),
        )
        .unwrap()
    }

    fn canonical_block(
        timing: MediaTiming,
        format: &AudioFormat,
        planes: Vec<Vec<f32>>,
    ) -> fm_frame::AudioBlock {
        fm_frame::AudioBlock::new(timing, format.sample_rate, format.channels.clone(), planes)
            .unwrap()
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 1.0e-6, "{actual} != {expected}");
    }

    #[test]
    fn gain_conversion_is_validated() {
        let gain = Gain::from_db(-6.0).unwrap();
        assert_close(gain.linear(), 0.501_187_2);
        assert_close(gain.db(), -6.0);
        let silence_db = Gain::from_linear(0.0).unwrap().db();
        assert!(silence_db.is_infinite() && silence_db.is_sign_negative());
        assert!(Gain::from_db(f32::NAN).is_err());
        assert!(Gain::from_db(25.0).is_err());
        assert!(Gain::from_linear(-1.0).is_err());
    }

    #[test]
    fn silence_sine_and_impulse_are_deterministic() {
        let format = stereo_format();
        let mut silence = SilenceGenerator::new(format.clone()).unwrap();
        let silent = silence.generate(8).unwrap();
        assert!(
            silent
                .planes()
                .iter()
                .flatten()
                .all(|sample| *sample == 0.0)
        );

        let mut whole = SineGenerator::new(format.clone(), 1_000.0, Gain::UNITY).unwrap();
        let expected = whole.generate(16).unwrap();
        let mut split = SineGenerator::new(format.clone(), 1_000.0, Gain::UNITY).unwrap();
        let first = split.generate(7).unwrap();
        let second = split.generate(9).unwrap();
        let combined: Vec<_> = first
            .plane(0)
            .unwrap()
            .iter()
            .chain(second.plane(0).unwrap())
            .copied()
            .collect();
        assert_eq!(combined, expected.plane(0).unwrap());
        assert_eq!(expected.plane(0), expected.plane(1));

        let mut impulse = ImpulseGenerator::new(format, 1, 5, -0.75).unwrap();
        let first = impulse.generate(3).unwrap();
        let second = impulse.generate(5).unwrap();
        assert!(first.planes().iter().flatten().all(|sample| *sample == 0.0));
        assert_eq!(second.plane(1).unwrap(), &[0.0, 0.0, -0.75, 0.0, 0.0]);
        assert!(second.plane(0).unwrap().iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn cadence_accumulates_exactly_for_1001_frames() {
        let mut allocator = FrameSampleAllocator::new(
            SampleRate::new(48_000).unwrap(),
            FrameRate::new(60_000, 1_001).unwrap(),
        )
        .unwrap();
        let allocations: Vec<_> = (0..1_001).map(|_| allocator.next_samples()).collect();
        assert!(
            allocations
                .iter()
                .all(|samples| matches!(samples, 800 | 801))
        );
        assert_eq!(allocations.iter().sum::<usize>(), 801_600);
        assert_eq!(allocator.allocated_samples(), 801_600);
        assert_eq!(allocator.allocated_frames(), 1_001);
    }

    #[test]
    fn timed_mix_preserves_caller_timing_and_accepts_ffmpeg_shaped_stereo() {
        let format = stereo_format();
        let id = input_id(1);
        let input_timing = timing(1);
        let output_timing = timing_for_samples(42, 2).with_flags(MediaFlags::DISCONTINUITY);
        let input = canonical_block(
            input_timing,
            &format,
            vec![vec![0.25, -0.5], vec![0.75, -1.0]],
        );
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer
            .add_input(
                id,
                format.clone(),
                ChannelMapping::identity(format.channels.clone()).unwrap(),
                InputState::default(),
            )
            .unwrap();

        let output = mixer
            .mix_timed(output_timing, 2, &[(id, &input)], None)
            .unwrap();

        assert_ne!(input.timing(), output_timing);
        assert_eq!(output.block.timing(), output_timing);
        assert_eq!(output.block.sample_rate(), format.sample_rate);
        assert_eq!(output.block.channel_layout(), &format.channels);
        assert_eq!(output.block.sample_count(), 2);
        assert_eq!(output.block.planes(), input.planes());
    }

    #[test]
    fn timed_mix_without_submitted_blocks_is_silence() {
        let format = stereo_format();
        let output_timing = timing_for_samples(9, 4);
        let mut mixer = MasterMixer::new(format.clone()).unwrap();

        let output = mixer.mix_timed(output_timing, 4, &[], None).unwrap();

        assert_eq!(output.block.timing(), output_timing);
        assert_eq!(output.block.sample_rate(), format.sample_rate);
        assert_eq!(output.block.channel_layout(), &format.channels);
        assert!(
            output
                .block
                .planes()
                .iter()
                .flatten()
                .all(|sample| *sample == 0.0)
        );
        assert_eq!(
            output.meters.channels(),
            &[ChannelMeter::default(), ChannelMeter::default()]
        );
    }

    #[test]
    fn planar_mix_reuses_caller_storage_and_copies_transactional_state() {
        let format = mono_format();
        let source = input_id(1);
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer
            .add_input(
                source,
                format.clone(),
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState::default(),
            )
            .unwrap();
        mixer
            .set_input_state(
                source,
                InputState {
                    gain: Gain::SILENCE,
                    ..InputState::default()
                },
                4,
            )
            .unwrap();
        let mut pending = MasterMixer::new(format.clone()).unwrap();
        pending
            .add_input(
                source,
                format.clone(),
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState::default(),
            )
            .unwrap();
        pending.copy_runtime_state_from(&mixer).unwrap();
        let planes = vec![vec![1.0, 1.0, f32::NAN, f32::NAN]];
        let mut output = vec![vec![9.0; 8]];
        let pointer = output[0].as_ptr();

        pending
            .mix_planar_timed_into(
                timing_for_samples(0, 2),
                2,
                &[PlanarAudioSource {
                    input: source,
                    sample_rate: format.sample_rate,
                    channel_layout: &format.channels,
                    planes: &planes,
                    samples: 2,
                    source_gain: SourceGain::UNITY,
                }],
                &[source],
                &mut output,
            )
            .unwrap();

        assert_eq!(output[0].as_ptr(), pointer);
        assert_close(output[0][0], 0.75);
        assert_close(output[0][1], 0.5);
        assert_close(output[0][2], 9.0);
        assert_eq!(mixer.current_linear_gain(source), Some(1.0));
        assert_eq!(pending.current_linear_gain(source), Some(0.5));
    }

    #[test]
    fn planar_mix_rejects_source_plane_count_transactionally_for_all_mappings() {
        let input_format = stereo_format();
        let output_format = mono_format();
        let source = input_id(1);
        let valid_planes = vec![vec![1.0; 2], vec![1.0; 2]];
        let route = ChannelMappingRoute {
            source: Channel::Right,
            destination: Channel::Mono,
            coefficient: 1.0,
        };

        for (routes, expected_prefix) in [(vec![], [0.0, 0.0]), (vec![route], [0.75, 0.5])] {
            for actual in [1, 3] {
                let mapping = ChannelMapping::new(
                    input_format.channels.clone(),
                    output_format.channels.clone(),
                    routes.clone(),
                )
                .unwrap();
                let mut mixer = MasterMixer::new(output_format.clone()).unwrap();
                mixer
                    .add_input(source, input_format.clone(), mapping, InputState::default())
                    .unwrap();
                mixer
                    .set_input_state(
                        source,
                        InputState {
                            gain: Gain::SILENCE,
                            ..InputState::default()
                        },
                        4,
                    )
                    .unwrap();
                let malformed_planes = vec![vec![1.0; 2]; actual];
                let mut output = vec![vec![9.0; 4]];

                assert_eq!(
                    mixer.mix_planar_timed_into(
                        timing_for_samples(0, 2),
                        2,
                        &[PlanarAudioSource {
                            input: source,
                            sample_rate: input_format.sample_rate,
                            channel_layout: &input_format.channels,
                            planes: &malformed_planes,
                            samples: 2,
                            source_gain: SourceGain::UNITY,
                        }],
                        &[],
                        &mut output,
                    ),
                    Err(AudioError::PlaneCountMismatch {
                        expected: 2,
                        actual,
                    })
                );
                assert_eq!(output, vec![vec![9.0; 4]]);
                assert_eq!(mixer.current_linear_gain(source), Some(1.0));

                mixer
                    .mix_planar_timed_into(
                        timing_for_samples(1, 2),
                        2,
                        &[PlanarAudioSource {
                            input: source,
                            sample_rate: input_format.sample_rate,
                            channel_layout: &input_format.channels,
                            planes: &valid_planes,
                            samples: 2,
                            source_gain: SourceGain::UNITY,
                        }],
                        &[],
                        &mut output,
                    )
                    .unwrap();

                assert_eq!(&output[0][..2], &expected_prefix);
                assert_eq!(&output[0][2..], &[9.0, 9.0]);
                assert_eq!(mixer.current_linear_gain(source), Some(0.5));
            }
        }
    }

    #[test]
    fn timed_source_gains_hit_endpoints_and_continue_linearly_across_intervals() {
        let format = mono_format();
        let primary = input_id(1);
        let secondary = input_id(2);
        let primary_block = canonical_block(timing(1), &format, vec![vec![1.0; 4]]);
        let secondary_block = canonical_block(timing(2), &format, vec![vec![-1.0; 4]]);
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer.set_clipping_policy(ClippingPolicy::Allow);
        for id in [primary, secondary] {
            mixer
                .add_input(
                    id,
                    format.clone(),
                    ChannelMapping::identity(mono_format().channels).unwrap(),
                    InputState {
                        follow_video: true,
                        ..InputState::default()
                    },
                )
                .unwrap();
        }

        let first = mixer
            .mix_timed_with_source_gains(
                timing_for_samples(10, 4),
                4,
                &[
                    (primary, &primary_block, SourceGain::new(2, 1, 2).unwrap()),
                    (
                        secondary,
                        &secondary_block,
                        SourceGain::new(0, 1, 2).unwrap(),
                    ),
                ],
                &[primary, secondary],
            )
            .unwrap();
        let second = mixer
            .mix_timed_with_source_gains(
                timing_for_samples(11, 4),
                4,
                &[
                    (primary, &primary_block, SourceGain::new(1, 0, 2).unwrap()),
                    (
                        secondary,
                        &secondary_block,
                        SourceGain::new(1, 2, 2).unwrap(),
                    ),
                ],
                &[primary, secondary],
            )
            .unwrap();

        assert_eq!(first.block.plane(0).unwrap(), &[0.75, 0.5, 0.25, 0.0]);
        assert_eq!(second.block.plane(0).unwrap(), &[-0.25, -0.5, -0.75, -1.0]);
        let joined = first
            .block
            .plane(0)
            .unwrap()
            .iter()
            .chain(second.block.plane(0).unwrap());
        for (left, right) in joined.clone().zip(joined.skip(1)) {
            assert_close(*right - *left, -0.25);
        }
    }

    #[test]
    fn timed_source_gains_preserve_mute_and_follow_video() {
        let format = mono_format();
        let primary = input_id(1);
        let secondary = input_id(2);
        let muted = input_id(3);
        let inactive = input_id(4);
        let block = canonical_block(timing(1), &format, vec![vec![1.0; 2]]);
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer.set_clipping_policy(ClippingPolicy::Allow);
        for (id, is_muted) in [
            (primary, false),
            (secondary, false),
            (muted, true),
            (inactive, false),
        ] {
            mixer
                .add_input(
                    id,
                    format.clone(),
                    ChannelMapping::identity(mono_format().channels).unwrap(),
                    InputState {
                        muted: is_muted,
                        soloed: false,
                        follow_video: true,
                        ..InputState::default()
                    },
                )
                .unwrap();
        }

        let output = mixer
            .mix_timed_with_source_gains(
                timing_for_samples(3, 2),
                2,
                &[
                    (primary, &block, SourceGain::UNITY),
                    (secondary, &block, SourceGain::UNITY),
                    (muted, &block, SourceGain::UNITY),
                    (inactive, &block, SourceGain::UNITY),
                ],
                &[primary, secondary, muted],
            )
            .unwrap();

        assert_eq!(output.block.plane(0).unwrap(), &[2.0, 2.0]);
    }

    #[test]
    fn timed_source_gain_failures_are_transactional() {
        let format = mono_format();
        let id = input_id(1);
        assert_eq!(
            SourceGain::new(0, 1, 0),
            Err(AudioError::InvalidSourceGain {
                start_numerator: 0,
                end_numerator: 1,
                denominator: 0,
            })
        );
        assert!(SourceGain::new(0, 2, 1).is_err());

        let valid = canonical_block(timing(1), &format, vec![vec![1.0; 2]]);
        let wrong_count = canonical_block(timing(2), &format, vec![vec![1.0]]);
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer
            .add_input(
                id,
                format,
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState::default(),
            )
            .unwrap();
        mixer
            .set_input_state(
                id,
                InputState {
                    gain: Gain::SILENCE,
                    ..InputState::default()
                },
                4,
            )
            .unwrap();

        assert_eq!(
            mixer.mix_timed_with_source_gains(
                timing_for_samples(4, 2),
                2,
                &[(id, &wrong_count, SourceGain::new(0, 1, 1).unwrap())],
                &[id],
            ),
            Err(AudioError::SampleCountMismatch {
                expected: 2,
                actual: 1,
            })
        );
        assert_eq!(mixer.current_linear_gain(id), Some(1.0));
        let rendered = mixer
            .mix_timed_with_source_gains(
                timing_for_samples(5, 2),
                2,
                &[(id, &valid, SourceGain::new(0, 1, 1).unwrap())],
                &[id],
            )
            .unwrap();
        assert_eq!(rendered.block.plane(0).unwrap(), &[0.375, 0.5]);
    }

    #[test]
    fn timed_validation_errors_leave_ramps_unchanged() {
        let format = mono_format();
        let id = input_id(1);
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer.set_clipping_policy(ClippingPolicy::Allow);
        mixer
            .add_input(
                id,
                format.clone(),
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState::default(),
            )
            .unwrap();
        mixer
            .set_input_state(
                id,
                InputState {
                    gain: Gain::SILENCE,
                    ..InputState::default()
                },
                4,
            )
            .unwrap();

        let output_timing = timing_for_samples(10, 2);
        let valid = canonical_block(timing(1), &format, vec![vec![1.0; 2]]);
        let wrong_rate = fm_frame::AudioBlock::new(
            timing(2),
            SampleRate::new(44_100).unwrap(),
            format.channels.clone(),
            vec![vec![1.0; 2]],
        )
        .unwrap();
        let wrong_layout = fm_frame::AudioBlock::new(
            timing(3),
            format.sample_rate,
            ChannelLayout::new(vec![Channel::Center]).unwrap(),
            vec![vec![1.0; 2]],
        )
        .unwrap();
        let wrong_count = canonical_block(timing(4), &format, vec![vec![1.0]]);
        let non_finite = canonical_block(timing(5), &format, vec![vec![1.0, f32::NAN]]);

        assert_eq!(
            mixer.mix_timed(output_timing, 2, &[(id, &wrong_rate)], None),
            Err(AudioError::FormatMismatch)
        );
        assert_eq!(mixer.current_linear_gain(id), Some(1.0));
        assert_eq!(
            mixer.mix_timed(output_timing, 2, &[(id, &wrong_layout)], None),
            Err(AudioError::FormatMismatch)
        );
        assert_eq!(mixer.current_linear_gain(id), Some(1.0));
        assert_eq!(
            mixer.mix_timed(output_timing, 2, &[(id, &wrong_count)], None),
            Err(AudioError::SampleCountMismatch {
                expected: 2,
                actual: 1,
            })
        );
        assert_eq!(mixer.current_linear_gain(id), Some(1.0));
        assert_eq!(
            mixer.mix_timed(output_timing, 2, &[(id, &non_finite)], None),
            Err(AudioError::NonFiniteSample {
                channel: 0,
                sample: 1,
            })
        );
        assert_eq!(mixer.current_linear_gain(id), Some(1.0));
        assert_eq!(
            mixer.mix_timed(output_timing, 2, &[(id, &valid), (id, &valid)], None),
            Err(AudioError::DuplicateInput(id))
        );
        assert_eq!(mixer.current_linear_gain(id), Some(1.0));
        let unknown = input_id(99);
        assert_eq!(
            mixer.mix_timed(output_timing, 2, &[(unknown, &valid)], None),
            Err(AudioError::UnknownInput(unknown))
        );
        assert_eq!(mixer.current_linear_gain(id), Some(1.0));
        assert_eq!(
            mixer.mix_timed(output_timing, MAX_SAMPLES_PER_BLOCK + 1, &[], None),
            Err(AudioError::SampleCountOutOfRange(MAX_SAMPLES_PER_BLOCK + 1))
        );
        assert_eq!(mixer.current_linear_gain(id), Some(1.0));

        let rendered = mixer
            .mix_timed(output_timing, 2, &[(id, &valid)], None)
            .unwrap();
        assert_eq!(rendered.block.plane(0).unwrap(), &[0.75, 0.5]);
    }

    #[test]
    fn timed_mix_rejects_duration_unrelated_to_sample_count() {
        let mut mixer = MasterMixer::new(mono_format()).unwrap();

        assert_eq!(
            mixer.mix_timed(timing(1), 2, &[], None),
            Err(AudioError::TimingSampleCountMismatch {
                samples: 2,
                sample_rate_hz: 48_000,
                duration_nanos: 1_000_000,
            })
        );
    }

    #[test]
    fn reset_runtime_state_clears_delay_and_finishes_configured_gain() {
        let format = mono_format();
        let id = input_id(1);
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer
            .add_input(
                id,
                format.clone(),
                ChannelMapping::identity(format.channels.clone()).unwrap(),
                InputState::default(),
            )
            .unwrap();
        mixer.set_input_delay(id, 2).unwrap();
        let impulse = canonical_block(timing_for_samples(0, 2), &format, vec![vec![1.0, 0.0]]);
        let first = mixer
            .mix_timed(timing_for_samples(0, 2), 2, &[(id, &impulse)], Some(id))
            .unwrap();
        assert_eq!(first.block.plane(0).unwrap(), &[0.0, 0.0]);
        mixer
            .set_input_state(
                id,
                InputState {
                    gain: Gain::SILENCE,
                    ..InputState::default()
                },
                2,
            )
            .unwrap();

        mixer.reset_runtime_state();

        assert_eq!(mixer.current_linear_gain(id), Some(0.0));
        assert_eq!(mixer.input_delay_samples(id), Some(2));
        let silence = canonical_block(timing_for_samples(1, 2), &format, vec![vec![0.0, 0.0]]);
        let replay = mixer
            .mix_timed(timing_for_samples(1, 2), 2, &[(id, &silence)], Some(id))
            .unwrap();
        assert_eq!(replay.block.plane(0).unwrap(), &[0.0, 0.0]);
    }

    #[test]
    fn numeric_render_errors_leave_ramps_unchanged() {
        let format = mono_format();
        let id = input_id(1);
        let maximum_gain = Gain::from_db(MAX_GAIN_DB).unwrap();
        let input = canonical_block(timing(1), &format, vec![vec![f32::MAX]]);
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer.set_clipping_policy(ClippingPolicy::Allow);
        mixer
            .add_input(
                id,
                format,
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState {
                    gain: maximum_gain,
                    ..InputState::default()
                },
            )
            .unwrap();
        mixer
            .set_input_state(
                id,
                InputState {
                    gain: Gain::SILENCE,
                    ..InputState::default()
                },
                4,
            )
            .unwrap();

        assert_eq!(
            mixer.mix_timed(timing_for_samples(1, 1), 1, &[(id, &input)], None),
            Err(AudioError::NonFiniteSample {
                channel: 0,
                sample: 0,
            })
        );
        assert_eq!(mixer.current_linear_gain(id), Some(maximum_gain.linear()));
    }

    #[test]
    fn canonical_audio_block_errors_are_typed_sources() {
        let mut mixer = MasterMixer::new(mono_format()).unwrap();
        let error = mixer.mix_timed(timing(1), 0, &[], None).unwrap_err();

        assert_eq!(
            error,
            AudioError::AudioBlock(fm_frame::AudioBlockError::ZeroSamples)
        );
        assert!(std::error::Error::source(&error).is_some());

        let mut high_rate_format = mono_format();
        high_rate_format.sample_rate =
            SampleRate::new(fm_frame::AudioBlock::MAX_SAMPLE_RATE_HZ + 1).unwrap();
        let id = input_id(1);
        let mut mixer = MasterMixer::new(high_rate_format.clone()).unwrap();
        mixer
            .add_input(
                id,
                high_rate_format,
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState::default(),
            )
            .unwrap();
        mixer
            .set_input_state(
                id,
                InputState {
                    gain: Gain::SILENCE,
                    ..InputState::default()
                },
                4,
            )
            .unwrap();

        assert_eq!(
            mixer.mix_timed(timing(2), 2, &[], None),
            Err(AudioError::AudioBlock(
                fm_frame::AudioBlockError::SampleRateTooHigh {
                    actual: fm_frame::AudioBlock::MAX_SAMPLE_RATE_HZ + 1,
                    maximum: fm_frame::AudioBlock::MAX_SAMPLE_RATE_HZ,
                }
            ))
        );
        assert_eq!(mixer.current_linear_gain(id), Some(1.0));
    }

    #[test]
    fn gain_mute_and_follow_video_control_inputs() {
        let format = mono_format();
        let first_id = input_id(1);
        let second_id = input_id(2);
        let block = AudioBlock::from_planar(format.clone(), vec![vec![0.5; 4]]).unwrap();
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer.set_clipping_policy(ClippingPolicy::Allow);
        mixer
            .add_input(
                first_id,
                format.clone(),
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState {
                    pan: Pan::CENTER,
                    group: None,
                    gain: Gain::from_db(-6.020_6).unwrap(),
                    balance: Balance::CENTER,
                    muted: false,
                    soloed: false,
                    follow_video: false,
                },
            )
            .unwrap();
        mixer
            .add_input(
                second_id,
                format,
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState {
                    pan: Pan::CENTER,
                    group: None,
                    gain: Gain::UNITY,
                    balance: Balance::CENTER,
                    muted: false,
                    soloed: false,
                    follow_video: true,
                },
            )
            .unwrap();

        let output = mixer
            .mix(
                4,
                &[(first_id, &block), (second_id, &block)],
                Some(first_id),
            )
            .unwrap();
        for sample in output.block.plane(0).unwrap() {
            assert_close(*sample, 0.25);
        }
        let muted = InputState {
            pan: Pan::CENTER,
            group: None,
            gain: Gain::UNITY,
            balance: Balance::CENTER,
            muted: true,
            soloed: false,
            follow_video: true,
        };
        mixer.set_input_state(second_id, muted, 0).unwrap();
        let output = mixer
            .mix(4, &[(second_id, &block)], Some(second_id))
            .unwrap();
        assert!(
            output
                .block
                .plane(0)
                .unwrap()
                .iter()
                .all(|sample| *sample == 0.0)
        );
    }

    #[test]
    fn stereo_balance_is_bounded_and_ramps_the_opposite_channel() {
        assert_eq!(Balance::from_basis_points(-10_001), None);
        assert_eq!(Balance::from_basis_points(10_001), None);
        let format = stereo_format();
        let id = input_id(1);
        let block =
            AudioBlock::from_planar(format.clone(), vec![vec![1.0; 4], vec![1.0; 4]]).unwrap();
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer
            .add_input(
                id,
                format.clone(),
                ChannelMapping::identity(format.channels).unwrap(),
                InputState::default(),
            )
            .unwrap();
        mixer
            .set_input_state(
                id,
                InputState {
                    balance: Balance::from_basis_points(-10_000).unwrap(),
                    ..InputState::default()
                },
                4,
            )
            .unwrap();

        let output = mixer.mix(4, &[(id, &block)], Some(id)).unwrap();
        assert_eq!(output.block.plane(0).unwrap(), &[1.0, 1.0, 1.0, 1.0]);
        assert_eq!(output.block.plane(1).unwrap(), &[0.75, 0.5, 0.25, 0.0]);
        assert_eq!(mixer.current_normalized_balance(id), Some(-1.0));

        mixer
            .set_input_state(
                id,
                InputState {
                    balance: Balance::from_basis_points(10_000).unwrap(),
                    ..InputState::default()
                },
                0,
            )
            .unwrap();
        let output = mixer.mix(4, &[(id, &block)], Some(id)).unwrap();
        assert_eq!(output.block.plane(0).unwrap(), &[0.0, 0.0, 0.0, 0.0]);
        assert_eq!(output.block.plane(1).unwrap(), &[1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn solo_gates_other_strips_while_mute_and_follow_video_remain_independent() {
        let format = mono_format();
        let first = input_id(1);
        let second = input_id(2);
        let first_block = AudioBlock::from_planar(format.clone(), vec![vec![0.25; 2]]).unwrap();
        let second_block = AudioBlock::from_planar(format.clone(), vec![vec![0.5; 2]]).unwrap();
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        for input in [first, second] {
            mixer
                .add_input(
                    input,
                    format.clone(),
                    ChannelMapping::identity(format.channels.clone()).unwrap(),
                    InputState::default(),
                )
                .unwrap();
        }

        mixer
            .set_input_state(
                first,
                InputState {
                    soloed: true,
                    ..InputState::default()
                },
                0,
            )
            .unwrap();
        let output = mixer
            .mix(2, &[(first, &first_block), (second, &second_block)], None)
            .unwrap();
        assert_eq!(output.block.plane(0).unwrap(), &[0.25, 0.25]);

        mixer
            .set_input_state(
                first,
                InputState {
                    soloed: true,
                    follow_video: true,
                    ..InputState::default()
                },
                0,
            )
            .unwrap();
        let output = mixer
            .mix(2, &[(first, &first_block), (second, &second_block)], None)
            .unwrap();
        assert_eq!(output.block.plane(0).unwrap(), &[0.0, 0.0]);

        mixer
            .set_input_state(
                first,
                InputState {
                    muted: true,
                    soloed: true,
                    ..InputState::default()
                },
                0,
            )
            .unwrap();
        let output = mixer
            .mix(2, &[(first, &first_block), (second, &second_block)], None)
            .unwrap();
        assert_eq!(output.block.plane(0).unwrap(), &[0.0, 0.0]);
    }

    #[test]
    fn logical_alias_reuses_source_mapping_with_independent_strip_controls() {
        let format = mono_format();
        let source = input_id(1);
        let alias = input_id(2);
        let block = AudioBlock::from_planar(format.clone(), vec![vec![1.0; 4]]).unwrap();
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer
            .add_input(
                source,
                format,
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState::default(),
            )
            .unwrap();
        mixer
            .add_input_alias(
                alias,
                source,
                InputState {
                    pan: Pan::CENTER,
                    group: None,
                    gain: Gain::from_db(-6.020_6).unwrap(),
                    balance: Balance::CENTER,
                    muted: false,
                    soloed: false,
                    follow_video: true,
                },
            )
            .unwrap();

        let inactive = mixer.mix(4, &[(alias, &block)], None).unwrap();
        assert!(
            inactive
                .block
                .plane(0)
                .unwrap()
                .iter()
                .all(|sample| *sample == 0.0)
        );
        let selected = mixer.mix(4, &[(alias, &block)], Some(alias)).unwrap();
        assert!(
            selected
                .block
                .plane(0)
                .unwrap()
                .iter()
                .all(|sample| (*sample - 0.5).abs() < 0.000_01)
        );
        assert_eq!(mixer.input_state(source), Some(InputState::default()));
    }

    #[test]
    fn arbitrary_channel_routes_are_summed() {
        let input_format = stereo_format();
        let output_format = mono_format();
        let id = input_id(1);
        let map = ChannelMapping::new(
            input_format.channels.clone(),
            output_format.channels.clone(),
            vec![
                ChannelMappingRoute {
                    source: fm_types::Channel::Left,
                    destination: fm_types::Channel::Mono,
                    coefficient: Gain::UNITY.linear(),
                },
                ChannelMappingRoute {
                    source: fm_types::Channel::Right,
                    destination: fm_types::Channel::Mono,
                    coefficient: Gain::from_db(-6.020_6).unwrap().linear(),
                },
            ],
        )
        .unwrap();
        let block =
            AudioBlock::from_planar(input_format.clone(), vec![vec![0.25, 0.5], vec![0.5, 0.5]])
                .unwrap();
        let mut mixer = MasterMixer::new(output_format).unwrap();
        mixer.set_clipping_policy(ClippingPolicy::Allow);
        mixer
            .add_input(id, input_format, map, InputState::default())
            .unwrap();
        let output = mixer.mix(2, &[(id, &block)], None).unwrap();
        assert_close(output.block.plane(0).unwrap()[0], 0.5);
        assert_close(output.block.plane(0).unwrap()[1], 0.75);
    }

    #[test]
    fn mixer_executes_the_canonical_signed_channel_mapping() {
        let input_format = stereo_format();
        let output_format = mono_format();
        let id = input_id(1);
        let mapping = ChannelMapping::new(
            input_format.channels.clone(),
            output_format.channels.clone(),
            vec![
                ChannelMappingRoute {
                    source: fm_types::Channel::Left,
                    destination: fm_types::Channel::Mono,
                    coefficient: 1.0,
                },
                ChannelMappingRoute {
                    source: fm_types::Channel::Right,
                    destination: fm_types::Channel::Mono,
                    coefficient: -0.5,
                },
            ],
        )
        .unwrap();
        let planes = vec![vec![0.25, 0.5], vec![0.5, 0.5]];
        let expected = mapping
            .map(&canonical_block(timing(1), &input_format, planes.clone()))
            .unwrap();
        let block = AudioBlock::from_planar(input_format.clone(), planes).unwrap();
        let mut mixer = MasterMixer::new(output_format).unwrap();
        mixer.set_clipping_policy(ClippingPolicy::Allow);
        mixer
            .add_input(id, input_format, mapping, InputState::default())
            .unwrap();

        let output = mixer.mix(2, &[(id, &block)], None).unwrap();

        assert_eq!(output.block.planes(), expected.planes());
    }

    #[test]
    fn mixer_rejects_mapping_overflow_before_default_clipping() {
        let format = mono_format();
        let id = input_id(1);
        let mapping = ChannelMapping::new(
            format.channels.clone(),
            format.channels.clone(),
            vec![ChannelMappingRoute {
                source: fm_types::Channel::Mono,
                destination: fm_types::Channel::Mono,
                coefficient: f32::MAX,
            }],
        )
        .unwrap();
        let canonical = canonical_block(timing(1), &format, vec![vec![2.0]]);
        assert_eq!(
            mapping.map(&canonical),
            Err(ChannelMappingError::NonFiniteOutput {
                channel: 0,
                sample: 0,
            })
        );
        let block = AudioBlock::from_planar(format.clone(), vec![vec![2.0]]).unwrap();
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer
            .add_input(id, format, mapping, InputState::default())
            .unwrap();

        assert_eq!(
            mixer.mix(1, &[(id, &block)], None),
            Err(AudioError::NonFiniteSample {
                channel: 0,
                sample: 0,
            })
        );
        assert_eq!(mixer.current_linear_gain(id), Some(1.0));
    }

    #[test]
    fn add_input_rejects_same_width_wrong_mapping_layout_atomically() {
        let format = stereo_format();
        let id = input_id(1);
        let swapped =
            ChannelLayout::new(vec![fm_types::Channel::Right, fm_types::Channel::Left]).unwrap();
        let mapping = ChannelMapping::matching(swapped, format.channels.clone()).unwrap();
        let mut mixer = MasterMixer::new(format.clone()).unwrap();

        assert_eq!(
            mixer.add_input(id, format, mapping, InputState::default()),
            Err(AudioError::MappingLayoutMismatch)
        );
        assert_eq!(mixer.input_state(id), None);
    }

    #[test]
    fn mixer_rejects_an_unrepresentable_destination_layout() {
        let channels =
            ChannelLayout::new(vec![fm_types::Channel::Left, fm_types::Channel::Left]).unwrap();
        let format = AudioFormat {
            sample_rate: SampleRate::new(48_000).unwrap(),
            sample_format: SampleFormat::F32,
            channels,
        };

        assert!(matches!(
            MasterMixer::new(format),
            Err(AudioError::ChannelMapping(
                ChannelMappingError::DuplicateLayoutChannel {
                    side: ChannelMappingSide::Destination,
                    channel: fm_types::Channel::Left,
                }
            ))
        ));
    }

    #[test]
    fn ramps_are_linear_and_continue_across_blocks() {
        let format = mono_format();
        let id = input_id(1);
        let block = AudioBlock::from_planar(format.clone(), vec![vec![1.0; 2]]).unwrap();
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer.set_clipping_policy(ClippingPolicy::Allow);
        mixer
            .add_input(
                id,
                format,
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState::default(),
            )
            .unwrap();
        mixer
            .set_input_state(
                id,
                InputState {
                    gain: Gain::SILENCE,
                    ..InputState::default()
                },
                4,
            )
            .unwrap();
        let first = mixer.mix(2, &[(id, &block)], None).unwrap();
        assert_eq!(first.block.plane(0).unwrap(), &[0.75, 0.5]);
        let second = mixer.mix(2, &[(id, &block)], None).unwrap();
        assert_eq!(second.block.plane(0).unwrap(), &[0.25, 0.0]);
        assert_eq!(mixer.current_linear_gain(id), Some(0.0));
    }

    #[test]
    fn clipping_policy_is_explicit() {
        let format = mono_format();
        let id = input_id(1);
        let block = AudioBlock::from_planar(format.clone(), vec![vec![0.75]]).unwrap();
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer
            .add_input(
                id,
                format,
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState::default(),
            )
            .unwrap();
        let clamped = mixer.mix(1, &[(id, &block)], None).unwrap();
        assert_eq!(clamped.block.plane(0).unwrap(), &[0.75]);

        let second = input_id(2);
        mixer
            .add_input(
                second,
                mono_format(),
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState::default(),
            )
            .unwrap();
        let clamped = mixer
            .mix(1, &[(id, &block), (second, &block)], None)
            .unwrap();
        assert_eq!(clamped.block.plane(0).unwrap(), &[1.0]);
        mixer.set_clipping_policy(ClippingPolicy::Allow);
        let allowed = mixer
            .mix(1, &[(id, &block), (second, &block)], None)
            .unwrap();
        assert_eq!(allowed.block.plane(0).unwrap(), &[1.5]);
    }

    #[test]
    fn meters_report_peak_and_rms_per_channel() {
        let block = AudioBlock::from_planar(
            stereo_format(),
            vec![vec![1.0, -1.0, 0.0, 0.0], vec![0.5, -0.5, 0.5, -0.5]],
        )
        .unwrap();
        let meters = measure(&block);
        assert_close(meters.channels()[0].peak, 1.0);
        assert_close(meters.channels()[0].rms, 2.0_f32.sqrt() / 2.0);
        assert_close(meters.channels()[1].peak, 0.5);
        assert_close(meters.channels()[1].rms, 0.5);
    }

    #[test]
    fn errors_leave_configuration_and_ramps_unchanged() {
        let format = mono_format();
        let id = input_id(1);
        let block = AudioBlock::from_planar(format.clone(), vec![vec![1.0; 2]]).unwrap();
        let wrong =
            AudioBlock::from_planar(stereo_format(), vec![vec![1.0; 2], vec![1.0; 2]]).unwrap();
        let mut mixer = MasterMixer::new(format.clone()).unwrap();
        mixer
            .add_input(
                id,
                format.clone(),
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState::default(),
            )
            .unwrap();
        mixer
            .set_input_state(
                id,
                InputState {
                    gain: Gain::SILENCE,
                    ..InputState::default()
                },
                4,
            )
            .unwrap();
        assert_eq!(
            mixer.mix(2, &[(id, &wrong)], None),
            Err(AudioError::FormatMismatch)
        );
        assert_eq!(mixer.current_linear_gain(id), Some(1.0));
        let prior = mixer.input_state(id);
        assert_eq!(
            mixer.set_input_state(id, InputState::default(), MAX_RAMP_SAMPLES + 1),
            Err(AudioError::RampTooLong(MAX_RAMP_SAMPLES + 1))
        );
        assert_eq!(mixer.input_state(id), prior);
        assert_eq!(
            mixer.add_input(
                id,
                format,
                ChannelMapping::identity(mono_format().channels).unwrap(),
                InputState::default()
            ),
            Err(AudioError::DuplicateInput(id))
        );
        let rendered = mixer.mix(2, &[(id, &block)], None).unwrap();
        assert_eq!(rendered.block.plane(0).unwrap(), &[0.75, 0.5]);
    }

    #[test]
    fn malformed_or_unbounded_blocks_are_rejected() {
        let mut wrong_format = mono_format();
        wrong_format.sample_format = SampleFormat::I16;
        assert_eq!(
            AudioBlock::from_planar(wrong_format, vec![vec![0.0]]),
            Err(AudioError::UnsupportedSampleFormat(SampleFormat::I16))
        );
        assert!(matches!(
            AudioBlock::from_planar(stereo_format(), vec![vec![0.0]]),
            Err(AudioError::PlaneCountMismatch { .. })
        ));
        assert!(matches!(
            AudioBlock::from_planar(stereo_format(), vec![vec![0.0], vec![]]),
            Err(AudioError::PlaneLengthMismatch { .. })
        ));
        assert!(matches!(
            AudioBlock::silence(mono_format(), MAX_SAMPLES_PER_BLOCK + 1),
            Err(AudioError::SampleCountOutOfRange(_))
        ));
    }
}
