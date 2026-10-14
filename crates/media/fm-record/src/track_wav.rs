//! Writes a [`RecorderTrackMap`] to WAV files, either as one interleaved
//! multichannel file or split into one file per track.

use core::fmt;

use crate::{
    track_map::{RecordBus, RecorderTrackMap, TrackMapError},
    wav::{
        BextMetadata, FinalizedWav, LargeFilePolicy, WavError, WavSampleFormat, WavSink, WavSpec,
        WavWriter,
    },
};

/// How a track map is laid out on disk.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrackFileMode {
    /// One file carrying every track's channels in map order.
    Interleaved,
    /// One file per track (split WAV).
    SplitPerTrack,
}

/// Shared file settings for every file a [`TrackWavRecorder`] writes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackWavOptions {
    pub sample_rate: u32,
    pub format: WavSampleFormat,
    pub large_file: LargeFilePolicy,
    /// Copied into every file; in split mode the description is suffixed with
    /// ` track=<name>` when it fits in 256 bytes.
    pub bext: Option<BextMetadata>,
    pub buffer_frames: u32,
}

/// Failures of [`TrackWavRecorder`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrackWavError {
    Map(TrackMapError),
    SinkCount {
        expected: usize,
        actual: usize,
    },
    Wav {
        file: usize,
        error: WavError,
    },
    /// A previous write failed part-way, so files may disagree in length.
    Poisoned,
}

impl fmt::Display for TrackWavError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Map(error) => error.fmt(formatter),
            Self::SinkCount { expected, actual } => {
                write!(formatter, "expected {expected} WAV sinks, got {actual}")
            }
            Self::Wav { file, error } => write!(formatter, "WAV file {file}: {error}"),
            Self::Poisoned => formatter.write_str("track recorder failed earlier and is closed"),
        }
    }
}

impl std::error::Error for TrackWavError {}

impl From<TrackMapError> for TrackWavError {
    fn from(error: TrackMapError) -> Self {
        Self::Map(error)
    }
}

/// Routes bus audio through a track map into one or more WAV writers.
#[derive(Debug)]
pub struct TrackWavRecorder {
    map: RecorderTrackMap,
    mode: TrackFileMode,
    writers: Vec<WavWriter>,
    poisoned: bool,
}

impl TrackWavRecorder {
    /// Number of sinks [`Self::create`] expects.
    #[must_use]
    pub fn file_count(map: &RecorderTrackMap, mode: TrackFileMode) -> usize {
        match mode {
            TrackFileMode::Interleaved => 1,
            TrackFileMode::SplitPerTrack => map.tracks().len(),
        }
    }

    /// The per-file specs [`Self::create`] will use, in sink order.
    #[must_use]
    pub fn file_specs(
        map: &RecorderTrackMap,
        mode: TrackFileMode,
        options: &TrackWavOptions,
    ) -> Vec<WavSpec> {
        let spec = |channels: u16, bext: Option<BextMetadata>| WavSpec {
            channels,
            sample_rate: options.sample_rate,
            format: options.format,
            channel_mask: 0,
            large_file: options.large_file,
            bext,
        };
        match mode {
            TrackFileMode::Interleaved => vec![spec(map.total_channels(), options.bext.clone())],
            TrackFileMode::SplitPerTrack => map
                .tracks()
                .iter()
                .map(|track| {
                    let bext = options.bext.clone().map(|mut bext| {
                        let suffix = if bext.description.is_empty() {
                            format!("track={}", track.name())
                        } else {
                            format!(" track={}", track.name())
                        };
                        if bext.description.len() + suffix.len() <= 256
                            && suffix.bytes().all(|byte| (0x20..=0x7E).contains(&byte))
                        {
                            bext.description.push_str(&suffix);
                        }
                        bext
                    });
                    spec(track.layout().channel_count(), bext)
                })
                .collect(),
        }
    }

    /// Creates one writer per sink and writes every header.
    ///
    /// # Errors
    ///
    /// Returns [`TrackWavError::SinkCount`] when `sinks` does not match
    /// [`Self::file_count`], and the first writer creation error.
    pub fn create(
        map: RecorderTrackMap,
        mode: TrackFileMode,
        options: &TrackWavOptions,
        sinks: Vec<Box<dyn WavSink>>,
    ) -> Result<Self, TrackWavError> {
        let expected = Self::file_count(&map, mode);
        if sinks.len() != expected {
            return Err(TrackWavError::SinkCount {
                expected,
                actual: sinks.len(),
            });
        }
        let specs = Self::file_specs(&map, mode, options);
        let mut writers = Vec::with_capacity(expected);
        for (file, (sink, spec)) in sinks.into_iter().zip(specs).enumerate() {
            writers.push(
                WavWriter::create(sink, spec, options.buffer_frames)
                    .map_err(|error| TrackWavError::Wav { file, error })?,
            );
        }
        Ok(Self {
            map,
            mode,
            writers,
            poisoned: false,
        })
    }

    #[must_use]
    pub fn map(&self) -> &RecorderTrackMap {
        &self.map
    }

    #[must_use]
    pub fn mode(&self) -> TrackFileMode {
        self.mode
    }

    #[must_use]
    pub fn writers(&self) -> &[WavWriter] {
        &self.writers
    }

    /// Frames accepted so far (identical across files).
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.writers.first().map_or(0, WavWriter::frames)
    }

    /// Routes `frames` frames of bus audio and appends them to every file.
    ///
    /// Routing and size limits are checked for every file before any file is
    /// written, so a refused call leaves all files unchanged.
    ///
    /// # Errors
    ///
    /// Returns routing errors, size-limit errors, or a writer I/O error (which
    /// poisons the recorder).
    pub fn write(
        &mut self,
        frames: usize,
        buses: &[(RecordBus, &[f32])],
    ) -> Result<(), TrackWavError> {
        if self.poisoned {
            return Err(TrackWavError::Poisoned);
        }
        let buffers = match self.mode {
            TrackFileMode::Interleaved => vec![self.map.route_interleaved(frames, buses)?],
            TrackFileMode::SplitPerTrack => self.map.route(frames, buses)?,
        };
        let frame_count = frames as u64;
        for (file, writer) in self.writers.iter().enumerate() {
            if writer.is_poisoned() {
                return Err(TrackWavError::Wav {
                    file,
                    error: WavError::Poisoned,
                });
            }
            if !writer.can_accept_frames(frame_count) {
                return Err(TrackWavError::Wav {
                    file,
                    error: WavError::SizeLimit {
                        requested: writer.data_bytes().saturating_add(
                            frame_count.saturating_mul(u64::from(writer.spec().block_align())),
                        ),
                        limit: writer.spec().max_data_bytes(),
                    },
                });
            }
        }
        for (file, (writer, buffer)) in self.writers.iter_mut().zip(&buffers).enumerate() {
            if let Err(error) = writer.write_f32(buffer) {
                self.poisoned = true;
                return Err(TrackWavError::Wav { file, error });
            }
        }
        Ok(())
    }

    /// Checkpoints every file.
    ///
    /// # Errors
    ///
    /// Returns the first writer error.
    pub fn checkpoint(&mut self) -> Result<(), TrackWavError> {
        for (file, writer) in self.writers.iter_mut().enumerate() {
            writer
                .checkpoint()
                .map_err(|error| TrackWavError::Wav { file, error })?;
        }
        Ok(())
    }

    /// Finalizes every file in sink order.
    ///
    /// # Errors
    ///
    /// Returns the first writer error; files not yet finalized remain
    /// recoverable with [`crate::recover_wav`].
    pub fn finalize(self) -> Result<Vec<FinalizedWav>, TrackWavError> {
        self.writers
            .into_iter()
            .enumerate()
            .map(|(file, writer)| {
                writer
                    .finalize()
                    .map_err(|error| TrackWavError::Wav { file, error })
            })
            .collect()
    }
}
