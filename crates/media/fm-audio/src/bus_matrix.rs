//! Deterministic Master plus seven aux (A–G) bus matrix with mix-minus.
//!
//! [`BusMatrix`] is a bounded reference slice of the eight-bus mixer:
//!
//! - every strip owns one [`BusSend`] per [`MixBus`] with an enable flag, a
//!   send level, and a pre- or post-fader [`SendTap`];
//! - strip mute silences every send of that strip, pre-fader sends included,
//!   while the strip fader only scales post-fader sends;
//! - every bus has a name, a fader, a mute, and optional [`StripDsp`]
//!   processing applied before the bus fader. A muted bus still advances its
//!   processing state so unmuting is continuous;
//! - [`BusMatrix::render`] writes every bus into caller-owned planes, updates
//!   per-bus peak/RMS meters in fixed storage, and renders every configured
//!   [`BusDestination`] without allocating;
//! - a mix-minus destination re-sums its bus from the per-strip contributions
//!   with the excluded strip skipped, in the same strip order as the bus sum.
//!   No subtraction is performed, so the feed is bit-identical to rendering
//!   the bus with that strip's send disabled. When the bus has processing, the
//!   mix-minus feed runs through its own processing state cloned from the bus
//!   configuration (reset when the destination is added or the bus processing
//!   is replaced).
//!
//! Destinations are opaque typed IDs ([`DestinationId`]); no device, network,
//! or recording I/O happens here. All strips and buses share one channel
//! count; channel mapping is out of scope. Control-plane methods validate
//! completely before changing state, and render validates every input and
//! output plane before writing anything.

use std::fmt;

use fm_types::{InputId, SampleRate};

use crate::{ChannelMeter, Gain, MAX_SAMPLES_PER_BLOCK, StripDsp};

/// Number of buses: Master plus aux A–G.
pub const BUS_COUNT: usize = 8;
/// Maximum channels per bus.
pub const MAX_BUS_CHANNELS: usize = 8;
/// Maximum strips feeding one matrix.
pub const MAX_BUS_STRIPS: usize = 64;
/// Maximum configured destinations.
pub const MAX_BUS_DESTINATIONS: usize = 32;
/// Maximum bus-name length in UTF-8 bytes.
pub const MAX_BUS_NAME_BYTES: usize = 32;
/// Largest accepted absolute input sample (+60 dBFS). With bounded gains,
/// strips, and processing this keeps every intermediate sum finite.
pub const MAX_BUS_INPUT_MAGNITUDE: f32 = 1024.0;

/// One of the eight mix buses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MixBus {
    Master,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
}

impl MixBus {
    /// Every bus in render order.
    pub const ALL: [Self; BUS_COUNT] = [
        Self::Master,
        Self::A,
        Self::B,
        Self::C,
        Self::D,
        Self::E,
        Self::F,
        Self::G,
    ];

    /// Zero-based index; Master is 0 and aux A–G are 1–7.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Bus at `index`, if any.
    #[must_use]
    pub const fn from_index(index: usize) -> Option<Self> {
        if index < BUS_COUNT {
            Some(Self::ALL[index])
        } else {
            None
        }
    }

    /// Fixed short label: `Master` or `A`–`G`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Master => "Master",
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::E => "E",
            Self::F => "F",
            Self::G => "G",
        }
    }
}

/// Where a send taps the strip (AU-002): before or after its fader.
pub use fm_types::audio_bus::SendTap;

/// One strip-to-bus send.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BusSend {
    pub enabled: bool,
    pub level: Gain,
    pub tap: SendTap,
}

impl BusSend {
    /// An enabled unity post-fader send.
    pub const UNITY: Self = Self {
        enabled: true,
        level: Gain::UNITY,
        tap: SendTap::PostFader,
    };
    /// A disabled unity post-fader send.
    pub const DISABLED: Self = Self {
        enabled: false,
        level: Gain::UNITY,
        tap: SendTap::PostFader,
    };
}

/// Opaque typed destination identity. No I/O is performed for any kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DestinationId {
    Device(u32),
    Network(u32),
    Record(u32),
}

/// One routed bus destination, optionally a mix-minus of one strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusDestination {
    pub id: DestinationId,
    pub bus: MixBus,
    /// Strip whose contribution is excluded from this feed.
    pub mix_minus: Option<InputId>,
}

/// One borrowed strip input for [`BusMatrix::render`].
#[derive(Clone, Copy, Debug)]
pub struct BusStripInput<'a> {
    pub strip: InputId,
    /// Planes in bus channel order.
    pub planes: &'a [&'a [f32]],
}

/// Summary of the most recent render.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BusMatrixTelemetry {
    pub samples: usize,
    /// Configured strips that supplied input this block.
    pub strips_present: usize,
    pub destinations_rendered: usize,
    pub mix_minus_rendered: usize,
}

/// Errors returned by [`BusMatrix`].
#[derive(Clone, Debug, PartialEq)]
pub enum BusMatrixError {
    ChannelCountOutOfRange {
        actual: usize,
        maximum: usize,
    },
    StripLimit {
        maximum: usize,
    },
    DuplicateStrip(InputId),
    UnknownStrip(InputId),
    StripInUse {
        strip: InputId,
        destination: DestinationId,
    },
    EmptyBusName,
    BusNameTooLong {
        bytes: usize,
        maximum: usize,
    },
    BusNameControlCharacter,
    DuplicateBusName {
        existing: MixBus,
    },
    DestinationLimit {
        maximum: usize,
    },
    DuplicateDestination(DestinationId),
    UnknownDestination(DestinationId),
    ProcessingChannelMismatch {
        expected: usize,
        actual: usize,
    },
    ProcessingSampleRateMismatch {
        expected_hz: u32,
        actual_hz: f32,
    },
    BlockSamplesOutOfRange {
        actual: usize,
        maximum: usize,
    },
    PlaneCountMismatch {
        expected: usize,
        actual: usize,
    },
    PlaneLengthMismatch {
        expected: usize,
        actual: usize,
    },
    DuplicateStripInput(InputId),
    SampleOutOfRange {
        strip: InputId,
        channel: usize,
        sample: usize,
    },
}

impl fmt::Display for BusMatrixError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelCountOutOfRange { actual, maximum } => write!(
                formatter,
                "bus channel count {actual} is outside 1..={maximum}"
            ),
            Self::StripLimit { maximum } => {
                write!(formatter, "bus matrix strip limit of {maximum} reached")
            }
            Self::DuplicateStrip(strip) => write!(formatter, "strip {strip} already exists"),
            Self::UnknownStrip(strip) => write!(formatter, "strip {strip} is not configured"),
            Self::StripInUse { strip, destination } => write!(
                formatter,
                "strip {strip} is the mix-minus source of destination {destination:?}"
            ),
            Self::EmptyBusName => formatter.write_str("bus name is empty"),
            Self::BusNameTooLong { bytes, maximum } => write!(
                formatter,
                "bus name is {bytes} bytes; the limit is {maximum}"
            ),
            Self::BusNameControlCharacter => {
                formatter.write_str("bus name contains a control character")
            }
            Self::DuplicateBusName { existing } => {
                write!(formatter, "bus name is already used by bus {existing:?}")
            }
            Self::DestinationLimit { maximum } => {
                write!(formatter, "bus destination limit of {maximum} reached")
            }
            Self::DuplicateDestination(id) => {
                write!(formatter, "destination {id:?} already exists")
            }
            Self::UnknownDestination(id) => write!(formatter, "destination {id:?} is unknown"),
            Self::ProcessingChannelMismatch { expected, actual } => write!(
                formatter,
                "bus processing has {actual} channels; expected {expected}"
            ),
            Self::ProcessingSampleRateMismatch {
                expected_hz,
                actual_hz,
            } => write!(
                formatter,
                "bus processing runs at {actual_hz} Hz; expected {expected_hz} Hz"
            ),
            Self::BlockSamplesOutOfRange { actual, maximum } => write!(
                formatter,
                "bus block size {actual} is outside 1..={maximum}"
            ),
            Self::PlaneCountMismatch { expected, actual } => write!(
                formatter,
                "bus matrix expected {expected} planes but received {actual}"
            ),
            Self::PlaneLengthMismatch { expected, actual } => write!(
                formatter,
                "bus plane has {actual} samples; expected {expected}"
            ),
            Self::DuplicateStripInput(strip) => {
                write!(formatter, "strip {strip} was supplied more than once")
            }
            Self::SampleOutOfRange {
                strip,
                channel,
                sample,
            } => write!(
                formatter,
                "strip {strip} channel {channel} sample {sample} is non-finite or exceeds \
                 {MAX_BUS_INPUT_MAGNITUDE}"
            ),
        }
    }
}

impl std::error::Error for BusMatrixError {}

#[derive(Clone, Debug)]
struct StripState {
    id: InputId,
    fader: Gain,
    muted: bool,
    sends: [BusSend; BUS_COUNT],
}

impl StripState {
    /// Linear coefficient applied to this strip's samples on `bus`, or `None`
    /// when the strip does not contribute.
    fn coefficient(&self, bus: MixBus) -> Option<f32> {
        let send = self.sends[bus.index()];
        if self.muted || !send.enabled {
            return None;
        }
        Some(match send.tap {
            SendTap::PreFader => send.level.linear(),
            SendTap::PostFader => send.level.linear() * self.fader.linear(),
        })
    }
}

#[derive(Clone, Debug)]
struct BusState {
    name: String,
    fader: Gain,
    muted: bool,
    processing: Option<StripDsp>,
}

#[derive(Clone, Debug)]
struct DestinationState {
    destination: BusDestination,
    /// Independent processing state for mix-minus feeds on processed buses.
    processing: Option<StripDsp>,
}

/// Bounded deterministic Master + aux A–G matrix. See the module docs.
#[derive(Clone, Debug)]
pub struct BusMatrix {
    sample_rate: SampleRate,
    channels: usize,
    strips: Vec<StripState>,
    buses: [BusState; BUS_COUNT],
    destinations: Vec<DestinationState>,
    meters: [[ChannelMeter; MAX_BUS_CHANNELS]; BUS_COUNT],
}

impl BusMatrix {
    /// Creates a matrix with no strips or destinations; every bus is at
    /// unity, unmuted, unprocessed, and named by [`MixBus::label`].
    ///
    /// Strip, destination, and bus-name storage is reserved here so later
    /// control changes within the limits do not grow the containers.
    ///
    /// # Errors
    ///
    /// Returns [`BusMatrixError::ChannelCountOutOfRange`] unless `channels`
    /// is in `1..=MAX_BUS_CHANNELS`.
    pub fn new(sample_rate: SampleRate, channels: usize) -> Result<Self, BusMatrixError> {
        if channels == 0 || channels > MAX_BUS_CHANNELS {
            return Err(BusMatrixError::ChannelCountOutOfRange {
                actual: channels,
                maximum: MAX_BUS_CHANNELS,
            });
        }
        let buses = MixBus::ALL.map(|bus| {
            let mut name = String::with_capacity(MAX_BUS_NAME_BYTES);
            name.push_str(bus.label());
            BusState {
                name,
                fader: Gain::UNITY,
                muted: false,
                processing: None,
            }
        });
        Ok(Self {
            sample_rate,
            channels,
            strips: Vec::with_capacity(MAX_BUS_STRIPS),
            buses,
            destinations: Vec::with_capacity(MAX_BUS_DESTINATIONS),
            meters: [[ChannelMeter::default(); MAX_BUS_CHANNELS]; BUS_COUNT],
        })
    }

    #[must_use]
    pub const fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    #[must_use]
    pub const fn channels(&self) -> usize {
        self.channels
    }

    // ---- strips -------------------------------------------------------------

    /// Adds a strip at unity fader, unmuted, with an enabled unity post-fader
    /// Master send and disabled aux sends. Strips sum in insertion order.
    ///
    /// # Errors
    ///
    /// Returns an error for a duplicate strip or when [`MAX_BUS_STRIPS`]
    /// strips already exist.
    pub fn add_strip(&mut self, strip: InputId) -> Result<(), BusMatrixError> {
        if self.strip_index(strip).is_some() {
            return Err(BusMatrixError::DuplicateStrip(strip));
        }
        if self.strips.len() >= MAX_BUS_STRIPS {
            return Err(BusMatrixError::StripLimit {
                maximum: MAX_BUS_STRIPS,
            });
        }
        let mut sends = [BusSend::DISABLED; BUS_COUNT];
        sends[MixBus::Master.index()] = BusSend::UNITY;
        self.strips.push(StripState {
            id: strip,
            fader: Gain::UNITY,
            muted: false,
            sends,
        });
        Ok(())
    }

    /// Removes a strip.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown strip or one that is the mix-minus
    /// source of a destination.
    pub fn remove_strip(&mut self, strip: InputId) -> Result<(), BusMatrixError> {
        let index = self.require_strip(strip)?;
        if let Some(state) = self
            .destinations
            .iter()
            .find(|state| state.destination.mix_minus == Some(strip))
        {
            return Err(BusMatrixError::StripInUse {
                strip,
                destination: state.destination.id,
            });
        }
        self.strips.remove(index);
        Ok(())
    }

    /// Configured strips in summing order.
    pub fn strips(&self) -> impl Iterator<Item = InputId> + '_ {
        self.strips.iter().map(|state| state.id)
    }

    /// Sets a strip fader.
    ///
    /// # Errors
    ///
    /// Returns [`BusMatrixError::UnknownStrip`] when absent.
    pub fn set_strip_fader(&mut self, strip: InputId, fader: Gain) -> Result<(), BusMatrixError> {
        let index = self.require_strip(strip)?;
        self.strips[index].fader = fader;
        Ok(())
    }

    /// Mutes or unmutes a strip (all of its sends).
    ///
    /// # Errors
    ///
    /// Returns [`BusMatrixError::UnknownStrip`] when absent.
    pub fn set_strip_mute(&mut self, strip: InputId, muted: bool) -> Result<(), BusMatrixError> {
        let index = self.require_strip(strip)?;
        self.strips[index].muted = muted;
        Ok(())
    }

    #[must_use]
    pub fn strip_fader(&self, strip: InputId) -> Option<Gain> {
        self.strip_index(strip)
            .map(|index| self.strips[index].fader)
    }

    #[must_use]
    pub fn strip_muted(&self, strip: InputId) -> Option<bool> {
        self.strip_index(strip)
            .map(|index| self.strips[index].muted)
    }

    /// Replaces one strip-to-bus send.
    ///
    /// # Errors
    ///
    /// Returns [`BusMatrixError::UnknownStrip`] when absent.
    pub fn set_send(
        &mut self,
        strip: InputId,
        bus: MixBus,
        send: BusSend,
    ) -> Result<(), BusMatrixError> {
        let index = self.require_strip(strip)?;
        self.strips[index].sends[bus.index()] = send;
        Ok(())
    }

    #[must_use]
    pub fn send(&self, strip: InputId, bus: MixBus) -> Option<BusSend> {
        self.strip_index(strip)
            .map(|index| self.strips[index].sends[bus.index()])
    }

    // ---- buses --------------------------------------------------------------

    /// Renames a bus. Names are 1..=[`MAX_BUS_NAME_BYTES`] bytes, contain no
    /// control characters, and are unique across buses ignoring ASCII case.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or duplicate name.
    pub fn set_bus_name(&mut self, bus: MixBus, name: &str) -> Result<(), BusMatrixError> {
        if name.trim().is_empty() {
            return Err(BusMatrixError::EmptyBusName);
        }
        if name.len() > MAX_BUS_NAME_BYTES {
            return Err(BusMatrixError::BusNameTooLong {
                bytes: name.len(),
                maximum: MAX_BUS_NAME_BYTES,
            });
        }
        if name.chars().any(char::is_control) {
            return Err(BusMatrixError::BusNameControlCharacter);
        }
        if let Some(existing) = self.bus_by_name(name)
            && existing != bus
        {
            return Err(BusMatrixError::DuplicateBusName { existing });
        }
        let stored = &mut self.buses[bus.index()].name;
        stored.clear();
        stored.push_str(name);
        Ok(())
    }

    #[must_use]
    pub fn bus_name(&self, bus: MixBus) -> &str {
        &self.buses[bus.index()].name
    }

    /// Resolves a bus by name, ignoring ASCII case.
    #[must_use]
    pub fn bus_by_name(&self, name: &str) -> Option<MixBus> {
        MixBus::ALL
            .into_iter()
            .find(|bus| self.buses[bus.index()].name.eq_ignore_ascii_case(name))
    }

    pub fn set_bus_fader(&mut self, bus: MixBus, fader: Gain) {
        self.buses[bus.index()].fader = fader;
    }

    pub fn set_bus_mute(&mut self, bus: MixBus, muted: bool) {
        self.buses[bus.index()].muted = muted;
    }

    #[must_use]
    pub fn bus_fader(&self, bus: MixBus) -> Gain {
        self.buses[bus.index()].fader
    }

    #[must_use]
    pub fn bus_muted(&self, bus: MixBus) -> bool {
        self.buses[bus.index()].muted
    }

    /// Installs (or clears) bus processing. The installed chain is reset, and
    /// every mix-minus destination on this bus receives a fresh reset clone.
    ///
    /// # Errors
    ///
    /// Returns an error when the chain's channel count or sample rate differs
    /// from the matrix.
    pub fn set_bus_processing(
        &mut self,
        bus: MixBus,
        processing: Option<StripDsp>,
    ) -> Result<(), BusMatrixError> {
        let processing = match processing {
            Some(mut dsp) => {
                self.validate_processing(&dsp)?;
                dsp.reset();
                Some(dsp)
            }
            None => None,
        };
        for state in &mut self.destinations {
            if state.destination.bus == bus && state.destination.mix_minus.is_some() {
                state.processing.clone_from(&processing);
            }
        }
        self.buses[bus.index()].processing = processing;
        Ok(())
    }

    #[must_use]
    pub fn bus_processing(&self, bus: MixBus) -> Option<&StripDsp> {
        self.buses[bus.index()].processing.as_ref()
    }

    /// Per-channel peak/RMS of each bus's final output in the last render.
    #[must_use]
    pub fn bus_meters(&self, bus: MixBus) -> &[ChannelMeter] {
        &self.meters[bus.index()][..self.channels]
    }

    // ---- destinations -------------------------------------------------------

    /// Adds a destination. A mix-minus strip must be configured.
    ///
    /// # Errors
    ///
    /// Returns an error for a duplicate ID, an unknown mix-minus strip, or
    /// when [`MAX_BUS_DESTINATIONS`] destinations already exist.
    pub fn add_destination(&mut self, destination: BusDestination) -> Result<(), BusMatrixError> {
        if self.destination_index(destination.id).is_some() {
            return Err(BusMatrixError::DuplicateDestination(destination.id));
        }
        if let Some(strip) = destination.mix_minus {
            self.require_strip(strip)?;
        }
        if self.destinations.len() >= MAX_BUS_DESTINATIONS {
            return Err(BusMatrixError::DestinationLimit {
                maximum: MAX_BUS_DESTINATIONS,
            });
        }
        let processing = match destination.mix_minus {
            Some(_) => self.buses[destination.bus.index()]
                .processing
                .as_ref()
                .map(|dsp| {
                    let mut dsp = dsp.clone();
                    dsp.reset();
                    dsp
                }),
            None => None,
        };
        self.destinations.push(DestinationState {
            destination,
            processing,
        });
        Ok(())
    }

    /// Removes a destination.
    ///
    /// # Errors
    ///
    /// Returns [`BusMatrixError::UnknownDestination`] when absent.
    pub fn remove_destination(
        &mut self,
        id: DestinationId,
    ) -> Result<BusDestination, BusMatrixError> {
        let index = self
            .destination_index(id)
            .ok_or(BusMatrixError::UnknownDestination(id))?;
        Ok(self.destinations.remove(index).destination)
    }

    /// Destinations in output order.
    pub fn destinations(&self) -> impl Iterator<Item = BusDestination> + '_ {
        self.destinations.iter().map(|state| state.destination)
    }

    // ---- render -------------------------------------------------------------

    /// Renders one block.
    ///
    /// `bus_out` holds `BUS_COUNT * channels` planes, bus-major in
    /// [`MixBus::ALL`] order; `destination_out` holds
    /// `destinations * channels` planes, destination-major in
    /// [`Self::destinations`] order. All planes share one length, the block
    /// size, in `1..=MAX_SAMPLES_PER_BLOCK`. Configured strips without an
    /// input contribute silence.
    ///
    /// # Errors
    ///
    /// Returns an error for plane-count or length mismatches, unknown or
    /// duplicate strip inputs, or non-finite or out-of-range samples.
    /// Validation completes before any output, meter, or processing state
    /// changes.
    pub fn render(
        &mut self,
        inputs: &[BusStripInput<'_>],
        bus_out: &mut [&mut [f32]],
        destination_out: &mut [&mut [f32]],
    ) -> Result<BusMatrixTelemetry, BusMatrixError> {
        let samples = self.validate_render(inputs, bus_out, destination_out)?;
        let channels = self.channels;

        for bus in MixBus::ALL {
            let state = &mut self.buses[bus.index()];
            for channel in 0..channels {
                let out = &mut *bus_out[bus.index() * channels + channel];
                sum_bus(&self.strips, inputs, bus, channel, None, out);
                finish_bus(
                    state.processing.as_mut(),
                    state.fader,
                    state.muted,
                    channel,
                    out,
                );
                self.meters[bus.index()][channel] = measure(out);
            }
        }

        let mut mix_minus_rendered = 0;
        for (index, state) in self.destinations.iter_mut().enumerate() {
            let bus = state.destination.bus;
            let bus_state = &self.buses[bus.index()];
            if state.destination.mix_minus.is_some() {
                mix_minus_rendered += 1;
            }
            for channel in 0..channels {
                let out = &mut *destination_out[index * channels + channel];
                match state.destination.mix_minus {
                    None => out.copy_from_slice(bus_out[bus.index() * channels + channel]),
                    Some(excluded) => {
                        sum_bus(&self.strips, inputs, bus, channel, Some(excluded), out);
                        finish_bus(
                            state.processing.as_mut(),
                            bus_state.fader,
                            bus_state.muted,
                            channel,
                            out,
                        );
                    }
                }
            }
        }

        let strips_present = self
            .strips
            .iter()
            .filter(|strip| inputs.iter().any(|input| input.strip == strip.id))
            .count();
        Ok(BusMatrixTelemetry {
            samples,
            strips_present,
            destinations_rendered: self.destinations.len(),
            mix_minus_rendered,
        })
    }

    fn validate_render(
        &self,
        inputs: &[BusStripInput<'_>],
        bus_out: &[&mut [f32]],
        destination_out: &[&mut [f32]],
    ) -> Result<usize, BusMatrixError> {
        let expected = BUS_COUNT * self.channels;
        if bus_out.len() != expected {
            return Err(BusMatrixError::PlaneCountMismatch {
                expected,
                actual: bus_out.len(),
            });
        }
        let samples = bus_out[0].len();
        if samples == 0 || samples > MAX_SAMPLES_PER_BLOCK {
            return Err(BusMatrixError::BlockSamplesOutOfRange {
                actual: samples,
                maximum: MAX_SAMPLES_PER_BLOCK,
            });
        }
        for plane in bus_out {
            check_len(plane.len(), samples)?;
        }
        let expected = self.destinations.len() * self.channels;
        if destination_out.len() != expected {
            return Err(BusMatrixError::PlaneCountMismatch {
                expected,
                actual: destination_out.len(),
            });
        }
        for plane in destination_out {
            check_len(plane.len(), samples)?;
        }
        for (position, input) in inputs.iter().enumerate() {
            self.require_strip(input.strip)?;
            if inputs[..position]
                .iter()
                .any(|earlier| earlier.strip == input.strip)
            {
                return Err(BusMatrixError::DuplicateStripInput(input.strip));
            }
            if input.planes.len() != self.channels {
                return Err(BusMatrixError::PlaneCountMismatch {
                    expected: self.channels,
                    actual: input.planes.len(),
                });
            }
            for (channel, plane) in input.planes.iter().enumerate() {
                check_len(plane.len(), samples)?;
                if let Some(sample) = plane
                    .iter()
                    .position(|value| !value.is_finite() || value.abs() > MAX_BUS_INPUT_MAGNITUDE)
                {
                    return Err(BusMatrixError::SampleOutOfRange {
                        strip: input.strip,
                        channel,
                        sample,
                    });
                }
            }
        }
        Ok(samples)
    }

    fn validate_processing(&self, dsp: &StripDsp) -> Result<(), BusMatrixError> {
        if dsp.channels() != self.channels {
            return Err(BusMatrixError::ProcessingChannelMismatch {
                expected: self.channels,
                actual: dsp.channels(),
            });
        }
        let expected_hz = self.sample_rate.hertz();
        if (f64::from(dsp.sample_rate_hz()) - f64::from(expected_hz)).abs() > f64::EPSILON {
            return Err(BusMatrixError::ProcessingSampleRateMismatch {
                expected_hz,
                actual_hz: dsp.sample_rate_hz(),
            });
        }
        Ok(())
    }

    fn strip_index(&self, strip: InputId) -> Option<usize> {
        self.strips.iter().position(|state| state.id == strip)
    }

    fn require_strip(&self, strip: InputId) -> Result<usize, BusMatrixError> {
        self.strip_index(strip)
            .ok_or(BusMatrixError::UnknownStrip(strip))
    }

    fn destination_index(&self, id: DestinationId) -> Option<usize> {
        self.destinations
            .iter()
            .position(|state| state.destination.id == id)
    }
}

/// Sums every contributing strip into `out` in strip order, skipping
/// `excluded`. Identical ordering makes a mix-minus bit-exact.
fn sum_bus(
    strips: &[StripState],
    inputs: &[BusStripInput<'_>],
    bus: MixBus,
    channel: usize,
    excluded: Option<InputId>,
    out: &mut [f32],
) {
    out.fill(0.0);
    for strip in strips {
        if excluded == Some(strip.id) {
            continue;
        }
        let Some(coefficient) = strip.coefficient(bus) else {
            continue;
        };
        let Some(input) = inputs.iter().find(|input| input.strip == strip.id) else {
            continue;
        };
        for (slot, sample) in out.iter_mut().zip(input.planes[channel]) {
            *slot += *sample * coefficient;
        }
    }
}

/// Applies bus processing, then the bus fader or mute, in place.
fn finish_bus(
    mut processing: Option<&mut StripDsp>,
    fader: Gain,
    muted: bool,
    channel: usize,
    out: &mut [f32],
) {
    let gain = if muted { 0.0 } else { fader.linear() };
    for slot in out.iter_mut() {
        let mut value = *slot;
        if let Some(dsp) = processing.as_deref_mut() {
            value = dsp
                .process_sample(channel, value)
                .expect("bus processing channel count is validated on install");
        }
        *slot = value * gain;
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn measure(plane: &[f32]) -> ChannelMeter {
    let mut peak = 0.0_f32;
    let mut squares = 0.0_f64;
    for sample in plane {
        peak = peak.max(sample.abs());
        squares += f64::from(*sample) * f64::from(*sample);
    }
    ChannelMeter {
        peak,
        rms: (squares / plane.len() as f64).sqrt() as f32,
    }
}

fn check_len(actual: usize, expected: usize) -> Result<(), BusMatrixError> {
    if actual == expected {
        Ok(())
    } else {
        Err(BusMatrixError::PlaneLengthMismatch { expected, actual })
    }
}
