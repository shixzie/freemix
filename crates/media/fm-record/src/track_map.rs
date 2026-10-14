//! Per-recorder audio track maps.
//!
//! A [`RecorderTrackMap`] selects channels from the Master bus and aux buses
//! into an ordered list of recorded tracks. Each track is mono, stereo, or
//! multichannel, and every channel reference is validated against a
//! [`BusInventory`] that describes the buses the mixer currently exposes.
//! Routing is a pure copy of interleaved `f32` frames, so the same inputs
//! always produce the same per-track buffers.

use core::fmt;

use fm_types::BusId;

/// Maximum number of tracks a single recorder may map.
pub const MAX_RECORDER_TRACKS: usize = 32;
/// Maximum number of channels a single multichannel track may carry.
pub const MAX_TRACK_CHANNELS: usize = 16;
/// Maximum number of channels across all tracks of one recorder.
pub const MAX_RECORDER_CHANNELS: usize = 64;
/// Maximum number of channels a bus in the inventory may expose.
pub const MAX_BUS_CHANNELS: u16 = 64;
/// Maximum number of buses an inventory may describe.
pub const MAX_INVENTORY_BUSES: usize = 65;
/// Maximum number of UTF-8 bytes in a track name.
pub const MAX_TRACK_NAME_BYTES: usize = 64;
/// Maximum number of frames routed by one call, bounding allocations.
pub const MAX_ROUTE_FRAMES: usize = 1 << 16;

/// Bus a recorder may take audio from.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RecordBus {
    /// The program Master bus.
    Master,
    /// An aux (submix/monitor) bus.
    Aux(BusId),
}

impl fmt::Display for RecordBus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Master => formatter.write_str("master"),
            Self::Aux(id) => write!(formatter, "aux:{id}"),
        }
    }
}

/// One zero-based channel of one bus.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrackSource {
    pub bus: RecordBus,
    pub channel: u16,
}

impl TrackSource {
    #[must_use]
    pub const fn new(bus: RecordBus, channel: u16) -> Self {
        Self { bus, channel }
    }

    #[must_use]
    pub const fn master(channel: u16) -> Self {
        Self::new(RecordBus::Master, channel)
    }
}

/// Channel layout of one recorded track, derived from its source count.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrackLayout {
    Mono,
    Stereo,
    /// Three or more channels.
    Multichannel(u16),
}

impl TrackLayout {
    #[must_use]
    pub const fn channel_count(self) -> u16 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::Multichannel(count) => count,
        }
    }
}

/// An ordered set of bus channels recorded as one track.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RecordTrack {
    name: String,
    sources: Vec<TrackSource>,
}

impl RecordTrack {
    /// Creates a track from explicit channel sources in output order.
    ///
    /// Validation happens when the track is placed in a [`RecorderTrackMap`].
    #[must_use]
    pub fn new(name: impl Into<String>, sources: Vec<TrackSource>) -> Self {
        Self {
            name: name.into(),
            sources,
        }
    }

    #[must_use]
    pub fn mono(name: impl Into<String>, source: TrackSource) -> Self {
        Self::new(name, vec![source])
    }

    #[must_use]
    pub fn stereo(name: impl Into<String>, left: TrackSource, right: TrackSource) -> Self {
        Self::new(name, vec![left, right])
    }

    /// Creates a track carrying the first `channels` channels of `bus`.
    #[must_use]
    pub fn whole_bus(name: impl Into<String>, bus: RecordBus, channels: u16) -> Self {
        Self::new(
            name,
            (0..channels)
                .map(|channel| TrackSource::new(bus, channel))
                .collect(),
        )
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn sources(&self) -> &[TrackSource] {
        &self.sources
    }

    /// Layout derived from the number of sources.
    ///
    /// # Panics
    ///
    /// Never for tracks held by a validated [`RecorderTrackMap`]; an unvalidated
    /// track with more than `u16::MAX` sources saturates.
    #[must_use]
    pub fn layout(&self) -> TrackLayout {
        match self.sources.len() {
            0 | 1 => TrackLayout::Mono,
            2 => TrackLayout::Stereo,
            count => TrackLayout::Multichannel(u16::try_from(count).unwrap_or(u16::MAX)),
        }
    }
}

/// Buses (and their channel counts) available to recorders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BusInventory {
    buses: Vec<(RecordBus, u16)>,
}

impl BusInventory {
    /// Creates an inventory from `(bus, channel_count)` pairs.
    ///
    /// # Errors
    ///
    /// Rejects duplicate buses, zero or excessive channel counts, and more
    /// than [`MAX_INVENTORY_BUSES`] entries.
    pub fn new(buses: Vec<(RecordBus, u16)>) -> Result<Self, TrackMapError> {
        if buses.len() > MAX_INVENTORY_BUSES {
            return Err(TrackMapError::TooManyBuses { count: buses.len() });
        }
        for (index, (bus, channels)) in buses.iter().enumerate() {
            if *channels == 0 || *channels > MAX_BUS_CHANNELS {
                return Err(TrackMapError::InvalidBusChannelCount {
                    bus: *bus,
                    channels: *channels,
                });
            }
            if buses[..index].iter().any(|(other, _)| other == bus) {
                return Err(TrackMapError::DuplicateBus { bus: *bus });
            }
        }
        Ok(Self { buses })
    }

    /// An inventory exposing only a stereo Master bus.
    #[must_use]
    pub fn stereo_master() -> Self {
        Self {
            buses: vec![(RecordBus::Master, 2)],
        }
    }

    #[must_use]
    pub fn channel_count(&self, bus: RecordBus) -> Option<u16> {
        self.buses
            .iter()
            .find(|(candidate, _)| *candidate == bus)
            .map(|(_, channels)| *channels)
    }

    #[must_use]
    pub fn buses(&self) -> &[(RecordBus, u16)] {
        &self.buses
    }
}

/// Validation and routing failures for track maps.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrackMapError {
    NoTracks,
    TooManyTracks {
        count: usize,
    },
    TooManyChannels {
        count: usize,
    },
    TooManyBuses {
        count: usize,
    },
    InvalidBusChannelCount {
        bus: RecordBus,
        channels: u16,
    },
    DuplicateBus {
        bus: RecordBus,
    },
    EmptyTrack {
        track: usize,
    },
    TrackTooWide {
        track: usize,
        channels: usize,
    },
    InvalidTrackName {
        track: usize,
    },
    DuplicateTrackName {
        track: usize,
    },
    UnknownBus {
        track: usize,
        bus: RecordBus,
    },
    ChannelOutOfRange {
        track: usize,
        bus: RecordBus,
        channel: u16,
        bus_channels: u16,
    },
    DuplicateSource {
        track: usize,
        source: TrackSource,
    },
    TooManyFrames {
        frames: usize,
    },
    MissingBusInput {
        bus: RecordBus,
    },
    DuplicateBusInput {
        bus: RecordBus,
    },
    UnexpectedBusInput {
        bus: RecordBus,
    },
    BusInputLength {
        bus: RecordBus,
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for TrackMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTracks => formatter.write_str("recorder track map has no tracks"),
            Self::TooManyTracks { count } => write!(
                formatter,
                "recorder track map has {count} tracks; at most {MAX_RECORDER_TRACKS} are allowed"
            ),
            Self::TooManyChannels { count } => write!(
                formatter,
                "recorder track map has {count} channels; at most {MAX_RECORDER_CHANNELS} are allowed"
            ),
            Self::TooManyBuses { count } => write!(
                formatter,
                "bus inventory has {count} buses; at most {MAX_INVENTORY_BUSES} are allowed"
            ),
            Self::InvalidBusChannelCount { bus, channels } => write!(
                formatter,
                "bus {bus} reports {channels} channels; expected 1..={MAX_BUS_CHANNELS}"
            ),
            Self::DuplicateBus { bus } => write!(formatter, "bus {bus} is listed twice"),
            Self::EmptyTrack { track } => write!(formatter, "track {track} has no channels"),
            Self::TrackTooWide { track, channels } => write!(
                formatter,
                "track {track} has {channels} channels; at most {MAX_TRACK_CHANNELS} are allowed"
            ),
            Self::InvalidTrackName { track } => write!(
                formatter,
                "track {track} name must be 1..={MAX_TRACK_NAME_BYTES} bytes without control characters"
            ),
            Self::DuplicateTrackName { track } => {
                write!(formatter, "track {track} name duplicates an earlier track")
            }
            Self::UnknownBus { track, bus } => {
                write!(formatter, "track {track} references unknown bus {bus}")
            }
            Self::ChannelOutOfRange {
                track,
                bus,
                channel,
                bus_channels,
            } => write!(
                formatter,
                "track {track} references channel {channel} of bus {bus}, which has {bus_channels} channels"
            ),
            Self::DuplicateSource { track, source } => write!(
                formatter,
                "track {track} references channel {} of bus {} twice",
                source.channel, source.bus
            ),
            Self::TooManyFrames { frames } => write!(
                formatter,
                "cannot route {frames} frames at once; at most {MAX_ROUTE_FRAMES} are allowed"
            ),
            Self::MissingBusInput { bus } => write!(formatter, "no audio supplied for bus {bus}"),
            Self::DuplicateBusInput { bus } => {
                write!(formatter, "audio for bus {bus} was supplied twice")
            }
            Self::UnexpectedBusInput { bus } => {
                write!(
                    formatter,
                    "audio supplied for bus {bus}, which is not in the inventory"
                )
            }
            Self::BusInputLength {
                bus,
                expected,
                actual,
            } => write!(
                formatter,
                "bus {bus} supplied {actual} samples; expected {expected}"
            ),
        }
    }
}

impl std::error::Error for TrackMapError {}

/// Validated, ordered mapping of bus channels into recorder tracks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecorderTrackMap {
    tracks: Vec<RecordTrack>,
    inventory: BusInventory,
}

impl RecorderTrackMap {
    /// Validates `tracks` against `inventory`.
    ///
    /// # Errors
    ///
    /// Returns the first [`TrackMapError`] found in track order: empty or
    /// oversized maps, empty or oversized tracks, invalid or duplicate names,
    /// unknown buses, out-of-range channels, and channels repeated within a
    /// track. The same bus channel may appear in different tracks.
    pub fn new(tracks: Vec<RecordTrack>, inventory: BusInventory) -> Result<Self, TrackMapError> {
        if tracks.is_empty() {
            return Err(TrackMapError::NoTracks);
        }
        if tracks.len() > MAX_RECORDER_TRACKS {
            return Err(TrackMapError::TooManyTracks {
                count: tracks.len(),
            });
        }
        let mut total = 0_usize;
        for (index, track) in tracks.iter().enumerate() {
            if !valid_track_name(&track.name) {
                return Err(TrackMapError::InvalidTrackName { track: index });
            }
            if tracks[..index].iter().any(|other| other.name == track.name) {
                return Err(TrackMapError::DuplicateTrackName { track: index });
            }
            let width = track.sources.len();
            if width == 0 {
                return Err(TrackMapError::EmptyTrack { track: index });
            }
            if width > MAX_TRACK_CHANNELS {
                return Err(TrackMapError::TrackTooWide {
                    track: index,
                    channels: width,
                });
            }
            for (position, source) in track.sources.iter().enumerate() {
                let Some(bus_channels) = inventory.channel_count(source.bus) else {
                    return Err(TrackMapError::UnknownBus {
                        track: index,
                        bus: source.bus,
                    });
                };
                if source.channel >= bus_channels {
                    return Err(TrackMapError::ChannelOutOfRange {
                        track: index,
                        bus: source.bus,
                        channel: source.channel,
                        bus_channels,
                    });
                }
                if track.sources[..position].contains(source) {
                    return Err(TrackMapError::DuplicateSource {
                        track: index,
                        source: *source,
                    });
                }
            }
            total += width;
        }
        if total > MAX_RECORDER_CHANNELS {
            return Err(TrackMapError::TooManyChannels { count: total });
        }
        Ok(Self { tracks, inventory })
    }

    /// The conventional default: one stereo track from Master channels 0/1.
    #[must_use]
    pub fn master_stereo() -> Self {
        Self {
            tracks: vec![RecordTrack::stereo(
                "Master",
                TrackSource::master(0),
                TrackSource::master(1),
            )],
            inventory: BusInventory::stereo_master(),
        }
    }

    #[must_use]
    pub fn tracks(&self) -> &[RecordTrack] {
        &self.tracks
    }

    #[must_use]
    pub fn inventory(&self) -> &BusInventory {
        &self.inventory
    }

    /// Total channels across all tracks (the width of an interleaved file).
    ///
    /// # Panics
    ///
    /// Never: validation bounds the total by [`MAX_RECORDER_CHANNELS`].
    #[must_use]
    pub fn total_channels(&self) -> u16 {
        let total: usize = self.tracks.iter().map(|track| track.sources.len()).sum();
        u16::try_from(total).expect("validated channel total fits u16")
    }

    /// All sources in interleaved-file channel order.
    #[must_use]
    pub fn interleaved_sources(&self) -> Vec<TrackSource> {
        self.tracks
            .iter()
            .flat_map(|track| track.sources.iter().copied())
            .collect()
    }

    /// Routes interleaved bus frames into one interleaved buffer per track.
    ///
    /// `buses` supplies `frames * channel_count` interleaved samples for every
    /// bus in the inventory that the map references; unreferenced inventory
    /// buses may be omitted.
    ///
    /// # Errors
    ///
    /// Rejects more than [`MAX_ROUTE_FRAMES`] frames, missing, duplicate, or
    /// unknown bus inputs, and inputs whose length does not match.
    pub fn route(
        &self,
        frames: usize,
        buses: &[(RecordBus, &[f32])],
    ) -> Result<Vec<Vec<f32>>, TrackMapError> {
        let inputs = self.resolve_inputs(frames, buses)?;
        Ok(self
            .tracks
            .iter()
            .map(|track| {
                let width = track.sources.len();
                let mut out = Vec::with_capacity(frames * width);
                for frame in 0..frames {
                    for source in &track.sources {
                        out.push(sample_at(&inputs, *source, frame));
                    }
                }
                out
            })
            .collect())
    }

    /// Routes interleaved bus frames into one buffer carrying every track's
    /// channels in [`Self::interleaved_sources`] order.
    ///
    /// # Errors
    ///
    /// Same as [`Self::route`].
    pub fn route_interleaved(
        &self,
        frames: usize,
        buses: &[(RecordBus, &[f32])],
    ) -> Result<Vec<f32>, TrackMapError> {
        let inputs = self.resolve_inputs(frames, buses)?;
        let sources = self.interleaved_sources();
        let mut out = Vec::with_capacity(frames * sources.len());
        for frame in 0..frames {
            for source in &sources {
                out.push(sample_at(&inputs, *source, frame));
            }
        }
        Ok(out)
    }

    fn resolve_inputs<'a>(
        &self,
        frames: usize,
        buses: &[(RecordBus, &'a [f32])],
    ) -> Result<Vec<ResolvedInput<'a>>, TrackMapError> {
        if frames > MAX_ROUTE_FRAMES {
            return Err(TrackMapError::TooManyFrames { frames });
        }
        let mut resolved: Vec<ResolvedInput<'a>> = Vec::with_capacity(buses.len());
        for (bus, samples) in buses {
            let Some(channels) = self.inventory.channel_count(*bus) else {
                return Err(TrackMapError::UnexpectedBusInput { bus: *bus });
            };
            if resolved.iter().any(|input| input.bus == *bus) {
                return Err(TrackMapError::DuplicateBusInput { bus: *bus });
            }
            let expected = frames * usize::from(channels);
            if samples.len() != expected {
                return Err(TrackMapError::BusInputLength {
                    bus: *bus,
                    expected,
                    actual: samples.len(),
                });
            }
            resolved.push(ResolvedInput {
                bus: *bus,
                channels: usize::from(channels),
                samples,
            });
        }
        for track in &self.tracks {
            for source in &track.sources {
                if !resolved.iter().any(|input| input.bus == source.bus) {
                    return Err(TrackMapError::MissingBusInput { bus: source.bus });
                }
            }
        }
        Ok(resolved)
    }
}

struct ResolvedInput<'a> {
    bus: RecordBus,
    channels: usize,
    samples: &'a [f32],
}

fn sample_at(inputs: &[ResolvedInput<'_>], source: TrackSource, frame: usize) -> f32 {
    inputs
        .iter()
        .find(|input| input.bus == source.bus)
        .map_or(0.0, |input| {
            input.samples[frame * input.channels + usize::from(source.channel)]
        })
}

fn valid_track_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TRACK_NAME_BYTES
        && name.trim() == name
        && !name.chars().any(char::is_control)
}

/// Deterministic, filesystem-safe file name for a split track file:
/// `{stem}_{NN}_{sanitized-name}.wav` with a one-based two-digit index.
#[must_use]
pub fn split_track_file_name(stem: &str, track_index: usize, track_name: &str) -> String {
    let sanitized: String = track_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    format!("{stem}_{:02}_{sanitized}.wav", track_index + 1)
}
