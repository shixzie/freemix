//! Native realization of the `LiveLAN` viewer stream (OR-017).
//!
//! While `LiveLAN` is enabled and `--live-lan-listen` runs, one bounded
//! `FFmpeg` streamer encodes the chosen source (Program, or one project
//! output, with the audio that output hears) into a short local HLS
//! playlist in a daemon-owned directory, which [`crate::live_lan`] serves
//! to viewers. The render loop never waits on it: the encoder starts and
//! stops on worker threads, and each frame costs one readback and one
//! nonblocking enqueue. A change of source restarts the encoder into a new
//! directory; a change of PIN or viewer limit only changes who may watch.
//!
//! The status ([`LiveLanStatus`]) is reported when it changes, with the
//! viewer count and the bytes served sampled at most once a second. A
//! failure is logged as `FREEMIXD_LIVELAN_FAILURE` (never with the PIN,
//! which this module only hands to the listener) and the encoder retries
//! after [`RETRY_DELAY`].

use std::{
    fmt::Write as _,
    fs,
    num::NonZeroU32,
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use fm_codec_ffmpeg::{
    record::{PairedFrame, RecordFormat},
    stream::{EncoderSettings, StreamConfig, StreamDestination, StreamState, Streamer},
};
use fm_frame::AudioBlock;
use fm_gpu::NativeTexture;
use fm_protocol::{LiveLanConfig, LiveLanSource, LiveLanState, LiveLanStatus};
use freemixd::native_media::{NativeMediaRuntime, NativeProgramReadback};

use crate::live_lan::{LiveLanAccess, LiveLanHandle, viewer_url};

/// Video bitrate of the viewer stream: comfortable for a LAN at 1080p.
const VIDEO_BITRATE_KBPS: u32 = 2_500;
const AUDIO_BITRATE_KBPS: u32 = 128;
/// One-second segments keep the delay to a few seconds.
const SEGMENT_SECONDS: u32 = 1;
/// How long a failed encoder waits before it starts again.
pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(5);
/// How often the viewer count and bytes served are sampled.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// How often the viewer address of a listener on `0.0.0.0` or `::` is
/// looked up again, so a network change (or a network that came up after
/// the daemon started) shows the address that works now.
const URL_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

enum Encoder {
    Idle,
    Starting {
        started: Receiver<Result<Streamer, String>>,
        readback: NativeProgramReadback,
        format: RecordFormat,
        directory: PathBuf,
    },
    Running {
        streamer: Box<Streamer>,
        readback: NativeProgramReadback,
        format: RecordFormat,
        directory: PathBuf,
        published: bool,
    },
    Failed {
        at: Instant,
    },
}

/// The `LiveLAN` stream of one native daemon.
pub(crate) struct NativeLiveLan {
    handle: LiveLanHandle,
    settings: fm_model::ProjectSettings,
    /// Parent of every encoder directory, private to this process and
    /// created on first use; removed on drop.
    root: PathBuf,
    root_created: bool,
    starts: u64,
    /// The source the encoder streams while enabled.
    source: Option<LiveLanSource>,
    encoder: Encoder,
    url: String,
    /// When `url` was last looked up.
    url_resolved: Instant,
    viewers: u8,
    egress_kbps: u32,
    sampled: Option<(Instant, u64)>,
}

impl NativeLiveLan {
    pub(crate) fn new(handle: LiveLanHandle, settings: fm_model::ProjectSettings) -> Self {
        let mut suffix = [0_u8; 8];
        let _ = getrandom::fill(&mut suffix);
        let suffix = suffix.iter().fold(String::new(), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        });
        let root =
            std::env::temp_dir().join(format!("freemixd-live-lan-{}-{suffix}", std::process::id()));
        let url = viewer_url(handle.address());
        Self {
            handle,
            settings,
            root,
            root_created: false,
            starts: 0,
            source: None,
            encoder: Encoder::Idle,
            url,
            url_resolved: Instant::now(),
            viewers: 0,
            egress_kbps: 0,
            sampled: None,
        }
    }

    /// The source to capture this frame, if the encoder takes frames.
    pub(crate) fn capturing(&self) -> Option<LiveLanSource> {
        matches!(self.encoder, Encoder::Running { .. })
            .then_some(self.source)
            .flatten()
    }

    /// Follows the project's `LiveLAN` configuration: who may watch, and
    /// whether (and what) the encoder streams. `sequence` is the next
    /// frame's.
    pub(crate) fn sync(
        &mut self,
        config: &LiveLanConfig,
        runtime: &NativeMediaRuntime,
        sequence: u64,
    ) {
        self.handle.set_access(config.enabled.then(|| {
            LiveLanAccess::new(
                config
                    .pin
                    .as_ref()
                    .map(fm_protocol::LiveLanPin::expose_secret),
                config.max_viewers,
            )
        }));
        let wanted = config.enabled.then_some(config.source);
        if wanted != self.source {
            self.stop_encoder();
            if self.source.is_some() && wanted.is_some() {
                self.handle.end_sessions();
            }
            self.source = wanted;
        }
        if self.source.is_none() {
            return;
        }
        self.advance(runtime, sequence);
    }

    fn advance(&mut self, runtime: &NativeMediaRuntime, sequence: u64) {
        match &mut self.encoder {
            Encoder::Idle => self.start_encoder(runtime, sequence),
            Encoder::Failed { at } if at.elapsed() >= RETRY_DELAY => {
                self.encoder = Encoder::Idle;
                self.start_encoder(runtime, sequence);
            }
            Encoder::Failed { .. } => {}
            Encoder::Starting { started, .. } => match started.try_recv() {
                Err(TryRecvError::Empty) => {}
                Ok(Ok(streamer)) => {
                    let Encoder::Starting {
                        readback,
                        format,
                        directory,
                        ..
                    } = std::mem::replace(&mut self.encoder, Encoder::Idle)
                    else {
                        unreachable!("the encoder is starting");
                    };
                    self.encoder = Encoder::Running {
                        streamer: Box::new(streamer),
                        readback,
                        format,
                        directory,
                        published: false,
                    };
                }
                Ok(Err(failure)) => self.fail(&failure),
                Err(TryRecvError::Disconnected) => self.fail("start:worker_lost"),
            },
            Encoder::Running {
                streamer,
                directory,
                published,
                ..
            } => {
                let telemetry = streamer.telemetry();
                if telemetry.state == StreamState::Failed || telemetry.failure.is_some() {
                    let failure = format!("stream:{:?}", telemetry.failure);
                    self.fail(&failure);
                } else if !*published && directory.join("index.m3u8").is_file() {
                    *published = true;
                    self.handle.set_media(Some(directory.clone()));
                }
            }
        }
    }

    fn start_encoder(&mut self, runtime: &NativeMediaRuntime, sequence: u64) {
        let video = self.settings.video.dimensions;
        let (Some(width), Some(height)) = (
            NonZeroU32::new(video.width()),
            NonZeroU32::new(video.height()),
        ) else {
            self.fail("configure:empty_picture");
            return;
        };
        let readback = match runtime.create_program_readback_blocking(width, height) {
            Ok(readback) => readback,
            Err(error) => {
                self.fail(&format!("readback:{error}"));
                return;
            }
        };
        let format = match RecordFormat::new(
            video.width(),
            video.height(),
            self.settings.frame_rate,
            self.settings.audio.sample_rate,
            self.settings.audio.channels.clone(),
            fm_frame::SequenceNumber::new(sequence),
        ) {
            Ok(format) => format,
            Err(error) => {
                self.fail(&format!("format:{error}"));
                return;
            }
        };
        if !self.root_created {
            // A fresh, owner-only directory: one that already exists (another
            // user's, or a link) is refused rather than written into.
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            if builder.create(&self.root).is_err() {
                self.fail("configure:directory");
                return;
            }
            self.root_created = true;
        }
        self.starts += 1;
        let directory = self.root.join(self.starts.to_string());
        let Some(destination) = directory
            .to_str()
            .and_then(|path| StreamDestination::parse(&format!("hls://{path}")).ok())
        else {
            self.fail("configure:directory");
            return;
        };
        let mut config = StreamConfig::new(format.clone(), destination);
        config.encoder = EncoderSettings {
            video_bitrate_kbps: VIDEO_BITRATE_KBPS,
            audio_bitrate_kbps: AUDIO_BITRATE_KBPS,
            keyframe_interval_seconds: SEGMENT_SECONDS,
        };
        let (sender, started) = mpsc::sync_channel(1);
        let abandoned = directory.clone();
        let spawned = thread::Builder::new()
            .name("freemixd-live-lan-start".into())
            .spawn(move || {
                let started = Streamer::start(config).map_err(|error| format!("start:{error}"));
                if let Err(mpsc::SendError(started)) = sender.send(started) {
                    // Stopped while starting: nothing waits for this one.
                    if let Ok(mut streamer) = started {
                        let _ = streamer.cancel();
                    }
                    let _ = fs::remove_dir_all(abandoned);
                }
            });
        if spawned.is_err() {
            self.fail("start:thread");
            return;
        }
        self.encoder = Encoder::Starting {
            started,
            readback,
            format,
            directory,
        };
    }

    /// Encodes this frame's `texture` and `audio`.
    pub(crate) fn capture(
        &mut self,
        runtime: &NativeMediaRuntime,
        texture: Option<&NativeTexture>,
        audio: AudioBlock,
    ) {
        let Encoder::Running {
            streamer,
            readback,
            format,
            ..
        } = &mut self.encoder
        else {
            return;
        };
        let Some(texture) = texture else {
            self.fail("capture:missing_output");
            return;
        };
        let sequence = audio.timing().sequence();
        let frame = match runtime.readback_program_blocking(readback, texture) {
            Ok(frame) => frame,
            Err(error) => {
                self.fail(&format!("readback:{error}"));
                return;
            }
        };
        if frame.width.checked_mul(4) != Some(frame.stride)
            || frame.width != readback.width()
            || frame.height != readback.height()
        {
            self.fail("readback:invalid_tight_layout");
            return;
        }
        match PairedFrame::new(format, sequence, frame.rgba, audio) {
            // A full queue drops the oldest pair; any other refusal surfaces
            // as the streamer's failure on the next frame.
            Ok(pair) => {
                let _ = streamer.enqueue(pair);
            }
            Err(error) => self.fail(&format!("frame:{error}")),
        }
    }

    fn fail(&mut self, failure: &str) {
        eprintln!(
            "FREEMIXD_LIVELAN_FAILURE\tv=1\tfailure={}",
            crate::diagnostic_field(failure)
        );
        self.stop_encoder();
        self.encoder = Encoder::Failed { at: Instant::now() };
    }

    /// Stops the encoder off the loop and forgets what it published.
    fn stop_encoder(&mut self) {
        self.handle.set_media(None);
        match std::mem::replace(&mut self.encoder, Encoder::Idle) {
            Encoder::Running {
                mut streamer,
                directory,
                ..
            } => {
                let _ = thread::Builder::new()
                    .name("freemixd-live-lan-stop".into())
                    .spawn(move || {
                        let _ = streamer.cancel();
                        let _ = fs::remove_dir_all(directory);
                    });
            }
            // Dropping the receiver makes the start worker cancel the
            // streamer it is still starting and remove its directory.
            Encoder::Starting { started, .. } => drop(started),
            Encoder::Idle | Encoder::Failed { .. } => {}
        }
    }

    /// The status clients see, sampled at most once a second.
    pub(crate) fn status(&mut self, enabled: bool) -> LiveLanStatus {
        let now = Instant::now();
        let due = self
            .sampled
            .is_none_or(|(at, _)| now.duration_since(at) >= SAMPLE_INTERVAL);
        if due {
            let bytes = self.handle.egress_bytes();
            if let Some((at, before)) = self.sampled {
                // Bits per millisecond are kilobits per second.
                let elapsed_ms = now.duration_since(at).as_millis().max(1);
                let bits = u128::from(bytes.saturating_sub(before)) * 8;
                self.egress_kbps = u32::try_from(bits / elapsed_ms).unwrap_or(u32::MAX);
            }
            self.sampled = Some((now, bytes));
            self.viewers = self.handle.viewers();
        }
        let address = self.handle.address();
        if enabled
            && address.ip().is_unspecified()
            && now.duration_since(self.url_resolved) >= URL_REFRESH_INTERVAL
        {
            self.url = viewer_url(address);
            self.url_resolved = now;
        }
        let state = match (&self.encoder, enabled) {
            (_, false) => LiveLanState::Off,
            (Encoder::Failed { .. }, true) => LiveLanState::Failed,
            (
                Encoder::Running {
                    published: true, ..
                },
                true,
            ) => LiveLanState::Live,
            (_, true) => LiveLanState::Starting,
        };
        LiveLanStatus {
            state,
            url: if enabled {
                self.url.clone()
            } else {
                String::new()
            },
            viewers: if enabled { self.viewers } else { 0 },
            egress_kbps: if enabled { self.egress_kbps } else { 0 },
        }
    }

    /// Stops the encoder within its own bound, for shutdown.
    pub(crate) fn shutdown(&mut self) {
        self.handle.set_access(None);
        self.handle.set_media(None);
        if let Encoder::Running { mut streamer, .. } =
            std::mem::replace(&mut self.encoder, Encoder::Idle)
        {
            let _ = streamer.cancel();
        }
        if self.root_created {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

impl Drop for NativeLiveLan {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use fm_types::{
        AudioFormat, ChannelLayout, ColorMetadata, FrameRate, PixelFormat, SampleFormat,
        SampleRate, ScanMode, VideoDimensions, VideoFormat,
    };

    use super::*;

    fn settings() -> fm_model::ProjectSettings {
        let frame_rate = FrameRate::new(25, 1).unwrap();
        fm_model::ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(320, 180).unwrap(),
                frame_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(48_000).unwrap(),
                sample_format: SampleFormat::F32,
                channels: ChannelLayout::stereo(),
            },
        }
    }

    /// A listener on an unspecified address advertises the address viewers
    /// can open now, looked up again while `LiveLAN` is on, not only the one
    /// known when the daemon started.
    #[test]
    fn the_viewer_address_of_an_unspecified_bind_is_looked_up_again() {
        let server = crate::live_lan::LiveLanServer::start("0.0.0.0:0".parse().unwrap()).unwrap();
        let mut live_lan = NativeLiveLan::new(server.handle(), settings());
        // As at a start with no network, or on another network since.
        live_lan.url = "http://192.0.2.200:1/".into();
        assert_eq!(live_lan.status(true).url, "http://192.0.2.200:1/");
        live_lan.url_resolved = Instant::now().checked_sub(URL_REFRESH_INTERVAL).unwrap();
        assert_eq!(live_lan.status(true).url, viewer_url(server.address()));
        drop(live_lan);
        server.shutdown().unwrap();
    }
}
