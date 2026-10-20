//! Frame-accurate local media-file playback over the bounded `FFmpeg` tools.
//!
//! [`FilePlayer`] couples the backend-independent
//! [`fm_playback::MediaPlayback`] transport with a packet index and windowed,
//! verified decodes of one local file:
//!
//! * **Index.** One bounded ffprobe packet scan of the selected video stream
//!   yields every presentation timestamp (sorted, so B-frame reordering is
//!   handled) and which frames are keyframes. Frame `n` is the `n`-th
//!   presentation timestamp. Only constant-frame-rate video is admitted: every
//!   timestamp must sit within half a frame of its nominal grid position.
//! * **Keyframe-aware seek.** A window starting at frame `n` seeks the demuxer
//!   to keyframe `k <= n` (`-ss` at `k`'s time, without `FFmpeg`'s accurate-seek
//!   trimming), keeps original timestamps (`-copyts`), and decodes forward,
//!   selecting frames by exact timestamp (`pts >= pts(n)`). A `showinfo` side
//!   chain reports every emitted timestamp, and the window is accepted only
//!   when those timestamps equal the index's `pts(n..)`. Seeking to the
//!   keyframe's own time suits both demuxer families (those that snap
//!   backward, such as Matroska and NUT, and `MPEG-TS`, which snaps forward to
//!   the next keyframe). A landing after the target is retried from earlier
//!   keyframes and finally from the start of the file, so a window is either
//!   exact or an error — never a silently shifted frame.
//! * **Audio.** Audio sample `s` of a track is the sample at `s / rate` seconds
//!   after video frame zero. Windows trim by exact sample timestamps
//!   (`atrim=start_pts`) and verify the first emitted timestamp with
//!   `ashowinfo`; a track that starts after the requested position is padded
//!   with leading silence, while a demuxer overshoot is retried earlier.
//!   Placement is as exact as the container's audio timestamps: sample-exact
//!   for per-stream time bases (NUT, MP4/MOV with a sample-rate timescale,
//!   WAV), within one sample for `MPEG-TS` (90 kHz), and within ±0.5 ms for
//!   Matroska (millisecond timestamps).
//! * **Audio-only files.** A file without a video stream (WAV, MP3, AAC/M4A,
//!   FLAC, Ogg Vorbis/Opus: a music bed or a sting) opens only when
//!   [`FilePlaybackConfig::audio_only_picture`] names a picture size; it is
//!   refused ([`Error::MissingStream`]) otherwise. Its frame timeline is
//!   synthesized at the output rate from the container duration (frame zero
//!   at the format start time, `ceil(duration * rate)` frames, every frame a
//!   keyframe), so transport, trims, loops, end actions, and audio placement
//!   behave exactly as for video; audio past the decoded end is silence. Its
//!   picture is one transparent-black frame of that size (straight alpha,
//!   sRGB), shared by every frame, so it shows as black on its own and is
//!   absent as a layer. An attached cover picture is not shown. A
//!   container duration that is estimated (raw MP3/AAC without an index)
//!   is as exact as `FFmpeg`'s estimate. Growing audio-only files are
//!   refused.
//! * **Growing files.** With [`GrowingFileConfig`] the player follows a file
//!   that is still being written (a recording in progress). [`FilePlayer::refresh`]
//!   (also called automatically while the transport waits at the live edge)
//!   detects appended bytes, rescans the index, and extends the transport. The
//!   newest `live_edge_offset_frames` frames are withheld because their packets
//!   may still be partial. The file must keep its identity and only grow: a
//!   shorter file, a different file at the path (rotation or replacement), or
//!   changed leading bytes are refused with typed errors. Use an appendable
//!   container such as `MPEG-TS`; formats that write their index at the end
//!   (`MP4`/`MOV`) cannot be probed until finished.
//!
//! Every tool run is bounded by the adapter's [`Limits`]. A growth rescan reads
//! the whole packet index again (bounded by the frame-metadata output limit and
//! timeout), so very long recordings should be followed with a generous limit.
//!
//! [`Limits`]: crate::Limits

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use fm_frame::{
    AlphaMode, AudioBlock, ChromaLocation, ClockDomainId, ColorMetadata, ColorPrimaries,
    CpuVideoFrame, CpuVideoPayload, CpuVideoPlane, MatrixCoefficients, PixelFormat, SignalRange,
    TimeBase, TransferFunction, VideoDimensions, VideoFrameMetadata,
};
use fm_playback::{
    AudioPlan, AudioTrack, FrameIndex, MediaControlError, MediaPlayback, MediaSourceConfig,
    MediaState, MediaTick, resample_linear,
};
use fm_types::FrameRate;
use serde_json::Value;

use crate::audio_seek::parse_input_start_microseconds;
use crate::decode::{
    decoded_limit, map_layout, mapped_video_metadata, media_timing, validate_video_stream,
};
use crate::probe::{StreamInfo, StreamKind, StreamSelector, parse_probe};
use crate::{Adapter, Error, LimitKind, Tool};

/// Leading bytes fingerprinted to detect an in-place rewrite of a source.
const PREFIX_FINGERPRINT_BYTES: usize = 64 * 1024;
/// Largest accepted video window; bounds `showinfo` diagnostics.
pub const MAX_PLAYBACK_WINDOW_FRAMES: u32 = 48;
/// Seek margins tried before decoding audio from the start of the file.
const AUDIO_SEEK_MARGINS_MICROSECONDS: [i64; 2] = [500_000, 4_000_000];
/// Earlier keyframes tried before decoding video from the start of the file.
const VIDEO_SEEK_ATTEMPTS: usize = 3;

/// Follow a file that is still being written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GrowingFileConfig {
    /// Newest indexed frames withheld from playback; at least one is always
    /// withheld because the final packet of a growing file may be partial.
    pub live_edge_offset_frames: u64,
    /// Treat the source as complete after this many consecutive
    /// [`FilePlayer::refresh`] calls observe no growth. `None` waits for
    /// [`FilePlayer::complete_source`].
    pub complete_after_idle_refreshes: Option<NonZeroU32>,
}

impl Default for GrowingFileConfig {
    fn default() -> Self {
        Self {
            live_edge_offset_frames: 2,
            complete_after_idle_refreshes: None,
        }
    }
}

/// How one local file is opened for playback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilePlaybackConfig {
    pub clock_domain: ClockDomainId,
    /// Output (program) frame rate that [`FilePlayer::tick`] is called at.
    pub output_rate: FrameRate,
    pub video: StreamSelector,
    /// Initially selected audio track, or `None` for silence.
    pub audio: Option<StreamSelector>,
    /// Video frames decoded per tool run, `1..=MAX_PLAYBACK_WINDOW_FRAMES`.
    pub window_frames: NonZeroU32,
    /// Minimum audio sample frames decoded per tool run.
    pub audio_window_samples: NonZeroU32,
    /// `Some` to follow a growing file.
    pub growing: Option<GrowingFileConfig>,
    /// The size of the transparent picture an audio-only file presents; with
    /// `None` (the default) a file without a video stream is refused.
    pub audio_only_picture: Option<VideoDimensions>,
}

impl FilePlaybackConfig {
    /// Defaults: best video and audio streams, 12-frame video windows,
    /// 48 000-sample audio windows, and a complete (non-growing) source.
    #[must_use]
    pub const fn new(clock_domain: ClockDomainId, output_rate: FrameRate) -> Self {
        Self {
            clock_domain,
            output_rate,
            video: StreamSelector::Best,
            audio: Some(StreamSelector::Best),
            window_frames: NonZeroU32::MIN.saturating_add(11),
            audio_window_samples: NonZeroU32::MIN.saturating_add(47_999),
            growing: None,
            audio_only_picture: None,
        }
    }
}

/// Typed file-playback failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilePlaybackError {
    Ffmpeg(Error),
    Control(MediaControlError),
    InvalidConfig,
    /// The selected video stream has no indexed frames yet.
    NoVideoFrames,
    /// The stream's frame rate is missing or zero.
    UnknownFrameRate,
    /// A timestamp is more than half a frame away from its nominal position.
    VariableFrameRate {
        frame: FrameIndex,
    },
    /// Packet timestamps are missing or duplicated.
    InvalidIndex,
    /// The source became shorter.
    SourceTruncated {
        previous_bytes: u64,
        current_bytes: u64,
    },
    /// A different file now sits at the source path (rotation/replacement).
    SourceReplaced,
    /// Previously read bytes or indexed timestamps changed in place.
    SourceRewritten,
    /// A complete (non-growing) source changed size or modification time.
    SourceChanged,
    /// A decode could not be verified to start at the requested frame.
    SeekVerificationFailed {
        frame: FrameIndex,
    },
    /// A complete source could not produce an indexed frame.
    FrameUnavailable {
        frame: FrameIndex,
    },
}

impl std::fmt::Display for FilePlaybackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ffmpeg(error) => error.fmt(formatter),
            Self::Control(error) => error.fmt(formatter),
            Self::InvalidConfig => formatter.write_str("invalid file playback configuration"),
            Self::NoVideoFrames => formatter.write_str("video stream has no frames yet"),
            Self::UnknownFrameRate => formatter.write_str("video frame rate is unknown"),
            Self::VariableFrameRate { frame } => {
                write!(formatter, "frame {frame} is off the constant-rate grid")
            }
            Self::InvalidIndex => formatter.write_str("video packet index is invalid"),
            Self::SourceTruncated {
                previous_bytes,
                current_bytes,
            } => write!(
                formatter,
                "source shrank from {previous_bytes} to {current_bytes} bytes"
            ),
            Self::SourceReplaced => formatter.write_str("a different file replaced the source"),
            Self::SourceRewritten => formatter.write_str("source content was rewritten in place"),
            Self::SourceChanged => formatter.write_str("complete source changed"),
            Self::SeekVerificationFailed { frame } => {
                write!(formatter, "decode could not be verified at frame {frame}")
            }
            Self::FrameUnavailable { frame } => {
                write!(formatter, "frame {frame} could not be decoded")
            }
        }
    }
}

impl std::error::Error for FilePlaybackError {}

impl From<Error> for FilePlaybackError {
    fn from(value: Error) -> Self {
        Self::Ffmpeg(value)
    }
}

impl From<MediaControlError> for FilePlaybackError {
    fn from(value: MediaControlError) -> Self {
        Self::Control(value)
    }
}

/// How the most recent video window was positioned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeekReport {
    pub target: FrameIndex,
    /// Keyframe the accepted decode started from, or frame zero.
    pub keyframe: FrameIndex,
    /// Frames decoded and discarded between the keyframe and the target.
    pub decode_forward_frames: u64,
    /// Tool runs needed; more than one means a demuxer overshoot was retried.
    pub attempts: u32,
}

/// Counters for tests and diagnostics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FilePlaybackTelemetry {
    pub index_scans: u64,
    pub video_windows: u64,
    pub audio_windows: u64,
    pub last_seek: Option<SeekReport>,
}

/// Result of [`FilePlayer::refresh`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRefresh {
    Unchanged,
    Grew { available_frames: u64 },
    Completed { frames: u64 },
}

/// One output tick of decoded media.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackOutput {
    pub tick: MediaTick,
    pub video: Option<Arc<CpuVideoFrame>>,
    /// The planned frame was not decodable yet (growing source); the previous
    /// frame is repeated instead.
    pub video_late: bool,
    pub audio: Option<AudioBlock>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VideoIndex {
    pts: Vec<i64>,
    keyframes: Vec<usize>,
}

#[derive(Clone, Debug)]
struct AudioStream {
    info: StreamInfo,
    start_pts: Option<i64>,
}

#[derive(Clone, Debug)]
struct VideoWindow {
    start: u64,
    frames: Vec<Arc<CpuVideoFrame>>,
}

#[derive(Clone, Debug)]
struct AudioWindow {
    track: u32,
    start: u64,
    channels: usize,
    interleaved: Vec<f32>,
    end_of_stream: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileState {
    size: u64,
    modified: Option<SystemTime>,
    identity: (u64, u64, Option<u32>, Option<u64>),
    prefix: u64,
    prefix_len: usize,
}

/// An audio-only file's synthesized frame timeline (microsecond timestamps
/// on the output-rate grid) and the transparent picture every frame shows.
#[derive(Clone, Debug)]
struct AudioOnly {
    start_microseconds: i64,
    duration_microseconds: i64,
    picture: Arc<CpuVideoFrame>,
}

/// The time base of an audio-only file's synthesized frame timestamps.
const AUDIO_ONLY_TIME_BASE: (u32, u32) = (1, 1_000_000);

/// A frame-accurate player over one local file. See the module documentation.
pub struct FilePlayer {
    adapter: Adapter,
    path: PathBuf,
    redaction: String,
    clock_domain: ClockDomainId,
    /// The selected video stream; `None` for an audio-only file.
    video: Option<StreamInfo>,
    /// An audio-only file's synthesized timeline and shared picture.
    audio_only: Option<AudioOnly>,
    time_base: TimeBase,
    frame_rate: FrameRate,
    width: u32,
    height: u32,
    format_start_microseconds: i64,
    index: VideoIndex,
    audio: Vec<AudioStream>,
    controller: MediaPlayback,
    growing: Option<GrowingFileConfig>,
    file: FileState,
    idle_refreshes: u32,
    window_frames: usize,
    audio_window_samples: usize,
    video_window: Option<VideoWindow>,
    /// The window after `video_window`, decoded ahead by [`FilePlayer::prefetch`].
    next_window: Option<VideoWindow>,
    audio_window: Option<AudioWindow>,
    last_video: Option<Arc<CpuVideoFrame>>,
    telemetry: FilePlaybackTelemetry,
}

impl Adapter {
    /// Opens one local file for frame-accurate playback.
    ///
    /// # Errors
    ///
    /// Returns a typed path, tool, probe, selector, index, frame-rate, or
    /// unsupported-format error.
    pub fn open_playback(
        &self,
        path: impl AsRef<Path>,
        config: FilePlaybackConfig,
    ) -> Result<FilePlayer, FilePlaybackError> {
        FilePlayer::open(self, path.as_ref(), &config)
    }
}

impl FilePlayer {
    #[allow(clippy::too_many_lines)]
    fn open(
        adapter: &Adapter,
        path: &Path,
        config: &FilePlaybackConfig,
    ) -> Result<Self, FilePlaybackError> {
        if config.window_frames.get() > MAX_PLAYBACK_WINDOW_FRAMES
            || config.window_frames.get() > adapter.limits().max_video_frames
        {
            return Err(FilePlaybackError::InvalidConfig);
        }
        let path = adapter.admit_path(path)?;
        let redaction = path.to_string_lossy().into_owned();
        let file = file_state(&path, adapter.limits().max_input_bytes)?;
        let output = adapter.run_unchecked(
            &redaction,
            Tool::Ffprobe,
            &crate::probe_args(&path),
            adapter.limits().probe_timeout,
            adapter.limits().max_probe_stdout_bytes,
        )?;
        let probe = parse_probe(&output.stdout, adapter.limits().max_streams)?;
        let format_start_microseconds =
            parse_input_start_microseconds(probe.format.start_time.as_deref())?;
        let details = stream_details(adapter, &path, &redaction)?;
        let video = match probe.select_video(config.video) {
            Ok(video) => Some(video.clone()),
            Err(Error::MissingStream)
                if config.audio_only_picture.is_some()
                    && config.growing.is_none()
                    && probe.select_audio(StreamSelector::Best).is_ok() =>
            {
                None
            }
            Err(error) => return Err(error.into()),
        };
        let (width, height, time_base, frame_rate, audio_only) =
            match (&video, config.audio_only_picture) {
                (Some(video), _) => {
                    let (width, height, _) = validate_video_stream(adapter.limits(), video)?;
                    let time_base = video.time_base.ok_or(Error::MalformedProbe)?;
                    let frame_rate = details
                        .iter()
                        .find(|detail| detail.index == video.index)
                        .and_then(|detail| detail.frame_rate)
                        .ok_or(FilePlaybackError::UnknownFrameRate)?;
                    (width, height, time_base, frame_rate, None)
                }
                (None, Some(size)) => {
                    let duration_microseconds =
                        parse_input_start_microseconds(probe.format.duration.as_deref())?;
                    if probe.format.duration.is_none() || duration_microseconds <= 0 {
                        return Err(FilePlaybackError::NoVideoFrames);
                    }
                    let time_base = TimeBase::new(AUDIO_ONLY_TIME_BASE.0, AUDIO_ONLY_TIME_BASE.1)
                        .map_err(|_| Error::InvalidTimeline)?;
                    let picture = transparent_picture(
                        size,
                        format_start_microseconds,
                        time_base,
                        config.output_rate,
                        config.clock_domain,
                    )?;
                    (
                        size.width(),
                        size.height(),
                        time_base,
                        config.output_rate,
                        Some(AudioOnly {
                            start_microseconds: format_start_microseconds,
                            duration_microseconds,
                            picture: Arc::new(picture),
                        }),
                    )
                }
                (None, None) => return Err(Error::MissingStream.into()),
            };

        let audio = probe
            .streams
            .iter()
            .filter(|stream| stream.kind == StreamKind::Audio)
            .filter(|stream| stream.sample_rate.is_some() && stream.channels.is_some())
            .map(|stream| AudioStream {
                start_pts: details
                    .iter()
                    .find(|detail| detail.index == stream.index)
                    .and_then(|detail| detail.start_pts),
                info: stream.clone(),
            })
            .collect::<Vec<_>>();
        let tracks = audio
            .iter()
            .map(|stream| AudioTrack {
                stream_index: stream.info.index,
                sample_rate: stream.info.sample_rate.unwrap_or(0),
                channels: u16::try_from(stream.info.channels.unwrap_or(0)).unwrap_or(u16::MAX),
                language: details
                    .iter()
                    .find(|detail| detail.index == stream.info.index)
                    .and_then(|detail| detail.language.clone()),
                title: details
                    .iter()
                    .find(|detail| detail.index == stream.info.index)
                    .and_then(|detail| detail.title.clone()),
                default: stream.info.default,
            })
            .collect::<Vec<_>>();
        let selected_audio = match config.audio {
            None => None,
            Some(selector) => match probe.select_audio(selector) {
                Ok(stream) => Some(stream.index),
                Err(Error::MissingStream) if selector == StreamSelector::Best => None,
                Err(error) => return Err(error.into()),
            },
        };

        let growing = config.growing.map(|growing| GrowingFileConfig {
            live_edge_offset_frames: growing.live_edge_offset_frames.max(1),
            ..growing
        });
        let mut player = Self {
            adapter: adapter.clone(),
            path,
            redaction,
            clock_domain: config.clock_domain,
            video,
            audio_only,
            time_base,
            frame_rate,
            width,
            height,
            format_start_microseconds,
            index: VideoIndex {
                pts: Vec::new(),
                keyframes: Vec::new(),
            },
            audio,
            controller: MediaPlayback::new(MediaSourceConfig {
                source_rate: frame_rate,
                output_rate: config.output_rate,
                available_frames: 0,
                complete: growing.is_none(),
                audio_tracks: tracks,
                selected_audio,
            })?,
            growing,
            file,
            idle_refreshes: 0,
            window_frames: usize::try_from(config.window_frames.get())
                .map_err(|_| FilePlaybackError::InvalidConfig)?,
            audio_window_samples: usize::try_from(config.audio_window_samples.get())
                .map_err(|_| FilePlaybackError::InvalidConfig)?,
            video_window: None,
            next_window: None,
            audio_window: None,
            last_video: None,
            telemetry: FilePlaybackTelemetry::default(),
        };
        player.index = player.scan_index()?;
        let frames = player.frame_count();
        if growing.is_none() && frames == 0 {
            return Err(FilePlaybackError::NoVideoFrames);
        }
        // A fresh transport accepts any count; completion follows `growing`.
        player.controller = MediaPlayback::new(MediaSourceConfig {
            source_rate: frame_rate,
            output_rate: config.output_rate,
            available_frames: frames,
            complete: growing.is_none(),
            audio_tracks: player.controller.audio_tracks().to_vec(),
            selected_audio,
        })?;
        if let Some(growing) = growing {
            player
                .controller
                .set_live_edge_offset(growing.live_edge_offset_frames);
        }
        Ok(player)
    }

    /// The transport (read-only).
    #[must_use]
    pub const fn controller(&self) -> &MediaPlayback {
        &self.controller
    }

    /// The transport: play, pause, seek, trim, rate, loop, end actions, and
    /// audio-track selection all go through it.
    pub const fn controller_mut(&mut self) -> &mut MediaPlayback {
        &mut self.controller
    }

    #[must_use]
    pub fn audio_tracks(&self) -> &[AudioTrack] {
        self.controller.audio_tracks()
    }

    #[must_use]
    pub const fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }

    /// Whether the file has no video stream: its frames are synthesized at
    /// the output rate and every one presents the same transparent picture.
    #[must_use]
    pub const fn is_audio_only(&self) -> bool {
        self.audio_only.is_some()
    }

    /// Frames currently indexed (including any withheld live-edge frames).
    #[must_use]
    pub fn frame_count(&self) -> u64 {
        u64::try_from(self.index.pts.len()).unwrap_or(u64::MAX)
    }

    /// Indexed keyframes in presentation order.
    #[must_use]
    pub fn keyframes(&self) -> Vec<FrameIndex> {
        self.index
            .keyframes
            .iter()
            .map(|&ordinal| FrameIndex::new(u64::try_from(ordinal).unwrap_or(u64::MAX)))
            .collect()
    }

    /// The last keyframe at or before `frame` (frame zero if none).
    #[must_use]
    pub fn keyframe_at_or_before(&self, frame: FrameIndex) -> FrameIndex {
        let ordinal = usize::try_from(frame.get()).unwrap_or(usize::MAX);
        FrameIndex::new(
            self.keyframe_ordinals_before(ordinal)
                .first()
                .map_or(0, |&value| u64::try_from(value).unwrap_or(u64::MAX)),
        )
    }

    #[must_use]
    pub const fn telemetry(&self) -> FilePlaybackTelemetry {
        self.telemetry
    }

    /// Advances one output tick and decodes what it presents.
    ///
    /// While a growing source waits at its live edge, this first calls
    /// [`Self::refresh`].
    ///
    /// # Errors
    ///
    /// Returns a typed source-identity, tool, verification, or decode error.
    pub fn tick(&mut self) -> Result<PlaybackOutput, FilePlaybackError> {
        if self.growing.is_some() && self.controller.state() == MediaState::Waiting {
            self.refresh()?;
        }
        let tick = self.controller.tick();
        let (video, video_late) = match tick.frame {
            Some(frame) => match self.frame(frame)? {
                Some(decoded) => {
                    self.last_video = Some(Arc::clone(&decoded));
                    (Some(decoded), false)
                }
                None => (self.last_video.clone(), true),
            },
            None => (self.last_video.clone(), false),
        };
        let audio = self.audio_for(&tick)?;
        Ok(PlaybackOutput {
            tick,
            video,
            video_late,
            audio,
        })
    }

    /// Decodes what an externally driven transport's `tick` presents.
    ///
    /// [`Self::tick`] advances this player's own transport; an authority that
    /// owns the transport elsewhere (the engine, which realizes operator
    /// commands on exact frames) computes each tick itself and hands it here.
    /// The tick must come from a transport over this file's index and audio
    /// tracks (same frame count, live-edge offset, and track indexes), with a
    /// playable range inside this player's. A planned frame a growing source
    /// cannot produce yet repeats the previous frame (`video_late`).
    ///
    /// # Errors
    ///
    /// Returns a typed source-identity, tool, verification, or decode error,
    /// or [`MediaControlError::UnknownAudioTrack`] for a track this file does
    /// not have.
    pub fn present(&mut self, tick: MediaTick) -> Result<PlaybackOutput, FilePlaybackError> {
        let (video, video_late) = match tick.frame {
            Some(frame) => match self.frame(frame)? {
                Some(decoded) => {
                    self.last_video = Some(Arc::clone(&decoded));
                    (Some(decoded), false)
                }
                None => (self.last_video.clone(), true),
            },
            None => (self.last_video.clone(), false),
        };
        let audio = self.audio_for(&tick)?;
        Ok(PlaybackOutput {
            tick,
            video,
            video_late,
            audio,
        })
    }

    /// Decodes the video window after the current one ahead of need, so
    /// forward playback crosses a window boundary without waiting on a tool
    /// run. Does nothing (returns `false`) when that window is already
    /// decoded, nothing was presented yet, or the current window reaches the
    /// playable end.
    ///
    /// # Errors
    ///
    /// Returns a typed source-identity, tool, or verification error.
    pub fn prefetch(&mut self) -> Result<bool, FilePlaybackError> {
        let Some(window) = &self.video_window else {
            return Ok(false);
        };
        let next = window.start + u64::try_from(window.frames.len()).unwrap_or(u64::MAX);
        if self
            .next_window
            .as_ref()
            .is_some_and(|ahead| ahead.start == next)
        {
            return Ok(false);
        }
        let start = usize::try_from(next).map_err(|_| Error::InvalidTimeline)?;
        let end = usize::try_from(self.controller.playable_end().get())
            .map_err(|_| Error::InvalidTimeline)?
            .min(self.index.pts.len());
        if start >= end {
            return Ok(false);
        }
        let frames = self.decode_video_window(start, self.window_frames.min(end - start))?;
        if frames.is_empty() {
            return Ok(false);
        }
        self.next_window = Some(VideoWindow {
            start: next,
            frames,
        });
        Ok(true)
    }

    /// Decodes one exact source frame (random access, e.g. for scrubbing).
    ///
    /// # Errors
    ///
    /// Returns [`FilePlaybackError::FrameUnavailable`] outside the playable
    /// range, or a typed decode error.
    pub fn decode_frame(
        &mut self,
        frame: FrameIndex,
    ) -> Result<Arc<CpuVideoFrame>, FilePlaybackError> {
        if frame >= self.controller.playable_end() {
            return Err(FilePlaybackError::FrameUnavailable { frame });
        }
        self.frame(frame)?
            .ok_or(FilePlaybackError::FrameUnavailable { frame })
    }

    /// Polls the source for growth and identity changes.
    ///
    /// # Errors
    ///
    /// Returns [`FilePlaybackError::SourceTruncated`],
    /// [`FilePlaybackError::SourceReplaced`],
    /// [`FilePlaybackError::SourceRewritten`], or (for a complete source)
    /// [`FilePlaybackError::SourceChanged`]; or a rescan error.
    pub fn refresh(&mut self) -> Result<SourceRefresh, FilePlaybackError> {
        let current = self.check_identity()?;
        let Some(growing) = self.growing else {
            return Ok(SourceRefresh::Unchanged);
        };
        if self.controller.is_complete() {
            return Ok(SourceRefresh::Unchanged);
        }
        if current.size == self.file.size {
            self.idle_refreshes = self.idle_refreshes.saturating_add(1);
            if growing
                .complete_after_idle_refreshes
                .is_some_and(|limit| self.idle_refreshes >= limit.get())
            {
                return self.complete_source();
            }
            return Ok(SourceRefresh::Unchanged);
        }
        self.idle_refreshes = 0;
        let index = self.scan_index()?;
        self.accept_growth(index, growing.live_edge_offset_frames)?;
        self.file = FileState {
            prefix: self.file.prefix,
            prefix_len: self.file.prefix_len,
            ..current
        };
        let available_frames = self.frame_count();
        self.controller.update_source(available_frames, false)?;
        Ok(SourceRefresh::Grew { available_frames })
    }

    /// Declares a growing source finished: rescans once and exposes every
    /// indexed frame, so end actions apply at the true end.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::refresh`].
    pub fn complete_source(&mut self) -> Result<SourceRefresh, FilePlaybackError> {
        let current = self.check_identity()?;
        if self.controller.is_complete() {
            return Ok(SourceRefresh::Unchanged);
        }
        let offset = self
            .growing
            .map_or(0, |growing| growing.live_edge_offset_frames);
        let index = self.scan_index()?;
        self.accept_growth(index, offset)?;
        self.file = FileState {
            prefix: self.file.prefix,
            prefix_len: self.file.prefix_len,
            ..current
        };
        let frames = self.frame_count();
        self.controller.update_source(frames, true)?;
        Ok(SourceRefresh::Completed { frames })
    }

    fn accept_growth(&mut self, index: VideoIndex, offset: u64) -> Result<(), FilePlaybackError> {
        let confirmed = self
            .index
            .pts
            .len()
            .saturating_sub(usize::try_from(offset).unwrap_or(usize::MAX));
        if index.pts.len() < self.index.pts.len()
            || index.pts.get(..confirmed) != self.index.pts.get(..confirmed)
        {
            return Err(FilePlaybackError::SourceRewritten);
        }
        self.index = index;
        Ok(())
    }

    fn check_identity(&self) -> Result<FileState, FilePlaybackError> {
        let current = match file_state(&self.path, self.adapter.limits().max_input_bytes) {
            Ok(current) => current,
            Err(FilePlaybackError::Ffmpeg(Error::InputNotFound)) => {
                return Err(FilePlaybackError::SourceReplaced);
            }
            Err(error) => return Err(error),
        };
        if current.identity != self.file.identity {
            return Err(FilePlaybackError::SourceReplaced);
        }
        if current.size < self.file.size {
            return Err(FilePlaybackError::SourceTruncated {
                previous_bytes: self.file.size,
                current_bytes: current.size,
            });
        }
        if prefix_fingerprint(&self.path, self.file.prefix_len)? != self.file.prefix {
            return Err(FilePlaybackError::SourceRewritten);
        }
        if self.growing.is_none()
            && (current.size != self.file.size || current.modified != self.file.modified)
        {
            return Err(FilePlaybackError::SourceChanged);
        }
        Ok(current)
    }

    fn scan_index(&mut self) -> Result<VideoIndex, FilePlaybackError> {
        let Some(video) = &self.video else {
            return self
                .audio_only
                .as_ref()
                .map(|audio_only| audio_only_index(audio_only, self.frame_rate))
                .ok_or(FilePlaybackError::NoVideoFrames)?;
        };
        let args = [
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-select_streams",
            &video.index.to_string(),
            "-show_entries",
            "packet=pts,flags",
            "-of",
            "json",
        ]
        .into_iter()
        .map(OsString::from)
        .chain([self.path.as_os_str().to_owned()])
        .collect::<Vec<_>>();
        let output = self.adapter.run_unchecked(
            &self.redaction,
            Tool::Ffprobe,
            &args,
            self.adapter.limits().frame_metadata_timeout,
            self.adapter.limits().max_frame_metadata_stdout_bytes,
        )?;
        self.telemetry.index_scans += 1;
        let index = parse_packet_index(&output.stdout, self.growing.is_some())?;
        validate_constant_rate(&index.pts, self.time_base, self.frame_rate)?;
        Ok(index)
    }

    fn keyframe_ordinals_before(&self, ordinal: usize) -> Vec<usize> {
        let position = self.index.keyframes.partition_point(|&key| key <= ordinal);
        self.index.keyframes[..position]
            .iter()
            .rev()
            .copied()
            .collect()
    }

    /// Returns the decoded frame, or `None` when a growing source cannot
    /// produce it yet.
    fn frame(
        &mut self,
        frame: FrameIndex,
    ) -> Result<Option<Arc<CpuVideoFrame>>, FilePlaybackError> {
        if let Some(audio_only) = &self.audio_only {
            // Every frame of an audio-only file shows the same picture.
            let end = self.controller.playable_end().get().min(self.frame_count());
            return if frame.get() < end {
                Ok(Some(Arc::clone(&audio_only.picture)))
            } else {
                Err(FilePlaybackError::FrameUnavailable { frame })
            };
        }
        if let Some(window) = &self.video_window
            && frame.get() >= window.start
            && let Some(decoded) = usize::try_from(frame.get() - window.start)
                .ok()
                .and_then(|offset| window.frames.get(offset))
        {
            return Ok(Some(Arc::clone(decoded)));
        }
        if let Some(window) = &self.next_window
            && frame.get() >= window.start
            && let Some(decoded) = usize::try_from(frame.get() - window.start)
                .ok()
                .and_then(|offset| window.frames.get(offset))
        {
            let decoded = Arc::clone(decoded);
            self.video_window = self.next_window.take();
            return Ok(Some(decoded));
        }
        let start = usize::try_from(frame.get()).map_err(|_| Error::InvalidTimeline)?;
        let end = usize::try_from(self.controller.playable_end().get())
            .map_err(|_| Error::InvalidTimeline)?
            .min(self.index.pts.len());
        if start >= end {
            return if self.controller.is_complete() {
                Err(FilePlaybackError::FrameUnavailable { frame })
            } else {
                Ok(None)
            };
        }
        let count = self.window_frames.min(end - start);
        let frames = self.decode_video_window(start, count)?;
        if frames.is_empty() {
            return if self.controller.is_complete() {
                Err(FilePlaybackError::FrameUnavailable { frame })
            } else {
                Ok(None)
            };
        }
        let first = Arc::clone(&frames[0]);
        self.video_window = Some(VideoWindow {
            start: frame.get(),
            frames,
        });
        Ok(Some(first))
    }

    fn decode_video_window(
        &mut self,
        start: usize,
        count: usize,
    ) -> Result<Vec<Arc<CpuVideoFrame>>, FilePlaybackError> {
        self.check_identity()?;
        let video = self
            .video
            .as_ref()
            .ok_or(FilePlaybackError::NoVideoFrames)?
            .index;
        let target = FrameIndex::new(u64::try_from(start).unwrap_or(u64::MAX));
        let frame_bytes = usize::try_from(self.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .and_then(|stride| {
                usize::try_from(self.height)
                    .ok()
                    .and_then(|height| stride.checked_mul(height))
            })
            .ok_or_else(|| {
                decoded_limit(u64::MAX, self.adapter.limits().max_total_decoded_bytes)
            })?;
        let total = frame_bytes
            .checked_mul(count)
            .filter(|total| *total <= self.adapter.limits().max_total_decoded_bytes)
            .ok_or_else(|| {
                decoded_limit(u64::MAX, self.adapter.limits().max_total_decoded_bytes)
            })?;
        let expected = &self.index.pts[start..start + count];
        let mut candidates = self
            .keyframe_ordinals_before(start)
            .into_iter()
            .filter(|&keyframe| keyframe > 0)
            .take(VIDEO_SEEK_ATTEMPTS)
            .map(Some)
            .collect::<Vec<_>>();
        candidates.push(None);
        for (attempt, keyframe) in candidates.into_iter().enumerate() {
            let attempts = u32::try_from(attempt + 1).unwrap_or(u32::MAX);
            let seek = match keyframe {
                Some(keyframe) => {
                    let microseconds =
                        pts_microseconds_floor(self.index.pts[keyframe], self.time_base)?
                            - self.format_start_microseconds;
                    (microseconds > 0).then_some(microseconds)
                }
                None => None,
            };
            let args = video_window_args(&self.path, video, seek, expected[0], count);
            let output = self.adapter.run_unchecked(
                &self.redaction,
                Tool::Ffmpeg,
                &args,
                self.adapter.limits().decode_timeout,
                total,
            )?;
            self.telemetry.video_windows += 1;
            let emitted = showinfo_pts(&output.stderr, "showinfo");
            if output.stdout.len() % frame_bytes != 0
                || output.stdout.len() / frame_bytes != emitted.len()
            {
                return Err(Error::OutputMismatch {
                    expected: total,
                    actual: output.stdout.len(),
                }
                .into());
            }
            if emitted.is_empty() {
                if self.controller.is_complete() {
                    continue;
                }
                return Ok(Vec::new());
            }
            if emitted[..] != expected[..emitted.len()] {
                // Demuxer landed after the target; retry from an earlier keyframe.
                continue;
            }
            let keyframe_ordinal = keyframe.unwrap_or(0);
            self.telemetry.last_seek = Some(SeekReport {
                target,
                keyframe: FrameIndex::new(u64::try_from(keyframe_ordinal).unwrap_or(u64::MAX)),
                decode_forward_frames: u64::try_from(start - keyframe_ordinal.min(start))
                    .unwrap_or(u64::MAX),
                attempts,
            });
            return output
                .stdout
                .chunks_exact(frame_bytes)
                .zip(emitted)
                .enumerate()
                .map(|(offset, (bytes, pts))| {
                    self.video_frame(bytes.to_vec(), pts, start + offset)
                        .map(Arc::new)
                })
                .collect();
        }
        Err(FilePlaybackError::SeekVerificationFailed { frame: target })
    }

    fn video_frame(
        &self,
        bytes: Vec<u8>,
        pts: i64,
        ordinal: usize,
    ) -> Result<CpuVideoFrame, FilePlaybackError> {
        let duration = u64::try_from(
            self.index
                .pts
                .get(ordinal + 1)
                .map_or_else(|| self.nominal_frame_ticks(), |next| next - pts),
        )
        .map_err(|_| Error::InvalidTimeline)?;
        let timing = media_timing(pts, duration, self.time_base, self.clock_domain, ordinal)?;
        let dimensions =
            VideoDimensions::new(self.width, self.height).ok_or(Error::FrameConstruction)?;
        let stride = usize::try_from(self.width).map_err(|_| Error::FrameConstruction)? * 4;
        let plane = CpuVideoPlane::new(stride, bytes).map_err(|_| Error::FrameConstruction)?;
        let payload = CpuVideoPayload::new(PixelFormat::Rgba8, dimensions, vec![plane])
            .map_err(|_| Error::FrameConstruction)?;
        let frame = CpuVideoFrame::new(timing, payload);
        match self.video.as_ref().and_then(mapped_video_metadata) {
            Some(metadata) => Ok(frame
                .with_metadata(metadata)
                .map_err(|_| Error::FrameConstruction)?),
            None => Ok(frame),
        }
    }

    fn nominal_frame_ticks(&self) -> i64 {
        let numerator =
            i128::from(self.time_base.denominator()) * i128::from(self.frame_rate.denominator());
        let denominator =
            i128::from(self.time_base.numerator()) * i128::from(self.frame_rate.numerator());
        i64::try_from(numerator / denominator.max(1)).unwrap_or(1)
    }

    fn audio_for(&mut self, tick: &MediaTick) -> Result<Option<AudioBlock>, FilePlaybackError> {
        let (track, start, end, output_samples, resample) = match tick.audio {
            AudioPlan::None => return Ok(None),
            AudioPlan::Silence {
                track,
                output_samples,
            } => (track, 0, 0, output_samples, false),
            AudioPlan::Source {
                track,
                start_sample,
                end_sample,
                output_samples,
                resample,
            } => (track, start_sample, end_sample, output_samples, resample),
        };
        let stream = self
            .audio
            .iter()
            .find(|stream| stream.info.index == track)
            .ok_or(MediaControlError::UnknownAudioTrack(track))?
            .info
            .clone();
        let sample_rate = stream.sample_rate.ok_or(Error::MalformedProbe)?;
        let channels = usize::try_from(stream.channels.ok_or(Error::MalformedProbe)?)
            .map_err(|_| Error::MalformedProbe)?;
        let (_, layout) = map_layout(
            stream.channel_layout.as_deref(),
            stream.channels.unwrap_or(0),
        )?;
        let output_len = usize::try_from(output_samples).map_err(|_| Error::InvalidTimeline)?;
        let planes = if end > start {
            let interleaved = self.audio_samples(track, start, end)?;
            let mut planes = deinterleave(&interleaved, channels);
            for plane in &mut planes {
                if resample {
                    *plane = resample_linear(plane, output_len);
                } else {
                    plane.resize(output_len, 0.0);
                }
            }
            planes
        } else {
            vec![vec![0.0; output_len]; channels]
        };
        let pts = if end > start {
            start
        } else {
            tick.output_tick.saturating_mul(u64::from(output_samples))
        };
        let timing = media_timing(
            i64::try_from(pts).map_err(|_| Error::InvalidTimeline)?,
            u64::from(output_samples).max(1),
            TimeBase::new(1, sample_rate).map_err(|_| Error::MalformedProbe)?,
            self.clock_domain,
            usize::try_from(tick.output_tick).map_err(|_| Error::InvalidTimeline)?,
        )?;
        let rate = fm_frame::SampleRate::new(sample_rate).ok_or(Error::MalformedProbe)?;
        AudioBlock::new(timing, rate, layout, planes)
            .map(Some)
            .map_err(|_| Error::FrameConstruction.into())
    }

    /// Interleaved samples `start..end` of `track`; shorter at end of data.
    fn audio_samples(
        &mut self,
        track: u32,
        start: u64,
        end: u64,
    ) -> Result<Vec<f32>, FilePlaybackError> {
        if let Some(window) = &self.audio_window
            && window.track == track
            && start >= window.start
        {
            let available = u64::try_from(window.interleaved.len() / window.channels.max(1))
                .unwrap_or(u64::MAX);
            let window_end = window.start + available;
            if end <= window_end || (window.end_of_stream && start <= window_end) {
                let from = usize::try_from(start - window.start)
                    .map_err(|_| Error::InvalidTimeline)?
                    * window.channels;
                let to = usize::try_from(end.min(window_end) - window.start)
                    .map_err(|_| Error::InvalidTimeline)?
                    * window.channels;
                return Ok(window.interleaved[from..to].to_vec());
            }
        }
        let wanted = usize::try_from(end - start).map_err(|_| Error::InvalidTimeline)?;
        let count = wanted.max(self.audio_window_samples);
        let window = self.decode_audio_window(track, start, count)?;
        let available = window.interleaved.len() / window.channels.max(1);
        let take = available.min(wanted) * window.channels;
        let samples = window.interleaved[..take].to_vec();
        self.audio_window = Some(window);
        Ok(samples)
    }

    #[allow(clippy::too_many_lines)]
    fn decode_audio_window(
        &mut self,
        track: u32,
        start: u64,
        count: usize,
    ) -> Result<AudioWindow, FilePlaybackError> {
        self.check_identity()?;
        let stream = self
            .audio
            .iter()
            .find(|stream| stream.info.index == track)
            .ok_or(MediaControlError::UnknownAudioTrack(track))?
            .clone();
        let sample_rate = stream.info.sample_rate.ok_or(Error::MalformedProbe)?;
        let channels = usize::try_from(stream.info.channels.ok_or(Error::MalformedProbe)?)
            .map_err(|_| Error::MalformedProbe)?;
        if count > self.adapter.limits().max_audio_samples {
            return Err(Error::LimitExceeded {
                kind: LimitKind::AudioSamples,
                actual: u64::try_from(count).unwrap_or(u64::MAX),
                maximum: u64::try_from(self.adapter.limits().max_audio_samples).unwrap_or(u64::MAX),
            }
            .into());
        }
        let total = count
            .checked_mul(channels)
            .and_then(|samples| samples.checked_mul(4))
            .filter(|bytes| *bytes <= self.adapter.limits().max_total_decoded_bytes)
            .ok_or_else(|| {
                decoded_limit(u64::MAX, self.adapter.limits().max_total_decoded_bytes)
            })?;
        // Absolute sample timestamp of video frame zero on this track.
        let video_start = *self
            .index
            .pts
            .first()
            .ok_or(FilePlaybackError::NoVideoFrames)?;
        let base = rescale_floor(video_start, self.time_base, sample_rate)?;
        let first = base
            .checked_add(i64::try_from(start).map_err(|_| Error::InvalidTimeline)?)
            .ok_or(Error::InvalidTimeline)?;
        let last = first
            .checked_add(i64::try_from(count).map_err(|_| Error::InvalidTimeline)?)
            .ok_or(Error::InvalidTimeline)?;
        let target_microseconds =
            i64::try_from(i128::from(first) * 1_000_000 / i128::from(sample_rate))
                .map_err(|_| Error::InvalidTimeline)?
                - self.format_start_microseconds;
        let track_start = stream
            .start_pts
            .map(|pts| {
                rescale_floor(
                    pts,
                    stream.info.time_base.unwrap_or(self.time_base),
                    sample_rate,
                )
            })
            .transpose()?;
        let mut seeks = AUDIO_SEEK_MARGINS_MICROSECONDS
            .iter()
            .map(|margin| target_microseconds - margin)
            .filter(|seek| *seek > 0)
            .map(Some)
            .collect::<Vec<_>>();
        seeks.push(None);
        for seek in seeks {
            let args = audio_window_args(&self.path, track, seek, first, last);
            let output = self.adapter.run_unchecked(
                &self.redaction,
                Tool::Ffmpeg,
                &args,
                self.adapter.limits().decode_timeout,
                total,
            )?;
            self.telemetry.audio_windows += 1;
            if output.stdout.len() % (channels * 4) != 0 {
                return Err(Error::OutputMismatch {
                    expected: total,
                    actual: output.stdout.len(),
                }
                .into());
            }
            let decoded = output
                .stdout
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_le_bytes(*bytes))
                .collect::<Vec<_>>();
            if decoded.iter().any(|sample| !sample.is_finite()) {
                return Err(Error::NonFiniteAudio.into());
            }
            let emitted_first = showinfo_pts(&output.stderr, "ashowinfo").first().copied();
            let lead = match emitted_first {
                None => 0,
                Some(pts) if pts == first => 0,
                Some(pts) if pts > first && track_start.is_some_and(|start| pts <= start) => {
                    // The track starts later than the requested position.
                    usize::try_from(pts - first).map_err(|_| Error::InvalidTimeline)?
                }
                Some(_) if seek.is_some() => continue,
                Some(_) => {
                    return Err(FilePlaybackError::SeekVerificationFailed {
                        frame: FrameIndex::new(start),
                    });
                }
            };
            let mut interleaved = vec![0.0; lead.min(count) * channels];
            interleaved.extend(decoded);
            interleaved.truncate(count * channels);
            let produced = interleaved.len() / channels;
            return Ok(AudioWindow {
                track,
                start,
                channels,
                end_of_stream: produced < count && self.controller.is_complete(),
                interleaved,
            });
        }
        Err(FilePlaybackError::SeekVerificationFailed {
            frame: FrameIndex::new(start),
        })
    }
}

/// An audio-only file's synthesized index: `ceil(duration * rate)` frames
/// on the output-rate grid from the format start, every one a keyframe.
fn audio_only_index(
    audio_only: &AudioOnly,
    frame_rate: FrameRate,
) -> Result<VideoIndex, FilePlaybackError> {
    let numerator = i128::from(frame_rate.numerator());
    let denominator = i128::from(frame_rate.denominator());
    // Frames whose start lies before the end: ceil(duration * rate).
    let scaled = i128::from(audio_only.duration_microseconds) * numerator;
    let unit = denominator * 1_000_000;
    let frames = usize::try_from((scaled + unit - 1) / unit).map_err(|_| Error::InvalidTimeline)?;
    let pts = (0..frames)
        .map(|ordinal| {
            let offset = i128::try_from(ordinal).unwrap_or(i128::MAX) * unit / numerator;
            i64::try_from(i128::from(audio_only.start_microseconds) + offset)
                .map_err(|_| Error::InvalidTimeline.into())
        })
        .collect::<Result<Vec<_>, FilePlaybackError>>()?;
    Ok(VideoIndex {
        keyframes: (0..frames).collect(),
        pts,
    })
}

/// The transparent-black picture an audio-only file presents: `size`,
/// straight alpha, sRGB, dated at the file's frame zero.
fn transparent_picture(
    size: VideoDimensions,
    start_microseconds: i64,
    time_base: TimeBase,
    frame_rate: FrameRate,
    clock_domain: ClockDomainId,
) -> Result<CpuVideoFrame, FilePlaybackError> {
    let duration = u64::try_from(
        i128::from(frame_rate.denominator()) * 1_000_000
            / i128::from(frame_rate.numerator()).max(1),
    )
    .map_err(|_| Error::InvalidTimeline)?
    .max(1);
    let timing = media_timing(start_microseconds, duration, time_base, clock_domain, 0)?;
    let payload = CpuVideoPayload::allocate(PixelFormat::Rgba8, size)
        .map_err(|_| Error::FrameConstruction)?;
    CpuVideoFrame::new(timing, payload)
        .with_metadata(VideoFrameMetadata::new(
            ColorMetadata {
                primaries: ColorPrimaries::Bt709,
                transfer: TransferFunction::Srgb,
                matrix: MatrixCoefficients::Identity,
                range: SignalRange::Full,
                chroma_location: ChromaLocation::Center,
            },
            Some(AlphaMode::Straight),
        ))
        .map_err(|_| Error::FrameConstruction.into())
}

struct StreamDetail {
    index: u32,
    frame_rate: Option<FrameRate>,
    start_pts: Option<i64>,
    language: Option<String>,
    title: Option<String>,
}

fn stream_details(
    adapter: &Adapter,
    path: &Path,
    redaction: &str,
) -> Result<Vec<StreamDetail>, FilePlaybackError> {
    let args = [
        "-v",
        "error",
        "-protocol_whitelist",
        "file",
        "-show_entries",
        "stream=index,avg_frame_rate,r_frame_rate,start_pts:stream_tags=language,title",
        "-of",
        "json",
    ]
    .into_iter()
    .map(OsString::from)
    .chain([path.as_os_str().to_owned()])
    .collect::<Vec<_>>();
    let output = adapter.run_unchecked(
        redaction,
        Tool::Ffprobe,
        &args,
        adapter.limits().probe_timeout,
        adapter.limits().max_probe_stdout_bytes,
    )?;
    let root: Value = serde_json::from_slice(&output.stdout).map_err(|_| Error::MalformedProbe)?;
    let streams = root
        .get("streams")
        .and_then(Value::as_array)
        .ok_or(Error::MalformedProbe)?;
    streams
        .iter()
        .map(|stream| {
            let index = stream
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| u32::try_from(index).ok())
                .ok_or(Error::MalformedProbe)?;
            let rate = |key: &str| stream.get(key).and_then(Value::as_str).and_then(parse_rate);
            let tag = |key: &str| {
                stream
                    .get("tags")
                    .and_then(|tags| tags.get(key))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            Ok(StreamDetail {
                index,
                frame_rate: rate("avg_frame_rate").or_else(|| rate("r_frame_rate")),
                start_pts: stream.get("start_pts").and_then(Value::as_i64),
                language: tag("language"),
                title: tag("title"),
            })
        })
        .collect()
}

fn parse_rate(value: &str) -> Option<FrameRate> {
    let (numerator, denominator) = value.split_once('/')?;
    FrameRate::new(numerator.parse().ok()?, denominator.parse().ok()?).ok()
}

fn parse_packet_index(bytes: &[u8], growing: bool) -> Result<VideoIndex, FilePlaybackError> {
    let root: Value = serde_json::from_slice(bytes).map_err(|_| Error::MalformedProbe)?;
    let packets = root
        .get("packets")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let mut entries = Vec::with_capacity(packets.len());
    let last = packets.len().saturating_sub(1);
    for (position, packet) in packets.iter().enumerate() {
        let flags = packet.get("flags").and_then(Value::as_str).unwrap_or("");
        if flags.contains('D') {
            continue;
        }
        if flags.contains('C') {
            // A corrupt final packet is expected while a file is being written.
            if growing && position == last {
                continue;
            }
            return Err(FilePlaybackError::InvalidIndex);
        }
        let pts = packet
            .get("pts")
            .and_then(Value::as_i64)
            .ok_or(FilePlaybackError::InvalidIndex)?;
        entries.push((pts, flags.contains('K')));
    }
    entries.sort_unstable_by_key(|(pts, _)| *pts);
    if entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(FilePlaybackError::InvalidIndex);
    }
    Ok(VideoIndex {
        keyframes: entries
            .iter()
            .enumerate()
            .filter(|(_, (_, key))| *key)
            .map(|(ordinal, _)| ordinal)
            .collect(),
        pts: entries.into_iter().map(|(pts, _)| pts).collect(),
    })
}

fn validate_constant_rate(
    pts: &[i64],
    time_base: TimeBase,
    frame_rate: FrameRate,
) -> Result<(), FilePlaybackError> {
    let Some(&first) = pts.first() else {
        return Ok(());
    };
    // Frame n nominally sits at first + n * tb_den * fr_den / (tb_num * fr_num)
    // ticks; compare in units scaled by 2 * tb_num * fr_num to stay exact.
    let scale = i128::from(time_base.numerator()) * i128::from(frame_rate.numerator());
    let step = i128::from(time_base.denominator()) * i128::from(frame_rate.denominator());
    for (ordinal, &value) in pts.iter().enumerate() {
        let actual = (i128::from(value) - i128::from(first)) * scale * 2;
        let nominal = i128::try_from(ordinal).unwrap_or(i128::MAX) * step * 2;
        if (actual - nominal).abs() > step {
            return Err(FilePlaybackError::VariableFrameRate {
                frame: FrameIndex::new(u64::try_from(ordinal).unwrap_or(u64::MAX)),
            });
        }
    }
    Ok(())
}

fn video_window_args(
    path: &Path,
    stream: u32,
    seek_microseconds: Option<i64>,
    first_pts: i64,
    count: usize,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-nostdin"),
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("info"),
        OsString::from("-protocol_whitelist"),
        OsString::from("file"),
        OsString::from("-noautorotate"),
        OsString::from("-copyts"),
    ];
    if let Some(seek) = seek_microseconds {
        args.extend([
            OsString::from("-noaccurate_seek"),
            OsString::from("-ss"),
            OsString::from(format!("{seek}us")),
        ]);
    }
    args.extend([
        OsString::from("-i"),
        path.as_os_str().to_owned(),
        OsString::from("-filter_complex"),
        OsString::from(format!(
            "[0:{stream}]select='gte(pts,{first_pts})',trim=end_frame={count},\
             split=2[out][verify];[verify]showinfo,nullsink"
        )),
        OsString::from("-map"),
        OsString::from("[out]"),
        OsString::from("-frames:v"),
        OsString::from(count.to_string()),
        OsString::from("-fps_mode"),
        OsString::from("passthrough"),
        OsString::from("-an"),
        OsString::from("-sn"),
        OsString::from("-dn"),
        OsString::from("-pix_fmt"),
        OsString::from("rgba"),
        OsString::from("-f"),
        OsString::from("rawvideo"),
        OsString::from("pipe:1"),
    ]);
    args
}

fn audio_window_args(
    path: &Path,
    stream: u32,
    seek_microseconds: Option<i64>,
    first_pts: i64,
    end_pts: i64,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-nostdin"),
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("info"),
        OsString::from("-protocol_whitelist"),
        OsString::from("file"),
        OsString::from("-copyts"),
    ];
    if let Some(seek) = seek_microseconds {
        args.extend([
            OsString::from("-noaccurate_seek"),
            OsString::from("-ss"),
            OsString::from(format!("{seek}us")),
        ]);
    }
    args.extend([
        OsString::from("-i"),
        path.as_os_str().to_owned(),
        OsString::from("-filter_complex"),
        OsString::from(format!(
            "[0:{stream}]atrim=start_pts={first_pts}:end_pts={end_pts},\
             asplit=2[out][verify];[verify]atrim=end_sample=1,ashowinfo,anullsink"
        )),
        OsString::from("-map"),
        OsString::from("[out]"),
        OsString::from("-vn"),
        OsString::from("-sn"),
        OsString::from("-dn"),
        OsString::from("-c:a"),
        OsString::from("pcm_f32le"),
        OsString::from("-f"),
        OsString::from("f32le"),
        OsString::from("pipe:1"),
    ]);
    args
}

/// Timestamps of every frame line printed by one `showinfo`/`ashowinfo`.
fn showinfo_pts(stderr: &[u8], filter: &str) -> Vec<i64> {
    let marker = format!("Parsed_{filter}_");
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| line.contains(&marker) && line.contains(" n:"))
        .filter_map(|line| {
            let rest = &line[line.find(" pts:")? + " pts:".len()..];
            let rest = rest.trim_start();
            let end = rest
                .find(|character: char| !(character.is_ascii_digit() || character == '-'))
                .unwrap_or(rest.len());
            rest[..end].parse().ok()
        })
        .collect()
}

fn deinterleave(interleaved: &[f32], channels: usize) -> Vec<Vec<f32>> {
    let channels = channels.max(1);
    (0..channels)
        .map(|channel| {
            interleaved
                .iter()
                .skip(channel)
                .step_by(channels)
                .copied()
                .collect()
        })
        .collect()
}

fn pts_microseconds_floor(pts: i64, time_base: TimeBase) -> Result<i64, Error> {
    crate::audio_seek::timestamp_microseconds_floor(pts, time_base)
}

/// `pts` in `time_base` rescaled to `1 / sample_rate`, rounded down.
fn rescale_floor(pts: i64, time_base: TimeBase, sample_rate: u32) -> Result<i64, Error> {
    let numerator = i128::from(pts) * i128::from(time_base.numerator()) * i128::from(sample_rate);
    i64::try_from(numerator.div_euclid(i128::from(time_base.denominator())))
        .map_err(|_| Error::InvalidTimeline)
}

fn file_state(path: &Path, maximum: u64) -> Result<FileState, FilePlaybackError> {
    let metadata = fs::metadata(path).map_err(|error| crate::map_input_io(&error))?;
    if !metadata.is_file() {
        return Err(Error::InputNotRegularFile.into());
    }
    if metadata.len() > maximum {
        return Err(Error::LimitExceeded {
            kind: LimitKind::InputBytes,
            actual: metadata.len(),
            maximum,
        }
        .into());
    }
    let (volume, file_index) = crate::platform_file_identity(path);
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino(), volume, file_index)
    };
    #[cfg(not(unix))]
    let identity = (0, 0, volume, file_index);
    let prefix_len = usize::try_from(metadata.len())
        .unwrap_or(usize::MAX)
        .min(PREFIX_FINGERPRINT_BYTES);
    Ok(FileState {
        size: metadata.len(),
        modified: metadata.modified().ok(),
        identity,
        prefix: prefix_fingerprint(path, prefix_len)?,
        prefix_len,
    })
}

/// FNV-1a over the first `length` bytes.
fn prefix_fingerprint(path: &Path, length: usize) -> Result<u64, FilePlaybackError> {
    let mut file = File::open(path).map_err(|error| crate::map_input_io(&error))?;
    let mut buffer = vec![0; length];
    let mut filled = 0;
    while filled < length {
        let read = file
            .read(&mut buffer[filled..])
            .map_err(|error| crate::map_input_io(&error))?;
        if read == 0 {
            return Err(FilePlaybackError::SourceTruncated {
                previous_bytes: u64::try_from(length).unwrap_or(u64::MAX),
                current_bytes: u64::try_from(filled).unwrap_or(u64::MAX),
            });
        }
        filled += read;
    }
    Ok(buffer.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn showinfo_parser_reads_padded_and_compact_timestamps() {
        let stderr = b"[Parsed_showinfo_3 @ 0x1] config in time_base: 1/90000\n\
[Parsed_showinfo_3 @ 0x1] n:   0 pts: 262800 pts_time:2.92 duration: 3600\n\
[Parsed_showinfo_3 @ 0x1] n:   1 pts: 266400 pts_time:2.96 duration: 3600\n\
[Parsed_ashowinfo_5 @ 0x2] n:0 pts:48000 pts_time:1 fmt:flt\n";
        assert_eq!(showinfo_pts(stderr, "showinfo"), [262_800, 266_400]);
        assert_eq!(showinfo_pts(stderr, "ashowinfo"), [48_000]);
    }

    #[test]
    fn packet_index_sorts_reordered_frames_and_tracks_keyframes() {
        let json = br#"{"packets":[{"pts":0,"flags":"K__"},{"pts":120,"flags":"___"},
            {"pts":40,"flags":"___"},{"pts":80,"flags":"___"},{"pts":160,"flags":"K__"},
            {"pts":200,"flags":"__C"}]}"#;
        let index = parse_packet_index(json, true).unwrap();
        assert_eq!(index.pts, [0, 40, 80, 120, 160]);
        assert_eq!(index.keyframes, [0, 4]);
        assert_eq!(
            parse_packet_index(json, false),
            Err(FilePlaybackError::InvalidIndex)
        );
    }

    #[test]
    fn constant_rate_validation_allows_rounded_timestamps_only() {
        let time_base = TimeBase::new(1, 1_000).unwrap();
        let ntsc = FrameRate::new(30_000, 1_001).unwrap();
        assert!(validate_constant_rate(&[0, 33, 67, 100, 133], time_base, ntsc).is_ok());
        assert_eq!(
            validate_constant_rate(&[0, 33, 90], time_base, ntsc),
            Err(FilePlaybackError::VariableFrameRate {
                frame: FrameIndex::new(2)
            })
        );
    }
}
