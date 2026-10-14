//! Deterministic monitor/PFL bus with isolated talkback routes.
//!
//! The monitor bus renders one of three sources into caller-owned monitor
//! planes: the program Master, the sum of PFL-selected strip taps, or one aux
//! bus tap. A monitor level and a fixed [`MONITOR_DIM_DB`] dim (optionally
//! engaged automatically while any talkback route is active) are applied.
//!
//! Talkback is isolated from program by construction:
//!
//! - the program Master is only ever borrowed immutably by [`MonitorBus::render`];
//! - [`TalkbackDestination`] has only guest and camera variants, so no route
//!   can target the program Master or the monitor output;
//! - talkback audio is exposed only as a mono `&[f32]` through
//!   [`TalkbackFeed`], which has no conversion into [`crate::AudioBlock`] or the
//!   `&[Vec<f32>]` planes required by [`crate::PlanarAudioSource`], so it cannot
//!   be submitted to [`crate::MasterMixer`] without an explicit copy.
//!
//! ```compile_fail
//! use std::num::NonZeroU128;
//!
//! use fm_audio::{MonitorBus, PlanarAudioSource, SourceGain, TalkbackDestination, TalkbackRoute};
//! use fm_types::{ChannelLayout, InputId, SampleRate};
//!
//! let mut bus = MonitorBus::new(2, 4).unwrap();
//! bus.add_talkback_route(TalkbackRoute::new(TalkbackDestination::Guest(1))).unwrap();
//! let feed = bus.talkback_feed(TalkbackDestination::Guest(1)).unwrap();
//! let layout = ChannelLayout::stereo();
//! // A talkback feed is not a set of program planes.
//! let _source = PlanarAudioSource {
//!     input: InputId::new(NonZeroU128::new(1).unwrap()),
//!     sample_rate: SampleRate::new(48_000).unwrap(),
//!     channel_layout: &layout,
//!     planes: feed.samples(),
//!     samples: 4,
//!     source_gain: SourceGain::UNITY,
//! };
//! ```
//!
//! Control-plane methods may allocate (talkback feed buffers are sized when a
//! route is added); [`MonitorBus::render`] never allocates and validates every
//! input before writing outputs.

use std::fmt;

use crate::{MAX_SAMPLES_PER_BLOCK, MIN_GAIN_DB};

/// Maximum monitor bus width in channels.
pub const MAX_MONITOR_CHANNELS: usize = 8;
/// Maximum number of strip indices addressable by the PFL selection.
pub const MAX_MONITOR_STRIPS: usize = 64;
/// Maximum number of aux buses addressable as a monitor source.
pub const MAX_MONITOR_AUX_BUSES: usize = 8;
/// Maximum number of simultaneously configured talkback routes.
pub const MAX_TALKBACK_ROUTES: usize = 16;
/// Fixed attenuation applied while the monitor is dimmed.
pub const MONITOR_DIM_DB: f32 = -20.0;
/// Highest monitor level accepted by [`MonitorBus::set_level_db`].
pub const MAX_MONITOR_LEVEL_DB: f32 = 12.0;
/// Highest talkback route gain.
pub const MAX_TALKBACK_GAIN_DB: f32 = 12.0;

/// Errors returned by the monitor bus.
#[derive(Clone, Debug, PartialEq)]
pub enum MonitorError {
    ChannelCountOutOfRange { actual: usize, maximum: usize },
    BlockSamplesOutOfRange { actual: usize, maximum: usize },
    StripIndexOutOfRange { strip: usize, maximum: usize },
    AuxIndexOutOfRange { aux: usize, maximum: usize },
    DuplicateTap { index: usize },
    InvalidLevelDb(f32),
    InvalidTalkbackGainDb(f32),
    TalkbackRouteLimit { maximum: usize },
    DuplicateTalkbackRoute(TalkbackDestination),
    UnknownTalkbackRoute(TalkbackDestination),
    PlaneCountMismatch { expected: usize, actual: usize },
    PlaneLengthMismatch { expected: usize, actual: usize },
    NonFiniteSample { sample: usize },
}

impl fmt::Display for MonitorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelCountOutOfRange { actual, maximum } => write!(
                formatter,
                "monitor channel count {actual} is outside 1..={maximum}"
            ),
            Self::BlockSamplesOutOfRange { actual, maximum } => write!(
                formatter,
                "monitor block size {actual} is outside 1..={maximum}"
            ),
            Self::StripIndexOutOfRange { strip, maximum } => {
                write!(formatter, "monitor strip {strip} is outside 0..{maximum}")
            }
            Self::AuxIndexOutOfRange { aux, maximum } => {
                write!(formatter, "monitor aux {aux} is outside 0..{maximum}")
            }
            Self::DuplicateTap { index } => {
                write!(formatter, "monitor tap {index} was supplied more than once")
            }
            Self::InvalidLevelDb(db) => write!(
                formatter,
                "monitor level {db} dB is outside {MIN_GAIN_DB}..={MAX_MONITOR_LEVEL_DB}"
            ),
            Self::InvalidTalkbackGainDb(db) => write!(
                formatter,
                "talkback gain {db} dB is outside {MIN_GAIN_DB}..={MAX_TALKBACK_GAIN_DB}"
            ),
            Self::TalkbackRouteLimit { maximum } => {
                write!(formatter, "talkback route limit of {maximum} reached")
            }
            Self::DuplicateTalkbackRoute(destination) => {
                write!(
                    formatter,
                    "talkback route to {destination:?} already exists"
                )
            }
            Self::UnknownTalkbackRoute(destination) => {
                write!(formatter, "no talkback route to {destination:?}")
            }
            Self::PlaneCountMismatch { expected, actual } => write!(
                formatter,
                "monitor expected {expected} planes but received {actual}"
            ),
            Self::PlaneLengthMismatch { expected, actual } => write!(
                formatter,
                "monitor plane has {actual} samples; expected {expected}"
            ),
            Self::NonFiniteSample { sample } => {
                write!(formatter, "monitor input sample {sample} is not finite")
            }
        }
    }
}

impl std::error::Error for MonitorError {}

/// Source rendered to the monitor output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MonitorSource {
    /// The program Master mix.
    #[default]
    Master,
    /// The sum of PFL-selected strip taps.
    Pfl,
    /// One aux bus tap, `0..MAX_MONITOR_AUX_BUSES`.
    Aux(usize),
}

/// Opaque identifier of the selected monitor output device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonitorDeviceId(pub u32);

/// A talkback destination. There is deliberately no program/Master variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TalkbackDestination {
    /// A remote guest return feed.
    Guest(u16),
    /// A camera operator intercom feed.
    Camera(u16),
}

/// Configuration of one talkback route.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TalkbackRoute {
    pub destination: TalkbackDestination,
    /// Route gain in dB, `MIN_GAIN_DB..=MAX_TALKBACK_GAIN_DB`.
    pub gain_db: f32,
    /// Whether the talk key is engaged for this route.
    pub active: bool,
}

impl TalkbackRoute {
    /// Returns an inactive unity-gain route.
    #[must_use]
    pub const fn new(destination: TalkbackDestination) -> Self {
        Self {
            destination,
            gain_db: 0.0,
            active: false,
        }
    }
}

/// A borrowed planar tap (pre-fader strip or aux bus) offered to the monitor.
#[derive(Clone, Copy, Debug)]
pub struct MonitorTap<'a> {
    /// Strip index for PFL taps or aux index for aux taps.
    pub index: usize,
    /// Planes in monitor channel order.
    pub planes: &'a [&'a [f32]],
}

/// Read-only view of one rendered talkback feed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TalkbackFeed<'a> {
    destination: TalkbackDestination,
    samples: &'a [f32],
}

impl<'a> TalkbackFeed<'a> {
    /// Destination of this feed.
    #[must_use]
    pub const fn destination(&self) -> TalkbackDestination {
        self.destination
    }

    /// Mono talkback samples of the most recent render.
    #[must_use]
    pub const fn samples(&self) -> &'a [f32] {
        self.samples
    }
}

/// Telemetry for the most recent monitor render.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorTelemetry {
    pub source: MonitorSource,
    pub samples: usize,
    /// Number of PFL-selected strip taps summed (zero unless source is PFL).
    pub pfl_taps_mixed: usize,
    /// Whether the selected aux tap was present (false unless source is aux).
    pub aux_present: bool,
    pub dimmed: bool,
    /// Linear gain applied to the monitor output (level and dim).
    pub gain: f32,
    pub active_talkback_routes: usize,
    pub device: Option<MonitorDeviceId>,
}

#[derive(Clone, Debug)]
struct RouteState {
    route: TalkbackRoute,
    gain: f32,
    feed: Vec<f32>,
    len: usize,
}

/// A bounded monitor/PFL bus with isolated talkback feeds.
#[derive(Clone, Debug)]
pub struct MonitorBus {
    channels: usize,
    max_block_samples: usize,
    source: MonitorSource,
    pfl: u64,
    dim: bool,
    dim_on_talkback: bool,
    level_db: f32,
    level: f32,
    device: Option<MonitorDeviceId>,
    routes: Vec<RouteState>,
}

impl MonitorBus {
    /// Creates a monitor bus of `channels` width for blocks of at most
    /// `max_block_samples` samples. The bus starts on Master at unity level,
    /// undimmed, with no PFL selection, device, or talkback routes.
    ///
    /// # Errors
    ///
    /// Returns an error for an out-of-range channel count or block size.
    pub fn new(channels: usize, max_block_samples: usize) -> Result<Self, MonitorError> {
        if channels == 0 || channels > MAX_MONITOR_CHANNELS {
            return Err(MonitorError::ChannelCountOutOfRange {
                actual: channels,
                maximum: MAX_MONITOR_CHANNELS,
            });
        }
        if max_block_samples == 0 || max_block_samples > MAX_SAMPLES_PER_BLOCK {
            return Err(MonitorError::BlockSamplesOutOfRange {
                actual: max_block_samples,
                maximum: MAX_SAMPLES_PER_BLOCK,
            });
        }
        Ok(Self {
            channels,
            max_block_samples,
            source: MonitorSource::Master,
            pfl: 0,
            dim: false,
            dim_on_talkback: false,
            level_db: 0.0,
            level: 1.0,
            device: None,
            routes: Vec::new(),
        })
    }

    /// Monitor bus width in channels.
    #[must_use]
    pub const fn channels(&self) -> usize {
        self.channels
    }

    /// Selects the monitor source.
    ///
    /// # Errors
    ///
    /// Returns [`MonitorError::AuxIndexOutOfRange`] for an invalid aux index.
    pub fn set_source(&mut self, source: MonitorSource) -> Result<(), MonitorError> {
        if let MonitorSource::Aux(aux) = source
            && aux >= MAX_MONITOR_AUX_BUSES
        {
            return Err(MonitorError::AuxIndexOutOfRange {
                aux,
                maximum: MAX_MONITOR_AUX_BUSES,
            });
        }
        self.source = source;
        Ok(())
    }

    /// Currently selected monitor source.
    #[must_use]
    pub const fn source(&self) -> MonitorSource {
        self.source
    }

    /// Adds or removes one strip from the PFL selection.
    ///
    /// # Errors
    ///
    /// Returns [`MonitorError::StripIndexOutOfRange`] for an invalid strip.
    pub fn set_pfl(&mut self, strip: usize, selected: bool) -> Result<(), MonitorError> {
        let bit = strip_bit(strip)?;
        if selected {
            self.pfl |= bit;
        } else {
            self.pfl &= !bit;
        }
        Ok(())
    }

    /// Clears the PFL selection.
    pub fn clear_pfl(&mut self) {
        self.pfl = 0;
    }

    /// Whether `strip` is PFL-selected.
    #[must_use]
    pub fn is_pfl(&self, strip: usize) -> bool {
        strip_bit(strip).is_ok_and(|bit| self.pfl & bit != 0)
    }

    /// PFL-selected strips in ascending order.
    pub fn pfl_strips(&self) -> impl Iterator<Item = usize> + '_ {
        (0..MAX_MONITOR_STRIPS).filter(|strip| self.pfl & (1_u64 << strip) != 0)
    }

    /// Engages or releases the fixed [`MONITOR_DIM_DB`] dim.
    pub fn set_dim(&mut self, dim: bool) {
        self.dim = dim;
    }

    /// Whether the manual dim is engaged.
    #[must_use]
    pub const fn dim(&self) -> bool {
        self.dim
    }

    /// Automatically dims the monitor while any talkback route is active.
    pub fn set_dim_on_talkback(&mut self, enabled: bool) {
        self.dim_on_talkback = enabled;
    }

    /// Sets the monitor level in dB.
    ///
    /// # Errors
    ///
    /// Returns [`MonitorError::InvalidLevelDb`] outside
    /// `MIN_GAIN_DB..=MAX_MONITOR_LEVEL_DB`.
    pub fn set_level_db(&mut self, db: f32) -> Result<(), MonitorError> {
        if !db.is_finite() || !(MIN_GAIN_DB..=MAX_MONITOR_LEVEL_DB).contains(&db) {
            return Err(MonitorError::InvalidLevelDb(db));
        }
        self.level_db = db;
        self.level = db_to_linear(db);
        Ok(())
    }

    /// Monitor level in dB.
    #[must_use]
    pub const fn level_db(&self) -> f32 {
        self.level_db
    }

    /// Selects (or clears) the monitor output device. No device I/O occurs.
    pub fn select_device(&mut self, device: Option<MonitorDeviceId>) {
        self.device = device;
    }

    /// Currently selected monitor output device.
    #[must_use]
    pub const fn device(&self) -> Option<MonitorDeviceId> {
        self.device
    }

    /// Adds a talkback route and sizes its feed buffer.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid gain, a duplicate destination, or when
    /// [`MAX_TALKBACK_ROUTES`] routes already exist.
    pub fn add_talkback_route(&mut self, route: TalkbackRoute) -> Result<(), MonitorError> {
        let gain = talkback_gain(route.gain_db)?;
        if self.route_index(route.destination).is_some() {
            return Err(MonitorError::DuplicateTalkbackRoute(route.destination));
        }
        if self.routes.len() >= MAX_TALKBACK_ROUTES {
            return Err(MonitorError::TalkbackRouteLimit {
                maximum: MAX_TALKBACK_ROUTES,
            });
        }
        self.routes.push(RouteState {
            route,
            gain,
            feed: vec![0.0; self.max_block_samples],
            len: 0,
        });
        Ok(())
    }

    /// Removes a talkback route.
    ///
    /// # Errors
    ///
    /// Returns [`MonitorError::UnknownTalkbackRoute`] when absent.
    pub fn remove_talkback_route(
        &mut self,
        destination: TalkbackDestination,
    ) -> Result<TalkbackRoute, MonitorError> {
        let index = self
            .route_index(destination)
            .ok_or(MonitorError::UnknownTalkbackRoute(destination))?;
        Ok(self.routes.remove(index).route)
    }

    /// Engages or releases the talk key of one route.
    ///
    /// # Errors
    ///
    /// Returns [`MonitorError::UnknownTalkbackRoute`] when absent.
    pub fn set_talkback_active(
        &mut self,
        destination: TalkbackDestination,
        active: bool,
    ) -> Result<(), MonitorError> {
        let index = self
            .route_index(destination)
            .ok_or(MonitorError::UnknownTalkbackRoute(destination))?;
        self.routes[index].route.active = active;
        Ok(())
    }

    /// Sets the gain of one route.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid gain or an unknown route.
    pub fn set_talkback_gain_db(
        &mut self,
        destination: TalkbackDestination,
        gain_db: f32,
    ) -> Result<(), MonitorError> {
        let gain = talkback_gain(gain_db)?;
        let index = self
            .route_index(destination)
            .ok_or(MonitorError::UnknownTalkbackRoute(destination))?;
        let state = &mut self.routes[index];
        state.route.gain_db = gain_db;
        state.gain = gain;
        Ok(())
    }

    /// Configured talkback routes in insertion order.
    pub fn talkback_routes(&self) -> impl Iterator<Item = TalkbackRoute> + '_ {
        self.routes.iter().map(|state| state.route)
    }

    /// The most recently rendered feed for one route.
    #[must_use]
    pub fn talkback_feed(&self, destination: TalkbackDestination) -> Option<TalkbackFeed<'_>> {
        self.route_index(destination).map(|index| {
            let state = &self.routes[index];
            TalkbackFeed {
                destination,
                samples: &state.feed[..state.len],
            }
        })
    }

    /// Renders the monitor output and all talkback feeds for one block.
    ///
    /// `program` is the Master mix, borrowed read-only. `strips` are pre-fader
    /// PFL taps indexed by strip; `auxes` are aux bus taps indexed by aux.
    /// Unselected taps are validated but not mixed; a selected strip or aux without a tap
    /// contributes silence. `talkback_mic` is the mono talkback microphone and
    /// only ever reaches talkback feeds, never `monitor_out` or program.
    ///
    /// # Errors
    ///
    /// Returns an error for plane-count or length mismatches, out-of-range or
    /// duplicate tap indices, or non-finite samples in any consumed input.
    /// Validation completes before any output changes.
    pub fn render(
        &mut self,
        program: &[&[f32]],
        strips: &[MonitorTap<'_>],
        auxes: &[MonitorTap<'_>],
        talkback_mic: &[f32],
        monitor_out: &mut [&mut [f32]],
    ) -> Result<MonitorTelemetry, MonitorError> {
        let samples = talkback_mic.len();
        if samples == 0 || samples > self.max_block_samples {
            return Err(MonitorError::BlockSamplesOutOfRange {
                actual: samples,
                maximum: self.max_block_samples,
            });
        }
        validate_finite(talkback_mic)?;
        self.validate_planes(program, samples)?;
        if monitor_out.len() != self.channels {
            return Err(MonitorError::PlaneCountMismatch {
                expected: self.channels,
                actual: monitor_out.len(),
            });
        }
        for plane in monitor_out.iter() {
            check_len(plane.len(), samples)?;
        }
        self.validate_taps(strips, samples, MAX_MONITOR_STRIPS, |strip, maximum| {
            MonitorError::StripIndexOutOfRange { strip, maximum }
        })?;
        self.validate_taps(auxes, samples, MAX_MONITOR_AUX_BUSES, |aux, maximum| {
            MonitorError::AuxIndexOutOfRange { aux, maximum }
        })?;

        let active_talkback_routes = self
            .routes
            .iter()
            .filter(|state| state.route.active)
            .count();
        let dimmed = self.dim || (self.dim_on_talkback && active_talkback_routes > 0);
        let gain = if dimmed {
            self.level * db_to_linear(MONITOR_DIM_DB)
        } else {
            self.level
        };

        let mut pfl_taps_mixed = 0;
        let mut aux_present = false;
        match self.source {
            MonitorSource::Master => {
                for (out, source) in monitor_out.iter_mut().zip(program) {
                    for (slot, sample) in out.iter_mut().zip(source.iter()) {
                        *slot = *sample * gain;
                    }
                }
            }
            MonitorSource::Pfl => {
                for out in monitor_out.iter_mut() {
                    out.fill(0.0);
                }
                for tap in strips.iter().filter(|tap| self.is_pfl(tap.index)) {
                    pfl_taps_mixed += 1;
                    for (out, source) in monitor_out.iter_mut().zip(tap.planes) {
                        for (slot, sample) in out.iter_mut().zip(source.iter()) {
                            *slot += *sample;
                        }
                    }
                }
                for out in monitor_out.iter_mut() {
                    for slot in out.iter_mut() {
                        *slot *= gain;
                    }
                }
            }
            MonitorSource::Aux(aux) => {
                let tap = auxes.iter().find(|tap| tap.index == aux);
                aux_present = tap.is_some();
                for (channel, out) in monitor_out.iter_mut().enumerate() {
                    match tap {
                        Some(tap) => {
                            for (slot, sample) in out.iter_mut().zip(tap.planes[channel].iter()) {
                                *slot = *sample * gain;
                            }
                        }
                        None => out.fill(0.0),
                    }
                }
            }
        }

        for state in &mut self.routes {
            let feed = &mut state.feed[..samples];
            if state.route.active {
                for (slot, sample) in feed.iter_mut().zip(talkback_mic) {
                    *slot = *sample * state.gain;
                }
            } else {
                feed.fill(0.0);
            }
            state.len = samples;
        }

        Ok(MonitorTelemetry {
            source: self.source,
            samples,
            pfl_taps_mixed,
            aux_present,
            dimmed,
            gain,
            active_talkback_routes,
            device: self.device,
        })
    }

    fn route_index(&self, destination: TalkbackDestination) -> Option<usize> {
        self.routes
            .iter()
            .position(|state| state.route.destination == destination)
    }

    fn validate_planes(&self, planes: &[&[f32]], samples: usize) -> Result<(), MonitorError> {
        if planes.len() != self.channels {
            return Err(MonitorError::PlaneCountMismatch {
                expected: self.channels,
                actual: planes.len(),
            });
        }
        for plane in planes {
            check_len(plane.len(), samples)?;
            validate_finite(plane)?;
        }
        Ok(())
    }

    fn validate_taps(
        &self,
        taps: &[MonitorTap<'_>],
        samples: usize,
        maximum: usize,
        out_of_range: impl Fn(usize, usize) -> MonitorError,
    ) -> Result<(), MonitorError> {
        let mut seen = 0_u64;
        for tap in taps {
            if tap.index >= maximum {
                return Err(out_of_range(tap.index, maximum));
            }
            let bit = 1_u64 << tap.index;
            if seen & bit != 0 {
                return Err(MonitorError::DuplicateTap { index: tap.index });
            }
            seen |= bit;
            self.validate_planes(tap.planes, samples)?;
        }
        Ok(())
    }
}

fn strip_bit(strip: usize) -> Result<u64, MonitorError> {
    if strip >= MAX_MONITOR_STRIPS {
        return Err(MonitorError::StripIndexOutOfRange {
            strip,
            maximum: MAX_MONITOR_STRIPS,
        });
    }
    Ok(1_u64 << strip)
}

fn check_len(actual: usize, expected: usize) -> Result<(), MonitorError> {
    if actual == expected {
        Ok(())
    } else {
        Err(MonitorError::PlaneLengthMismatch { expected, actual })
    }
}

fn validate_finite(samples: &[f32]) -> Result<(), MonitorError> {
    match samples.iter().position(|sample| !sample.is_finite()) {
        Some(sample) => Err(MonitorError::NonFiniteSample { sample }),
        None => Ok(()),
    }
}

fn talkback_gain(db: f32) -> Result<f32, MonitorError> {
    if !db.is_finite() || !(MIN_GAIN_DB..=MAX_TALKBACK_GAIN_DB).contains(&db) {
        return Err(MonitorError::InvalidTalkbackGainDb(db));
    }
    Ok(db_to_linear(db))
}

#[allow(clippy::cast_possible_truncation)]
fn db_to_linear(db: f32) -> f32 {
    10.0_f64.powf(f64::from(db) / 20.0) as f32
}
