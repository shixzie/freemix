//! A deterministic audio render plane for the simulated daemon (AU-005).
//!
//! The native daemon renders its Program bus with [`fm_audio::MasterMixer`],
//! applying the persisted per-strip and Master [`StripDsp`] chains, the strip
//! faders and pans, the VCA-style groups, and the solo mode. The simulated
//! engine has no decoders, so this module drives the same mixer with one
//! deterministic simulated tone per input (or silence) and the same
//! [`fm_audio::LoudnessMeter`] metering the native runtime reports.
//!
//! [`SimulatedAudioConfig`] is a plain, protocol-neutral description of one
//! project revision: the daemon maps a `Project` onto it. Nothing here
//! depends on `fm-model` or `fm-engine`.
//!
//! # Determinism
//!
//! Every generator is phase-continuous and every mixer/DSP/meter state is
//! advanced only by [`SimulatedAudioPlane::render`]. Two planes built from
//! the same configuration and rendered through the same frames therefore
//! produce identical PCM, sample for sample.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use fm_audio::{
    AudioError, AudioGenerator, Balance, ChannelMeter, DspDynamicsReading, Gain, GroupState,
    InputState, LoudnessMeter, LoudnessReading, MAX_SAMPLES_PER_BLOCK, MIXER_GROUP_COUNT,
    MasterMixer, MasterMixerDelayError, MixerGroup, MixerMeterSlices, Pan, PlanarAudioSource,
    SilenceGenerator, SineGenerator, SoloMode,
};
use fm_frame::{
    ClockDomainId, MediaTimestamp, MediaTiming, NormalizedDuration, NormalizedTimestamp,
    OriginalTimestamp, SequenceNumber, TimeBase, TimingError,
};
use fm_switcher::{ProgramFrame, TransitionKind};
use fm_types::strip_dsp::{StripDsp, StripDspTarget};
use fm_types::{AudioFormat, FrameRate, InputId};

/// Level of every simulated input tone, in dBFS.
///
/// The simulated sources are diagnostic tones, not programme audio, so they
/// all sit at a fixed, comfortable level; strip and Master faders and chains
/// then decide what reaches Program.
pub const SIMULATED_TONE_LEVEL_DBFS: f32 = -18.0;

/// A deterministic simulated tone on one input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimulatedTone {
    /// Tone frequency in hertz, within `0..=`Nyquist.
    pub frequency_hz: f32,
}

impl SimulatedTone {
    #[must_use]
    pub const fn new(frequency_hz: f32) -> Self {
        Self { frequency_hz }
    }
}

/// The protocol-neutral fader, pan, delay, and switch state of one strip.
///
/// It mirrors the persisted model values without depending on the model:
/// gains are millidecibels, balance and pan are basis points, and the delay
/// is in samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimulatedStripState {
    pub gain_millidb: i32,
    pub balance_basis_points: i32,
    pub pan_basis_points: i32,
    pub delay_samples: u32,
    pub muted: bool,
    pub soloed: bool,
    pub follow_video: bool,
}

impl SimulatedStripState {
    /// Unity gain, centred, undelayed, unmuted, unsoloed, following video.
    pub const DEFAULT: Self = Self {
        gain_millidb: 0,
        balance_basis_points: 0,
        pan_basis_points: 0,
        delay_samples: 0,
        muted: false,
        soloed: false,
        follow_video: true,
    };

    fn input_state(self, input: InputId) -> Result<InputState, SimulatedAudioError> {
        Ok(InputState {
            gain: gain_from_millidb(self.gain_millidb)?,
            balance: Balance::from_basis_points(self.balance_basis_points)
                .ok_or(SimulatedAudioError::InvalidStripState { input })?,
            pan: Pan::from_basis_points(self.pan_basis_points)
                .ok_or(SimulatedAudioError::InvalidStripState { input })?,
            muted: self.muted,
            soloed: self.soloed,
            follow_video: self.follow_video,
            group: None,
        })
    }
}

impl Default for SimulatedStripState {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The state of one VCA-style strip group.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimulatedGroupState {
    pub gain_millidb: i32,
    pub muted: bool,
    pub soloed: bool,
    /// The group this group is nested in, numbered `1..=`[`MIXER_GROUP_COUNT`].
    pub parent: Option<u8>,
}

impl SimulatedGroupState {
    /// Unity gain, unmuted, unsoloed, top-level.
    pub const DEFAULT: Self = Self {
        gain_millidb: 0,
        muted: false,
        soloed: false,
        parent: None,
    };

    fn group_state(self) -> Result<GroupState, SimulatedAudioError> {
        Ok(GroupState {
            gain: gain_from_millidb(self.gain_millidb)?,
            muted: self.muted,
        })
    }
}

/// One simulated input strip.
#[derive(Clone, Debug, PartialEq)]
pub struct SimulatedAudioStrip {
    pub input: InputId,
    pub state: SimulatedStripState,
    /// The strip's persisted DSP chain (AU-005).
    pub dsp: StripDsp,
    /// The simulated source: a tone, or silence when `None`.
    pub tone: Option<SimulatedTone>,
    /// The strip's VCA-style group, numbered `1..=`[`MIXER_GROUP_COUNT`].
    pub group: Option<u8>,
}

impl SimulatedAudioStrip {
    /// A strip in `group`-less state.
    #[must_use]
    pub const fn new(
        input: InputId,
        state: SimulatedStripState,
        dsp: StripDsp,
        tone: Option<SimulatedTone>,
    ) -> Self {
        Self {
            input,
            state,
            dsp,
            tone,
            group: None,
        }
    }

    fn input_state(&self) -> Result<InputState, SimulatedAudioError> {
        let mut state = self.state.input_state(self.input)?;
        state.group = self
            .group
            .map(|number| {
                MixerGroup::new(number).ok_or(SimulatedAudioError::InvalidGroup { number })
            })
            .transpose()?;
        Ok(state)
    }
}

/// One project revision's audio configuration, independent of the model.
#[derive(Clone, Debug, PartialEq)]
pub struct SimulatedAudioConfig {
    /// The Master bus format; every simulated input shares it.
    pub format: AudioFormat,
    /// The video cadence that determines each frame's sample span.
    pub frame_rate: FrameRate,
    /// Every configured input strip; each input appears at most once.
    pub strips: Vec<SimulatedAudioStrip>,
    /// The Master bus DSP chain (AU-005).
    pub master_dsp: StripDsp,
    /// The four VCA-style strip groups, group 1 first.
    pub groups: [SimulatedGroupState; MIXER_GROUP_COUNT],
    /// How a soloed strip is heard.
    pub solo_mode: SoloMode,
}

impl SimulatedAudioConfig {
    /// An empty configuration: no strips, every group at unity, no solo.
    #[must_use]
    pub const fn new(format: AudioFormat, frame_rate: FrameRate) -> Self {
        Self {
            format,
            frame_rate,
            strips: Vec::new(),
            master_dsp: StripDsp::DEFAULT,
            groups: [SimulatedGroupState::DEFAULT; MIXER_GROUP_COUNT],
            solo_mode: SoloMode::InPlace,
        }
    }
}

/// Errors returned while configuring or rendering the simulated audio plane.
#[derive(Clone, Debug, PartialEq)]
pub enum SimulatedAudioError {
    /// The configured format is unusable, or `resync` was given a different
    /// format or frame rate than the plane was built with.
    FormatMismatch,
    /// The frame rate asks for zero samples in this frame.
    EmptyFrame { frame: u64 },
    /// A frame or configuration needs more samples than one mixer block holds.
    TooManySamples { samples: usize, maximum: usize },
    /// A program input is not one of the configured strips.
    UnknownInput(InputId),
    /// Two strips configure the same input.
    DuplicateInput(InputId),
    /// A strip's gain, balance, pan, or delay is out of range.
    InvalidStripState { input: InputId },
    /// A group number or parent is outside `1..=`[`MIXER_GROUP_COUNT`].
    InvalidGroup { number: u8 },
    /// A transition with a secondary input has no transition kind.
    MissingTransitionKind,
    /// The simulated plane does not render a Stinger's own audio policy.
    UnsupportedTransition(TransitionKind),
    /// A transition's exact rational gains are malformed.
    InvalidTransition {
        denominator: u32,
        start: u32,
        end: u32,
    },
    /// The absolute sample span does not fit the timeline.
    TimelineOverflow,
    /// Canonical media timing could not be built.
    Timing(TimingError),
    /// A strip delay was refused by the Master mixer.
    Delay(MasterMixerDelayError),
    /// A loudness meter could not be created.
    Loudness(fm_audio::DspError),
    /// The Master mixer refused the configuration or a render.
    Mixer(AudioError),
}

impl fmt::Display for SimulatedAudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FormatMismatch => {
                formatter.write_str("simulated audio format or frame rate mismatch")
            }
            Self::EmptyFrame { frame } => {
                write!(formatter, "video frame {frame} spans zero audio samples")
            }
            Self::TooManySamples { samples, maximum } => write!(
                formatter,
                "a render of {samples} samples exceeds the limit of {maximum}"
            ),
            Self::UnknownInput(input) => write!(formatter, "audio input {input} is not configured"),
            Self::DuplicateInput(input) => {
                write!(formatter, "audio input {input} is configured twice")
            }
            Self::InvalidStripState { input } => {
                write!(formatter, "audio strip {input} has an out-of-range value")
            }
            Self::InvalidGroup { number } => write!(
                formatter,
                "audio group {number} is outside 1..={MIXER_GROUP_COUNT}"
            ),
            Self::MissingTransitionKind => {
                formatter.write_str("a secondary program input needs a transition kind")
            }
            Self::UnsupportedTransition(kind) => write!(
                formatter,
                "simulated audio does not render the {kind:?} transition's own audio"
            ),
            Self::InvalidTransition {
                denominator,
                start,
                end,
            } => write!(
                formatter,
                "transition gains {start}/{denominator}..{end}/{denominator} are invalid"
            ),
            Self::TimelineOverflow => {
                formatter.write_str("simulated audio sample span overflows the timeline")
            }
            Self::Timing(error) => write!(formatter, "audio timing error: {error}"),
            Self::Delay(error) => write!(formatter, "strip delay error: {error}"),
            Self::Loudness(error) => write!(formatter, "loudness meter error: {error}"),
            Self::Mixer(error) => write!(formatter, "Master mixer error: {error}"),
        }
    }
}

impl std::error::Error for SimulatedAudioError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Timing(error) => Some(error),
            Self::Delay(error) => Some(error),
            Self::Loudness(error) => Some(error),
            Self::Mixer(error) => Some(error),
            _ => None,
        }
    }
}

impl From<AudioError> for SimulatedAudioError {
    fn from(value: AudioError) -> Self {
        Self::Mixer(value)
    }
}

/// The meter interval of the last rendered frame.
///
/// The shape mirrors the native daemon's `NativeAudioMeters`: Master first,
/// then every strip in ascending [`InputId`] order. The first
/// `end_sample - start_sample` samples of `monitor_planes` are valid.
pub struct SimulatedAudioMeters<'a> {
    pub frame: u64,
    pub start_sample: u64,
    pub end_sample: u64,
    pub channels: usize,
    pub sample_rate: u32,
    pub master: &'a [ChannelMeter],
    pub inputs: &'a [InputId],
    pub input_meters: &'a [ChannelMeter],
    pub monitor: &'a [ChannelMeter],
    pub monitor_solo: bool,
    pub monitor_planes: &'a [Vec<f32>],
    /// The Master chain (`None`) when it is running, first, then every strip
    /// with a running dynamics stage, in ascending input order.
    pub dynamics: &'a [(Option<InputId>, DspDynamicsReading)],
    /// BS.1770 loudness and true peak: the Master (`None`) first, then every
    /// strip, in ascending input order.
    pub loudness: &'a [(Option<InputId>, LoudnessReading)],
}

/// One deterministic source per input: a fixed-level tone or silence.
#[derive(Clone, Debug)]
enum SimulatedGenerator {
    Tone(SineGenerator),
    Silence(SilenceGenerator),
}

impl SimulatedGenerator {
    fn new(tone: Option<SimulatedTone>, format: &AudioFormat) -> Result<Self, SimulatedAudioError> {
        match tone {
            Some(tone) => {
                let amplitude = Gain::from_db(SIMULATED_TONE_LEVEL_DBFS)?;
                Ok(Self::Tone(SineGenerator::new(
                    format.clone(),
                    tone.frequency_hz,
                    amplitude,
                )?))
            }
            None => Ok(Self::Silence(SilenceGenerator::new(format.clone())?)),
        }
    }

    fn fill(&mut self, planes: &mut [Vec<f32>], samples: usize) -> Result<(), SimulatedAudioError> {
        let block = match self {
            Self::Tone(generator) => generator.generate(samples),
            Self::Silence(generator) => generator.generate(samples),
        }?;
        for (plane, generated) in planes.iter_mut().zip(block.planes()) {
            plane[..samples].copy_from_slice(&generated[..samples]);
        }
        Ok(())
    }
}

/// The exact source gains of one Program frame, mirroring the native plan.
#[derive(Clone, Copy, Debug)]
struct ProgramAudioPlan {
    primary: InputId,
    primary_gain: fm_audio::SourceGain,
    secondary: Option<(InputId, fm_audio::SourceGain)>,
}

impl ProgramAudioPlan {
    fn source_gain(self, input: InputId) -> fm_audio::SourceGain {
        if input == self.primary {
            self.primary_gain
        } else {
            self.secondary
                .filter(|(secondary, _)| *secondary == input)
                .map_or(fm_audio::SourceGain::UNITY, |(_, gain)| gain)
        }
    }
}

/// The simulated daemon's audio render plane; see the module documentation.
pub struct SimulatedAudioPlane {
    config: SimulatedAudioConfig,
    format: AudioFormat,
    frame_rate: FrameRate,
    clock_domain: ClockDomainId,
    max_samples: usize,
    mixer: MasterMixer,
    generators: BTreeMap<InputId, SimulatedGenerator>,
    source_planes: BTreeMap<InputId, Vec<Vec<f32>>>,
    program: Vec<Vec<f32>>,
    monitor: Vec<Vec<f32>>,
    master_meters: Vec<ChannelMeter>,
    input_meters: Vec<ChannelMeter>,
    monitor_meters: Vec<ChannelMeter>,
    input_loudness: Vec<LoudnessMeter>,
    loudness_inputs: Vec<InputId>,
    master_loudness: LoudnessMeter,
    dynamics: Vec<(Option<InputId>, DspDynamicsReading)>,
    loudness: Vec<(Option<InputId>, LoudnessReading)>,
    monitor_solo: bool,
    frame: u64,
    start_sample: u64,
    end_sample: u64,
    samples: usize,
}

impl SimulatedAudioPlane {
    /// Builds the plane for one project revision.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the format, frame rate, strips, groups,
    /// chains, or tones are unusable.
    pub fn new(
        config: SimulatedAudioConfig,
        clock_domain: ClockDomainId,
    ) -> Result<Self, SimulatedAudioError> {
        validate_config(&config)?;
        let format = config.format.clone();
        let frame_rate = config.frame_rate;
        let max_samples = max_frame_samples(format.sample_rate.hertz(), frame_rate)?;
        let channels = format.channels.channels().len();
        let mut mixer =
            MasterMixer::new(format.clone()).map_err(|_| SimulatedAudioError::FormatMismatch)?;
        apply_config(&mut mixer, &[], &config)?;
        let generators = build_generators(&config, None, &BTreeMap::new())?;
        let loudness_inputs: Vec<InputId> = generators.keys().copied().collect();
        let input_loudness = loudness_inputs
            .iter()
            .map(|_| {
                LoudnessMeter::new(format.sample_rate.hertz(), &format.channels)
                    .map_err(SimulatedAudioError::Loudness)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let master_loudness = LoudnessMeter::new(format.sample_rate.hertz(), &format.channels)
            .map_err(SimulatedAudioError::Loudness)?;
        let source_planes = config
            .strips
            .iter()
            .map(|strip| (strip.input, vec![vec![0.0; max_samples]; channels]))
            .collect();
        Ok(Self {
            config,
            format,
            frame_rate,
            clock_domain,
            max_samples,
            mixer,
            generators,
            source_planes,
            program: vec![vec![0.0; max_samples]; channels],
            monitor: vec![vec![0.0; max_samples]; channels],
            master_meters: vec![ChannelMeter::default(); channels],
            input_meters: vec![ChannelMeter::default(); loudness_inputs.len() * channels],
            monitor_meters: vec![ChannelMeter::default(); channels],
            input_loudness,
            loudness_inputs,
            master_loudness,
            dynamics: Vec::new(),
            loudness: Vec::new(),
            monitor_solo: false,
            frame: 0,
            start_sample: 0,
            end_sample: 0,
            samples: 0,
        })
    }

    #[must_use]
    pub const fn config(&self) -> &SimulatedAudioConfig {
        &self.config
    }

    /// The rendered Program PCM of the last frame; the first
    /// [`Self::program_samples`] samples of each plane are valid.
    #[must_use]
    pub fn program_planes(&self) -> &[Vec<f32>] {
        &self.program
    }

    /// The monitor bus PCM of the last frame; the first
    /// [`Self::program_samples`] samples of each plane are valid.
    #[must_use]
    pub fn monitor_planes(&self) -> &[Vec<f32>] {
        &self.monitor
    }

    /// Samples rendered in the last frame.
    #[must_use]
    pub const fn program_samples(&self) -> usize {
        self.samples
    }

    /// The last rendered frame number.
    #[must_use]
    pub const fn frame(&self) -> u64 {
        self.frame
    }

    /// The last frame's absolute sample span `(start, end)`.
    #[must_use]
    pub const fn sample_span(&self) -> (u64, u64) {
        (self.start_sample, self.end_sample)
    }

    /// Re-applies a project revision without restarting the plane.
    ///
    /// Strips are added and removed, faders, pans, delays, groups, solo mode,
    /// and DSP chains are retargeted (chains glide or crossfade exactly as in
    /// the native daemon), and tones keep their phase when their settings did
    /// not change. Loudness meters are carried across for inputs that remain.
    ///
    /// # Errors
    ///
    /// Returns [`SimulatedAudioError::FormatMismatch`] when the format or
    /// frame rate differs from the plane's, or a typed configuration error.
    /// Nothing changes on error.
    pub fn resync(&mut self, config: SimulatedAudioConfig) -> Result<(), SimulatedAudioError> {
        if config.format != self.format || config.frame_rate != self.frame_rate {
            return Err(SimulatedAudioError::FormatMismatch);
        }
        validate_config(&config)?;
        let generators = build_generators(&config, Some(&self.config), &self.generators)?;
        let mut mixer = self.mixer.clone();
        let existing: Vec<InputId> = self.config.strips.iter().map(|strip| strip.input).collect();
        apply_config(&mut mixer, &existing, &config)?;
        let loudness_inputs: Vec<InputId> = generators.keys().copied().collect();
        let previous: BTreeMap<InputId, LoudnessMeter> = self
            .loudness_inputs
            .iter()
            .copied()
            .zip(self.input_loudness.iter().cloned())
            .collect();
        let input_loudness = loudness_inputs
            .iter()
            .map(|input| match previous.get(input) {
                Some(meter) => Ok(meter.clone()),
                None => LoudnessMeter::new(self.format.sample_rate.hertz(), &self.format.channels)
                    .map_err(SimulatedAudioError::Loudness),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let channels = self.format.channels.channels().len();
        let max_samples = self.max_samples;
        let inputs: BTreeSet<InputId> = config.strips.iter().map(|strip| strip.input).collect();
        self.source_planes.retain(|input, _| inputs.contains(input));
        for &input in &loudness_inputs {
            self.source_planes
                .entry(input)
                .or_insert_with(|| vec![vec![0.0; max_samples]; channels]);
        }
        self.mixer = mixer;
        self.generators = generators;
        self.loudness_inputs = loudness_inputs;
        self.input_loudness = input_loudness;
        self.input_meters = vec![ChannelMeter::default(); self.loudness_inputs.len() * channels];
        self.master_meters = vec![ChannelMeter::default(); channels];
        self.monitor_meters = vec![ChannelMeter::default(); channels];
        self.monitor_solo = false;
        self.dynamics.clear();
        self.loudness.clear();
        self.config = config;
        Ok(())
    }

    /// Renders one video frame's Program interval.
    ///
    /// The frame's rational span fixes the sample count, the program inputs'
    /// linear transition gains weight their strips, and every other strip
    /// feeds Program at unity, exactly as the native daemon mixes decoded
    /// sources. `resets` restarts the integrated loudness of the Master
    /// and/or named strips before this frame is measured.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an empty or oversized span, an unknown
    /// program input, a malformed transition, or a refused mixer render.
    pub fn render(
        &mut self,
        program: ProgramFrame,
        frame: u64,
        resets: &[StripDspTarget],
    ) -> Result<(), SimulatedAudioError> {
        let (start_sample, end_sample) =
            frame_sample_span(frame, self.format.sample_rate.hertz(), self.frame_rate)
                .ok_or(SimulatedAudioError::TimelineOverflow)?;
        let samples = usize::try_from(end_sample - start_sample)
            .map_err(|_| SimulatedAudioError::TimelineOverflow)?;
        if samples == 0 {
            return Err(SimulatedAudioError::EmptyFrame { frame });
        }
        if samples > MAX_SAMPLES_PER_BLOCK || samples > self.max_samples {
            return Err(SimulatedAudioError::TooManySamples {
                samples,
                maximum: MAX_SAMPLES_PER_BLOCK.min(self.max_samples),
            });
        }
        self.mixer.set_dsp_block_capacity(samples);
        let plan = self.transition_plan(program)?;
        for &target in resets {
            let _ = self.reset_loudness(target);
        }
        for (&input, generator) in &mut self.generators {
            let planes = self
                .source_planes
                .get_mut(&input)
                .ok_or(SimulatedAudioError::UnknownInput(input))?;
            generator.fill(planes, samples)?;
        }
        let timing = output_audio_timing(
            frame,
            start_sample,
            end_sample,
            self.format.sample_rate.hertz(),
            self.clock_domain,
        )?;
        let mut active_video_inputs = [program.primary; 2];
        let active_video_input_count = match plan.secondary {
            Some((secondary, _)) => {
                active_video_inputs[1] = secondary;
                2
            }
            None => 1,
        };
        let sources: Vec<PlanarAudioSource<'_>> = self
            .generators
            .keys()
            .map(|input| {
                let planes = self
                    .source_planes
                    .get(input)
                    .ok_or(SimulatedAudioError::UnknownInput(*input))?;
                Ok(PlanarAudioSource {
                    input: *input,
                    sample_rate: self.format.sample_rate,
                    channel_layout: &self.format.channels,
                    planes,
                    samples,
                    source_gain: plan.source_gain(*input),
                })
            })
            .collect::<Result<_, SimulatedAudioError>>()?;
        self.monitor_solo = self.mixer.mix_planar_timed_into_with_monitor(
            timing,
            samples,
            &sources,
            &active_video_inputs[..active_video_input_count],
            &mut self.program,
            &mut self.monitor,
            MixerMeterSlices {
                master: &mut self.master_meters,
                inputs: &mut self.input_meters,
                monitor: &mut self.monitor_meters,
                loudness: Some(&mut self.input_loudness),
            },
            None,
        )?;
        self.master_loudness.process_planes(&self.program, samples);
        self.frame = frame;
        self.start_sample = start_sample;
        self.end_sample = end_sample;
        self.samples = samples;
        Ok(())
    }

    /// Takes the meter interval of the last rendered frame.
    ///
    /// Interval true peaks restart; integrated, momentary, and short-term
    /// loudness keep running until [`Self::reset_loudness`]. Before the first
    /// render the interval is empty and every meter reads silence.
    #[must_use]
    pub fn take_meters(&mut self) -> SimulatedAudioMeters<'_> {
        self.dynamics.clear();
        let running = |reading: DspDynamicsReading| {
            (reading.gate_db.is_some()
                || reading.expander_db.is_some()
                || reading.compressor_db.is_some()
                || reading.limiter_db.is_some())
            .then_some(reading)
        };
        if let Some(reading) = self.mixer.master_dynamics().and_then(running) {
            self.dynamics.push((None, reading));
        }
        for &input in &self.loudness_inputs {
            if let Some(reading) = self.mixer.input_dynamics(input).and_then(running) {
                self.dynamics.push((Some(input), reading));
            }
        }
        self.loudness.clear();
        self.loudness
            .push((None, self.master_loudness.take_reading()));
        for (&input, meter) in self.loudness_inputs.iter().zip(&mut self.input_loudness) {
            self.loudness.push((Some(input), meter.take_reading()));
        }
        SimulatedAudioMeters {
            frame: self.frame,
            start_sample: self.start_sample,
            end_sample: self.end_sample,
            channels: self.format.channels.channels().len(),
            sample_rate: self.format.sample_rate.hertz(),
            master: &self.master_meters,
            inputs: &self.loudness_inputs,
            input_meters: &self.input_meters,
            monitor: &self.monitor_meters,
            monitor_solo: self.monitor_solo,
            monitor_planes: &self.monitor,
            dynamics: &self.dynamics,
            loudness: &self.loudness,
        }
    }

    /// Restarts the integrated loudness, loudness range, and maximum true
    /// peak of Master (`None`) or one strip, leaving momentary and short-term
    /// loudness running; `false` when that meter does not exist.
    #[must_use]
    pub fn reset_loudness(&mut self, target: StripDspTarget) -> bool {
        match target {
            StripDspTarget::Master => {
                self.master_loudness.reset_measurement();
                true
            }
            StripDspTarget::Input(input) => self
                .loudness_inputs
                .iter()
                .position(|candidate| *candidate == input)
                .and_then(|index| self.input_loudness.get_mut(index))
                .map(LoudnessMeter::reset_measurement)
                .is_some(),
        }
    }

    /// The Program source gains of one frame, mirroring the native plan: the
    /// primary and secondary crossfade linearly across the frame, and every
    /// other input stays at unity.
    fn transition_plan(
        &self,
        program: ProgramFrame,
    ) -> Result<ProgramAudioPlan, SimulatedAudioError> {
        if self.mixer.input_state(program.primary).is_none() {
            return Err(SimulatedAudioError::UnknownInput(program.primary));
        }
        let Some(secondary) = program
            .secondary
            .filter(|secondary| *secondary != program.primary)
        else {
            return Ok(ProgramAudioPlan {
                primary: program.primary,
                primary_gain: fm_audio::SourceGain::UNITY,
                secondary: None,
            });
        };
        if self.mixer.input_state(secondary).is_none() {
            return Err(SimulatedAudioError::UnknownInput(secondary));
        }
        let Some(kind) = program.transition_kind else {
            return Err(SimulatedAudioError::MissingTransitionKind);
        };
        if let TransitionKind::Stinger(_) = kind {
            return Err(SimulatedAudioError::UnsupportedTransition(kind));
        }
        let invalid = SimulatedAudioError::InvalidTransition {
            denominator: program.mix_denominator,
            start: program.mix_start_numerator,
            end: program.mix_end_numerator,
        };
        let secondary_gain = fm_audio::SourceGain::new(
            program.mix_start_numerator,
            program.mix_end_numerator,
            program.mix_denominator,
        )
        .map_err(|_| invalid.clone())?;
        let primary_start = program
            .mix_denominator
            .checked_sub(program.mix_start_numerator)
            .ok_or_else(|| invalid.clone())?;
        let primary_end = program
            .mix_denominator
            .checked_sub(program.mix_end_numerator)
            .ok_or_else(|| invalid.clone())?;
        let primary_gain =
            fm_audio::SourceGain::new(primary_start, primary_end, program.mix_denominator)
                .map_err(|_| invalid)?;
        Ok(ProgramAudioPlan {
            primary: program.primary,
            primary_gain,
            secondary: Some((secondary, secondary_gain)),
        })
    }
}

/// Validates one revision before anything is built.
fn validate_config(config: &SimulatedAudioConfig) -> Result<(), SimulatedAudioError> {
    max_frame_samples(config.format.sample_rate.hertz(), config.frame_rate)?;
    let max_delay =
        u32::try_from(fm_audio::MAX_SAMPLE_DELAY_SAMPLES).expect("the sample-delay bound fits u32");
    for (index, strip) in config.strips.iter().enumerate() {
        if config.strips[..index]
            .iter()
            .any(|previous| previous.input == strip.input)
        {
            return Err(SimulatedAudioError::DuplicateInput(strip.input));
        }
        if let Some(number) = strip.group
            && MixerGroup::new(number).is_none()
        {
            return Err(SimulatedAudioError::InvalidGroup { number });
        }
        if strip.state.delay_samples > max_delay {
            return Err(SimulatedAudioError::InvalidStripState { input: strip.input });
        }
    }
    for group in &config.groups {
        if let Some(number) = group.parent
            && MixerGroup::new(number).is_none()
        {
            return Err(SimulatedAudioError::InvalidGroup { number });
        }
    }
    Ok(())
}

/// Adds, removes, and retargets everything one revision describes.
fn apply_config(
    mixer: &mut MasterMixer,
    existing: &[InputId],
    config: &SimulatedAudioConfig,
) -> Result<(), SimulatedAudioError> {
    for input in existing {
        if !config.strips.iter().any(|strip| strip.input == *input) {
            mixer.remove_input(*input)?;
        }
    }
    let mapping = fm_audio::ChannelMapping::identity(config.format.channels.clone())
        .map_err(|_| SimulatedAudioError::FormatMismatch)?;
    for strip in &config.strips {
        let state = strip.input_state()?;
        if mixer.input_state(strip.input).is_none() {
            mixer.add_input(strip.input, config.format.clone(), mapping.clone(), state)?;
        }
        let delay = usize::try_from(strip.state.delay_samples)
            .map_err(|_| SimulatedAudioError::InvalidStripState { input: strip.input })?;
        mixer
            .set_input_delay(strip.input, delay)
            .map_err(SimulatedAudioError::Delay)?;
        mixer.set_input_state(strip.input, state, 0)?;
        mixer.set_input_dsp(strip.input, &strip.dsp)?;
    }
    for (index, group) in config.groups.iter().enumerate() {
        let number = u8::try_from(index + 1).expect("a mixer group index fits u8");
        let mixer_group = MixerGroup::new(number).expect("the group count matches the mixer");
        mixer.set_group_state(mixer_group, group.group_state()?, 0)?;
        mixer.set_group_soloed(mixer_group, group.soloed);
    }
    // Clear every parent first so a topology change that moves a group under
    // a former child is not refused as a transient cycle.
    for index in 0..MIXER_GROUP_COUNT {
        let number = u8::try_from(index + 1).expect("a mixer group index fits u8");
        let mixer_group = MixerGroup::new(number).expect("the group count matches the mixer");
        mixer.set_group_parent(mixer_group, None)?;
    }
    for (index, group) in config.groups.iter().enumerate() {
        let number = u8::try_from(index + 1).expect("a mixer group index fits u8");
        let mixer_group = MixerGroup::new(number).expect("the group count matches the mixer");
        let parent = group
            .parent
            .map(|parent| {
                MixerGroup::new(parent).ok_or(SimulatedAudioError::InvalidGroup { number: parent })
            })
            .transpose()?;
        mixer.set_group_parent(mixer_group, parent)?;
    }
    mixer.set_master_dsp(&config.master_dsp)?;
    mixer.set_solo_mode(config.solo_mode);
    Ok(())
}

/// Builds one generator per strip, keeping a generator whose tone did not
/// change so its phase stays continuous across a revision.
fn build_generators(
    config: &SimulatedAudioConfig,
    previous_config: Option<&SimulatedAudioConfig>,
    previous: &BTreeMap<InputId, SimulatedGenerator>,
) -> Result<BTreeMap<InputId, SimulatedGenerator>, SimulatedAudioError> {
    let mut generators = BTreeMap::new();
    for strip in &config.strips {
        let unchanged = previous_config
            .and_then(|config| config.strips.iter().find(|old| old.input == strip.input))
            .is_some_and(|old| old.tone == strip.tone);
        let generator = match previous.get(&strip.input) {
            Some(generator) if unchanged => generator.clone(),
            _ => SimulatedGenerator::new(strip.tone, &config.format)?,
        };
        generators.insert(strip.input, generator);
    }
    Ok(generators)
}

/// The largest number of samples one frame can span.
fn max_frame_samples(
    sample_rate_hz: u32,
    frame_rate: FrameRate,
) -> Result<usize, SimulatedAudioError> {
    let numerator = u128::from(sample_rate_hz) * u128::from(frame_rate.denominator());
    let maximum = numerator.div_ceil(u128::from(frame_rate.numerator()));
    let maximum = usize::try_from(maximum).map_err(|_| SimulatedAudioError::TooManySamples {
        samples: usize::MAX,
        maximum: MAX_SAMPLES_PER_BLOCK,
    })?;
    if maximum == 0 {
        return Err(SimulatedAudioError::EmptyFrame { frame: 0 });
    }
    if maximum > MAX_SAMPLES_PER_BLOCK {
        return Err(SimulatedAudioError::TooManySamples {
            samples: maximum,
            maximum: MAX_SAMPLES_PER_BLOCK,
        });
    }
    Ok(maximum)
}

/// The absolute `(start, end)` sample span of `frame`, floored exactly like
/// the native daemon's rational cadence.
fn frame_sample_span(frame: u64, sample_rate_hz: u32, frame_rate: FrameRate) -> Option<(u64, u64)> {
    let samples_per_frame = u128::from(sample_rate_hz) * u128::from(frame_rate.denominator());
    let denominator = u128::from(frame_rate.numerator());
    let start = u128::from(frame)
        .checked_mul(samples_per_frame)?
        .checked_div(denominator)?;
    let end = u128::from(frame.checked_add(1)?)
        .checked_mul(samples_per_frame)?
        .checked_div(denominator)?;
    Some((u64::try_from(start).ok()?, u64::try_from(end).ok()?))
}

/// Canonical timing for one frame's absolute sample span.
fn output_audio_timing(
    frame: u64,
    start_sample: u64,
    end_sample: u64,
    sample_rate_hz: u32,
    clock_domain: ClockDomainId,
) -> Result<MediaTiming, SimulatedAudioError> {
    let start_ns = normalized_sample_endpoint(start_sample, sample_rate_hz)?;
    let end_ns = normalized_sample_endpoint(end_sample, sample_rate_hz)?;
    let duration_ns =
        u64::try_from(end_ns - start_ns).map_err(|_| SimulatedAudioError::TimelineOverflow)?;
    let time_base = TimeBase::new(1, sample_rate_hz).map_err(|_| SimulatedAudioError::FormatMismatch)?;
    let start_tick =
        i64::try_from(start_sample).map_err(|_| SimulatedAudioError::TimelineOverflow)?;
    MediaTiming::new(
        OriginalTimestamp::new(MediaTimestamp::new(start_tick), time_base),
        NormalizedTimestamp::from_nanos(start_ns),
        NormalizedDuration::from_nanos(duration_ns).map_err(SimulatedAudioError::Timing)?,
        clock_domain,
        SequenceNumber::new(frame),
    )
    .map_err(SimulatedAudioError::Timing)
}

/// `sample` as nanoseconds on the normalized timeline, floored.
fn normalized_sample_endpoint(
    sample: u64,
    sample_rate_hz: u32,
) -> Result<i64, SimulatedAudioError> {
    i128::from(sample)
        .checked_mul(1_000_000_000)
        .and_then(|nanos| nanos.checked_div(i128::from(sample_rate_hz)))
        .and_then(|nanos| i64::try_from(nanos).ok())
        .ok_or(SimulatedAudioError::TimelineOverflow)
}

/// A millidecibel fader as a mixer gain, exactly as the native daemon maps it.
#[allow(clippy::cast_precision_loss)]
fn gain_from_millidb(millidb: i32) -> Result<Gain, SimulatedAudioError> {
    Ok(Gain::from_db(millidb as f32 / 1_000.0)?)
}
