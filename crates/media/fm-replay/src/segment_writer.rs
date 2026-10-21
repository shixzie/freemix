//! Rolling replay segment files cut from one fragmented MP4 stream.
//!
//! A replay source is recorded by one long-running muxer whose fragmented
//! MP4 output (an empty `moov`, then one `moof` + `mdat` fragment per
//! keyframe) streams into a [`ReplaySegmentWriter`]. The writer cuts that
//! stream with `fm-record`'s [`Fmp4Splitter`] into whole fragments and
//! writes them into rolling segment files of
//! [`SegmentWriterConfig::fragments_per_segment`] fragments each. Every
//! segment starts with a copy of the init segment, so each file is
//! independently playable and can be deleted on its own once retention
//! releases it.
//!
//! The writer reports each segment it opens and closes
//! ([`SegmentFileEvent`]) with the exact number of video frames a closed
//! file holds, counted from the fragments' sample tables, so a caller that
//! numbered the frames it fed the muxer knows exactly which frames each file
//! holds without probing it. A trailing `mfra` index is dropped: its offsets
//! describe the original stream, not the segment files.

use std::{
    fmt,
    fs::{File, OpenOptions},
    io::{self, Write},
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use fm_record::{Fmp4Splitter, Fmp4Unit, SplitError};

/// Largest init segment or fragment the writer buffers by default.
pub const DEFAULT_SEGMENT_UNIT_BYTES: u64 = 64 * 1024 * 1024;

/// Where and how a [`ReplaySegmentWriter`] cuts its files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SegmentWriterConfig {
    /// The directory the segment files are created in; it must exist.
    pub directory: PathBuf,
    /// The file name prefix: segment `n` is `<prefix>-<n:06>.mp4`.
    pub prefix: String,
    /// Fragments per segment file (the last one may hold fewer).
    pub fragments_per_segment: NonZeroU32,
    /// Largest init segment or fragment buffered from the stream.
    pub max_unit_bytes: u64,
}

impl SegmentWriterConfig {
    /// The path of segment `index`.
    #[must_use]
    pub fn segment_path(&self, index: u64) -> PathBuf {
        segment_path(&self.directory, &self.prefix, index)
    }
}

/// The path of segment `index` of `prefix` in `directory`.
#[must_use]
pub fn segment_path(directory: &Path, prefix: &str, index: u64) -> PathBuf {
    directory.join(format!("{prefix}-{index:06}.mp4"))
}

/// A segment file opened or finished by the writer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SegmentFileEvent {
    /// Segment `index` was created and holds its first fragment.
    Opened { index: u64, path: PathBuf },
    /// Segment `index` is complete: it holds `video_frames` frames (the
    /// frames after every earlier segment's, in stream order) in `bytes`.
    Closed {
        index: u64,
        path: PathBuf,
        video_frames: u64,
        bytes: u64,
    },
}

/// Why the writer stopped.
#[derive(Debug)]
pub enum SegmentWriterError {
    /// The stream is not a relocatable fragmented MP4.
    Stream(SplitError),
    /// The init segment declares no video track.
    NoVideoTrack,
    /// A segment file could not be created or written.
    Io(io::Error),
}

impl fmt::Display for SegmentWriterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stream(error) => write!(formatter, "replay stream: {error}"),
            Self::NoVideoTrack => formatter.write_str("replay stream has no video track"),
            Self::Io(error) => write!(formatter, "replay segment file: {error}"),
        }
    }
}

impl std::error::Error for SegmentWriterError {}

impl From<io::Error> for SegmentWriterError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

struct OpenSegment {
    index: u64,
    path: PathBuf,
    file: File,
    fragments: u32,
    video_frames: u64,
    bytes: u64,
}

/// Cuts one fragmented MP4 stream into rolling segment files. See the
/// module documentation.
pub struct ReplaySegmentWriter {
    config: SegmentWriterConfig,
    splitter: Fmp4Splitter,
    init: Option<Vec<u8>>,
    video_track: Option<u32>,
    current: Option<OpenSegment>,
    next_index: u64,
    units: Vec<Fmp4Unit>,
    failed: bool,
}

impl fmt::Debug for ReplaySegmentWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReplaySegmentWriter")
            .field("config", &self.config)
            .field("next_index", &self.next_index)
            .finish_non_exhaustive()
    }
}

impl ReplaySegmentWriter {
    /// A writer whose first segment is index 1.
    #[must_use]
    pub fn new(config: SegmentWriterConfig) -> Self {
        let splitter = Fmp4Splitter::new(config.max_unit_bytes);
        Self {
            config,
            splitter,
            init: None,
            video_track: None,
            current: None,
            next_index: 1,
            units: Vec::new(),
            failed: false,
        }
    }

    /// The configuration the writer cuts by.
    #[must_use]
    pub const fn config(&self) -> &SegmentWriterConfig {
        &self.config
    }

    /// Appends muxer output and writes every fragment it completes.
    ///
    /// # Errors
    ///
    /// A stream that is not a relocatable fragmented MP4, has no video
    /// track, or a segment file that cannot be written. Errors are sticky:
    /// the open segment is left as it is (it ends on a whole fragment).
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SegmentFileEvent>, SegmentWriterError> {
        if self.failed {
            return Err(SegmentWriterError::Stream(SplitError::Poisoned));
        }
        let mut units = std::mem::take(&mut self.units);
        let result = self
            .splitter
            .push(bytes, &mut units)
            .map_err(SegmentWriterError::Stream)
            .and_then(|()| self.write_units(&mut units));
        units.clear();
        self.units = units;
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Ends the stream: closes the open segment.
    ///
    /// # Errors
    ///
    /// The same failures as [`Self::push`]. A stream that ended inside a
    /// fragment still closes the segment on its last whole fragment and
    /// reports the truncation.
    pub fn finish(&mut self) -> Result<Vec<SegmentFileEvent>, SegmentWriterError> {
        let mut events = Vec::new();
        let mut finished = Ok(());
        if !self.failed {
            let mut units = Vec::new();
            finished = self
                .splitter
                .finish(&mut units)
                .map_err(SegmentWriterError::Stream)
                .and_then(|()| {
                    events = self.write_units(&mut units)?;
                    Ok(())
                });
        }
        if let Some(event) = self.close_current()? {
            events.push(event);
        }
        finished.map(|()| events)
    }

    fn write_units(
        &mut self,
        units: &mut Vec<Fmp4Unit>,
    ) -> Result<Vec<SegmentFileEvent>, SegmentWriterError> {
        let mut events = Vec::new();
        for unit in units.drain(..) {
            match unit {
                Fmp4Unit::Init { bytes, movie } => {
                    self.video_track = movie
                        .tracks()
                        .iter()
                        .find(|track| &track.handler == b"vide")
                        .map(|track| track.track_id);
                    if self.video_track.is_none() {
                        return Err(SegmentWriterError::NoVideoTrack);
                    }
                    self.init = Some(bytes);
                }
                Fmp4Unit::Fragment { bytes, fragment } => {
                    let full = self.current.as_ref().is_some_and(|segment| {
                        segment.fragments >= self.config.fragments_per_segment.get()
                    });
                    if full && let Some(event) = self.close_current()? {
                        events.push(event);
                    }
                    if self.current.is_none() {
                        events.push(self.open_next()?);
                    }
                    let video = self.video_track;
                    let frames: u64 = fragment
                        .runs
                        .iter()
                        .filter(|run| Some(run.track_id) == video)
                        .map(|run| run.samples)
                        .sum();
                    let segment = self.current.as_mut().expect("a segment was opened above");
                    segment.file.write_all(&bytes)?;
                    segment.fragments = segment.fragments.saturating_add(1);
                    segment.video_frames = segment.video_frames.saturating_add(frames);
                    segment.bytes = segment
                        .bytes
                        .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                }
                Fmp4Unit::Index { .. } => {}
            }
        }
        Ok(events)
    }

    fn open_next(&mut self) -> Result<SegmentFileEvent, SegmentWriterError> {
        let init = self
            .init
            .as_ref()
            .ok_or(SegmentWriterError::Stream(SplitError::Invalid {
                reason: "fragment before init segment",
                offset: 0,
            }))?;
        let index = self.next_index;
        let path = self.config.segment_path(index);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(init)?;
        self.next_index += 1;
        self.current = Some(OpenSegment {
            index,
            path: path.clone(),
            file,
            fragments: 0,
            video_frames: 0,
            bytes: u64::try_from(init.len()).unwrap_or(u64::MAX),
        });
        Ok(SegmentFileEvent::Opened { index, path })
    }

    fn close_current(&mut self) -> Result<Option<SegmentFileEvent>, SegmentWriterError> {
        let Some(segment) = self.current.take() else {
            return Ok(None);
        };
        segment.file.sync_data()?;
        Ok(Some(SegmentFileEvent::Closed {
            index: segment.index,
            path: segment.path,
            video_frames: segment.video_frames,
            bytes: segment.bytes,
        }))
    }
}
