//! Native realization of transport-controlled media-file inputs (IN-003).
//!
//! The engine owns every media input's transport ([`fm_engine::media`]): it
//! realizes play, pause, seek, trim, rate, end actions, and track changes on
//! exact frames and projects, for each frame, which source frame and audio
//! samples it presents (a [`MediaTick`]). This module decodes what those
//! ticks present. Each media-file input of a native project (except one a
//! Stinger plays clip-locally) owns one [`FilePlayer`] on its own worker
//! thread, opened at the program rate; the session loop never decodes.
//!
//! Before each frame is realized the loop hands every worker the tick the
//! engine projects for it ([`NativeMediaFiles::observe_frame`]). The worker
//! presents it ([`FilePlayer::present`]: the decoded picture and the audio
//! block of exactly that tick) and then decodes the next video window ahead
//! of need ([`FilePlayer::prefetch`]), so forward playback never waits on a
//! tool run at a window boundary. The loop collects finished frames
//! ([`NativeMediaFiles::take_completed`]): the picture is ingested as the
//! input's live source frame, re-dated onto the output timeline (the source
//! timestamps jump on every seek and loop), and the audio goes to the
//! Master as that frame's pushed audio, resampled to the Master's sample
//! count and mapped onto its layout there.
//!
//! [`NativeMediaFiles::covered`] holds a frame until every media input's
//! projected tick has been presented, at most [`MAX_MEDIA_FRAME_WAIT`] past
//! its host deadline or, when the loop runs late, past the moment it first
//! waited; a frame realized without it shows the input's previous picture
//! and silence, and is counted. A planned frame a growing source
//! cannot produce yet repeats the previous picture (`video_late`).
//!
//! A growing source is refreshed on every tick that waits at its live edge
//! and otherwise once a second, and each growth or completion is reported to
//! the engine ([`FileReport::Source`]), which extends the transport. An
//! operator's `complete_source` reaches the worker through the projected
//! frame's completion request. A failure (the file replaced, truncated, or
//! rewritten in place, or a window that cannot be decoded) faults the input
//! ([`FileReport::Fault`]); the engine stops its transport and every client
//! sees the typed fault, and the last picture stays on air.
//!
//! A media file must have the project's video dimensions, like every native
//! source; one that does not faults its input as unsupported.
//!
//! A closed input (IN-026) is not opened at startup and releases its decoder
//! when it closes; while closed it shows no picture (transparent black, so
//! black on its own and absent as a layer) and no sound, as the simulated
//! daemon shows nothing of a media input whose source is released. An input
//! whose file does not open at startup (missing, unsupported, or another
//! picture size) starts offline with the typed fault instead of stopping the
//! daemon, as a file that stops playing later does; it is retried when the
//! input is closed and reopened. A worker that panics faults its input
//! (`decode_failed`) and is no longer waited for.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::{
    Adapter, FilePlaybackConfig, FilePlaybackError, FilePlayer, GrowingFileConfig, SourceRefresh,
    StreamSelector,
};
use fm_engine::fm_playback::{FrameIndex, MediaState, MediaTick};
use fm_engine::{EngineMediaFault, EngineMediaSource, FrameResult};
use fm_frame::{
    AlphaMode, AudioBlock, ChromaLocation, ClockDomainId, ColorMetadata, ColorPrimaries,
    CpuVideoFrame, CpuVideoPayload, MatrixCoefficients, MediaTimestamp, MediaTiming,
    NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, SequenceNumber, SignalRange,
    TimeBase, TransferFunction, VideoFrameMetadata,
};
use fm_model::MediaPlaybackSettings;
use fm_types::{FrameRate, InputId, PixelFormat, VideoDimensions};

use crate::source_wait::SourceWait;

/// Longest a projected frame waits for a media input's tick to be presented
/// before it is realized with the input's previous picture.
pub(crate) const MAX_MEDIA_FRAME_WAIT: Duration = Duration::from_millis(100);

/// One media-file input a native daemon plays.
pub(crate) struct MediaFileInput {
    pub(crate) input: InputId,
    pub(crate) path: PathBuf,
    pub(crate) settings: MediaPlaybackSettings,
    /// The input is closed (IN-026): its file is not opened until it
    /// reopens.
    pub(crate) closed: bool,
}

/// What a worker reports to the engine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FileReport {
    /// The source now has `frames` frames, complete or still growing.
    Source { frames: u64, complete: bool },
    /// The source failed; its transport stops.
    Fault(EngineMediaFault),
    /// The file reopened as `source` (an input that reopened): the engine
    /// opens the transport on it unless it already is.
    Opened(EngineMediaSource),
}

/// One presented tick, collected by the loop.
pub(crate) struct Presented {
    pub(crate) input: InputId,
    /// The engine frame it is for.
    pub(crate) frame: u64,
    /// A new picture to ingest; `None` keeps the input's current one.
    pub(crate) video: Option<CpuVideoFrame>,
    pub(crate) audio: Option<AudioBlock>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Request {
    frame: u64,
    tick: MediaTick,
}

#[derive(Default)]
struct WorkerState {
    /// The latest tick to present; a newer one replaces an unserved one.
    request: Option<Request>,
    /// The engine asked to complete the growing source.
    complete: bool,
    /// The last presented tick not yet collected, latest-wins.
    completed: Option<(Request, Option<CpuVideoFrame>, Option<AudioBlock>)>,
    reports: Vec<FileReport>,
    /// The input is closed (IN-026): the worker releases its decoder, and
    /// reopens the file once this clears.
    closed: bool,
    /// How many times the input reopened. A close and reopen the worker did
    /// not wake for in between (it was presenting a tick) still reopens
    /// the file, which may have changed meanwhile.
    reopened: u64,
    shutdown: bool,
}

struct Shared {
    state: Mutex<WorkerState>,
    wake: Condvar,
    /// The worker panicked: it presents nothing more and is not waited for.
    panicked: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, WorkerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct Worker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    /// The tick last handed to the worker.
    requested: Option<Request>,
    /// The tick whose result the loop collected last.
    collected: Option<Request>,
    /// The input is closed: its worker presents nothing (IN-026).
    closed: bool,
    /// The input closed and its blank picture is still to be presented.
    blank_pending: bool,
    /// The latest engine frame a picture was presented for, so a blank
    /// picture is always dated after it.
    last_frame: Option<u64>,
}

impl Worker {
    fn panicked(&self) -> bool {
        self.shared.panicked.load(Ordering::Acquire)
    }
}

/// The media-file inputs of one native daemon and their workers.
pub(crate) struct NativeMediaFiles {
    workers: BTreeMap<InputId, Worker>,
    /// Each input's source as opened at startup, for the engine; an input
    /// closed or offline at startup has none.
    sources: Vec<(InputId, EngineMediaSource)>,
    /// Blank pictures of inputs that closed, collected with the presented
    /// ticks.
    blanks: Vec<Presented>,
    rate: FrameRate,
    clock_domain: ClockDomainId,
    dimensions: (u32, u32),
    /// How long the projected frame has waited for its media ticks.
    wait: SourceWait,
    /// The codec adapter workers open their files through; `None` until a
    /// daemon without media files at startup adds one.
    adapter: Option<Adapter>,
}

/// How one worker starts.
struct WorkerStart {
    /// The file as opened at startup, if it was.
    player: Option<FilePlayer>,
    /// Reports already due (a startup fault).
    reports: Vec<FileReport>,
    closed: bool,
    /// Added live: the worker opens the file itself first, as an input
    /// reopening does.
    open_live: bool,
}

/// The playback configuration of an input with `settings`.
fn playback_config(
    settings: &MediaPlaybackSettings,
    rate: FrameRate,
    clock_domain: ClockDomainId,
) -> FilePlaybackConfig {
    let mut config = FilePlaybackConfig::new(clock_domain, rate);
    config.audio = Some(
        settings
            .audio_stream
            .map_or(StreamSelector::Best, StreamSelector::Index),
    );
    config.growing = settings.growing.map(|growing| GrowingFileConfig {
        live_edge_offset_frames: u64::from(growing.live_edge_offset_frames),
        complete_after_idle_refreshes: NonZeroU32::new(growing.idle_completion_refreshes),
    });
    config
}

/// Starts one input's worker thread.
fn spawn_worker(
    adapter: Adapter,
    input: InputId,
    path: PathBuf,
    config: FilePlaybackConfig,
    start: WorkerStart,
    growing: bool,
    (rate, clock_domain, dimensions): (FrameRate, ClockDomainId, (u32, u32)),
) -> Result<Worker, String> {
    let WorkerStart {
        player,
        reports,
        closed,
        open_live,
    } = start;
    let shared = Arc::new(Shared {
        state: Mutex::new(WorkerState {
            reports,
            closed,
            ..WorkerState::default()
        }),
        wake: Condvar::new(),
        panicked: AtomicBool::new(false),
    });
    let worker_shared = Arc::clone(&shared);
    let thread = thread::Builder::new()
        .name(format!("freemix-media-file-{input}"))
        .spawn(move || {
            let reopen = || adapter.open_playback(&path, config);
            run_guarded(&worker_shared, || {
                run_worker(
                    &worker_shared,
                    player,
                    &reopen,
                    WorkerMode { growing, open_live },
                    rate,
                    clock_domain,
                    dimensions,
                );
            });
        })
        .map_err(|error| format!("native media input {input} worker: {error}"))?;
    Ok(Worker {
        shared,
        thread: Some(thread),
        requested: None,
        collected: None,
        closed,
        blank_pending: false,
        last_frame: None,
    })
}

/// Maps a playback failure onto the input fault clients see.
pub(crate) fn engine_fault(error: &FilePlaybackError) -> EngineMediaFault {
    use fm_codec_ffmpeg::Error;
    match error {
        FilePlaybackError::Ffmpeg(
            Error::InputNotFound | Error::InputAccessDenied | Error::InputNotRegularFile,
        ) => EngineMediaFault::SourceMissing,
        FilePlaybackError::Ffmpeg(Error::ToolUnavailable { .. }) => {
            EngineMediaFault::ToolUnavailable
        }
        FilePlaybackError::Ffmpeg(
            Error::Unsupported(_) | Error::MissingStream | Error::InvalidSelector,
        )
        | FilePlaybackError::InvalidConfig
        | FilePlaybackError::NoVideoFrames
        | FilePlaybackError::UnknownFrameRate
        | FilePlaybackError::VariableFrameRate { .. }
        | FilePlaybackError::InvalidIndex => EngineMediaFault::Unsupported,
        FilePlaybackError::SourceTruncated { .. } => EngineMediaFault::SourceTruncated,
        FilePlaybackError::SourceReplaced => EngineMediaFault::SourceReplaced,
        FilePlaybackError::SourceRewritten => EngineMediaFault::SourceRewritten,
        FilePlaybackError::Ffmpeg(Error::SourceChanged) | FilePlaybackError::SourceChanged => {
            EngineMediaFault::SourceChanged
        }
        FilePlaybackError::Ffmpeg(_)
        | FilePlaybackError::Control(_)
        | FilePlaybackError::SeekVerificationFailed { .. }
        | FilePlaybackError::FrameUnavailable { .. } => EngineMediaFault::DecodeFailed,
    }
}

/// The output-timeline timing of the picture shown on engine frame `frame`
/// (see [`timing_at`]): sequence `frame + 1`, so the startup picture is
/// sequence zero.
pub(crate) fn output_timing(
    frame: u64,
    rate: FrameRate,
    clock_domain: ClockDomainId,
) -> Option<MediaTiming> {
    timing_at(frame.checked_add(1)?, rate, clock_domain)
}

/// Sequence `sequence` on the program frame grid, one frame long.
pub(crate) fn timing_at(
    sequence: u64,
    rate: FrameRate,
    clock_domain: ClockDomainId,
) -> Option<MediaTiming> {
    let duration = u64::try_from(
        u128::from(rate.denominator()) * 1_000_000_000 / u128::from(rate.numerator()).max(1),
    )
    .ok()?
    .max(1);
    let pts = i64::try_from(sequence.checked_mul(duration)?).ok()?;
    MediaTiming::new(
        OriginalTimestamp::new(
            MediaTimestamp::new(pts),
            TimeBase::new(1, 1_000_000_000).ok()?,
        ),
        NormalizedTimestamp::from_nanos(pts),
        NormalizedDuration::from_nanos(duration).ok()?,
        clock_domain,
        SequenceNumber::new(sequence),
    )
    .ok()
}

/// The picture of an input that shows none (closed, or offline at
/// startup): transparent black at `dimensions`, dated `timing`.
pub(crate) fn blank_picture(dimensions: (u32, u32), timing: MediaTiming) -> Option<CpuVideoFrame> {
    let size = VideoDimensions::new(dimensions.0, dimensions.1)?;
    let payload = CpuVideoPayload::allocate(PixelFormat::Rgba8, size).ok()?;
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
        .ok()
}

/// What the engine knows of `player`'s source.
fn engine_source(player: &FilePlayer, growing: bool) -> EngineMediaSource {
    EngineMediaSource::external(
        player.frame_rate(),
        player.frame_count(),
        !growing,
        player.audio_tracks().to_vec(),
    )
}

/// Opens one input's file at startup and decodes its startup picture (the
/// frame its transport cues), which must have the project's `dimensions`.
fn open_at_startup(
    adapter: &Adapter,
    path: &std::path::Path,
    config: FilePlaybackConfig,
    settings: &MediaPlaybackSettings,
    dimensions: (u32, u32),
) -> Result<(FilePlayer, Arc<CpuVideoFrame>), EngineMediaFault> {
    let mut player = adapter
        .open_playback(path, config)
        .map_err(|error| engine_fault(&error))?;
    let cue = settings.start_frame.max(settings.trim_in);
    let last = player.controller().playable_end().get().saturating_sub(1);
    let seed = player
        .decode_frame(FrameIndex::new(cue.min(last)))
        .map_err(|error| engine_fault(&error))?;
    let size = seed.payload().dimensions();
    if (size.width(), size.height()) != dimensions {
        return Err(EngineMediaFault::Unsupported);
    }
    Ok((player, seed))
}

/// Runs `body` (a worker), faulting its input (`decode_failed`) if it
/// panics, so the input goes offline and no frame waits for it.
fn run_guarded(shared: &Shared, body: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(body)).is_err() {
        shared.panicked.store(true, Ordering::Release);
        let mut state = shared.lock();
        state.completed = None;
        state
            .reports
            .push(FileReport::Fault(EngineMediaFault::DecodeFailed));
    }
}

/// `frame` re-dated onto the output timeline.
fn redated(frame: &CpuVideoFrame, timing: MediaTiming) -> Option<CpuVideoFrame> {
    let moved = CpuVideoFrame::new(timing, frame.payload().clone());
    match frame.metadata() {
        Some(metadata) => moved.with_metadata(metadata).ok(),
        None => Some(moved),
    }
}

impl NativeMediaFiles {
    /// Opens every open input's file, decodes its startup picture (the
    /// frame its transport cues), and starts its worker. Returns the startup
    /// pictures to seed the source registry with, dated sequence zero. A
    /// closed input is not opened, and one whose file does not open starts
    /// offline with its fault reported; both start with a blank picture.
    ///
    /// # Errors
    ///
    /// Names an input whose startup picture cannot be timed or whose worker
    /// cannot be started.
    #[allow(clippy::too_many_lines)] // one input after another: open, seed, worker
    pub(crate) fn start(
        adapter: &Adapter,
        inputs: Vec<MediaFileInput>,
        rate: FrameRate,
        clock_domain: ClockDomainId,
        dimensions: (u32, u32),
    ) -> Result<(Self, Vec<(InputId, CpuVideoFrame)>), String> {
        let mut workers = BTreeMap::new();
        let mut sources = Vec::with_capacity(inputs.len());
        let mut seeds = Vec::with_capacity(inputs.len());
        for MediaFileInput {
            input,
            path,
            settings,
            closed,
        } in inputs
        {
            let config = playback_config(&settings, rate, clock_domain);
            let growing = settings.growing.is_some();
            let seed_timing = timing_at(0, rate, clock_domain)
                .ok_or_else(|| format!("native media input {input} cannot be timed"))?;
            let opened = if closed {
                None
            } else {
                Some(open_at_startup(
                    adapter, &path, config, &settings, dimensions,
                ))
            };
            let mut reports = Vec::new();
            let (player, seed) = match opened {
                Some(Ok((player, seed))) => {
                    sources.push((input, engine_source(&player, growing)));
                    (Some(player), redated(&seed, seed_timing))
                }
                Some(Err(fault)) => {
                    eprintln!(
                        "FREEMIXD_MEDIA\tv=1\tinput={input}\tstartup_fault={}",
                        fault.token()
                    );
                    reports.push(FileReport::Fault(fault));
                    (None, blank_picture(dimensions, seed_timing))
                }
                None => (None, blank_picture(dimensions, seed_timing)),
            };
            seeds.push((
                input,
                seed.ok_or_else(|| format!("native media input {input} cannot be timed"))?,
            ));
            let worker = spawn_worker(
                adapter.clone(),
                input,
                path,
                config,
                WorkerStart {
                    player,
                    reports,
                    closed,
                    open_live: false,
                },
                growing,
                (rate, clock_domain, dimensions),
            )?;
            workers.insert(input, worker);
        }
        Ok((
            Self {
                workers,
                sources,
                blanks: Vec::new(),
                rate,
                clock_domain,
                dimensions,
                wait: SourceWait::new(MAX_MEDIA_FRAME_WAIT),
                adapter: Some(adapter.clone()),
            },
            seeds,
        ))
    }

    /// No media-file inputs (a project without local video). One added
    /// live opens through an adapter made then.
    pub(crate) fn start_empty(
        rate: FrameRate,
        clock_domain: ClockDomainId,
        dimensions: (u32, u32),
    ) -> (Self, Vec<(InputId, CpuVideoFrame)>) {
        (
            Self {
                workers: BTreeMap::new(),
                sources: Vec::new(),
                blanks: Vec::new(),
                rate,
                clock_domain,
                dimensions,
                wait: SourceWait::new(MAX_MEDIA_FRAME_WAIT),
                adapter: None,
            },
            Vec::new(),
        )
    }

    /// Adds media-file input `input` while the daemon runs (IN-026) and
    /// returns its seed picture: transparent black, sequence zero, held
    /// until its worker presents the file. The file is opened on the
    /// worker, never here: the worker reports it ([`FileReport::Opened`],
    /// and the engine opens the transport on it) or its fault
    /// ([`FileReport::Fault`], and the input goes offline), exactly as an
    /// input reopening does. A closed input opens once it reopens.
    /// `adapter` makes the codec adapter when none exists yet; without one
    /// the input faults as `tool_unavailable`.
    pub(crate) fn add_live(
        &mut self,
        input: MediaFileInput,
        adapter: impl FnOnce() -> Option<Adapter>,
    ) -> Result<CpuVideoFrame, String> {
        let MediaFileInput {
            input,
            path,
            settings,
            closed,
        } = input;
        self.remove(input);
        if self.adapter.is_none() {
            self.adapter = adapter();
        }
        let seed = timing_at(0, self.rate, self.clock_domain)
            .and_then(|timing| blank_picture(self.dimensions, timing))
            .ok_or_else(|| format!("native media input {input} cannot be timed"))?;
        let config = playback_config(&settings, self.rate, self.clock_domain);
        let growing = settings.growing.is_some();
        let worker = if let Some(adapter) = self.adapter.clone() {
            spawn_worker(
                adapter,
                input,
                path,
                config,
                WorkerStart {
                    player: None,
                    reports: Vec::new(),
                    closed,
                    open_live: true,
                },
                growing,
                (self.rate, self.clock_domain, self.dimensions),
            )?
        } else {
            eprintln!(
                "FREEMIXD_MEDIA\tv=1\tinput={input}\tstartup_fault={}",
                EngineMediaFault::ToolUnavailable.token()
            );
            Worker {
                shared: Arc::new(Shared {
                    state: Mutex::new(WorkerState {
                        reports: vec![FileReport::Fault(EngineMediaFault::ToolUnavailable)],
                        closed,
                        ..WorkerState::default()
                    }),
                    wake: Condvar::new(),
                    panicked: AtomicBool::new(true),
                }),
                thread: None,
                requested: None,
                collected: None,
                closed,
                blank_pending: false,
                last_frame: None,
            }
        };
        self.workers.insert(input, worker);
        Ok(seed)
    }

    /// Removes media-file input `input` (an input removed, or given another
    /// source, live): its worker stops and is joined off the session loop,
    /// and nothing more of it is presented or reported. Returns whether it
    /// had a worker.
    pub(crate) fn remove(&mut self, input: InputId) -> bool {
        self.sources.retain(|(candidate, _)| *candidate != input);
        self.blanks.retain(|blank| blank.input != input);
        let Some(mut worker) = self.workers.remove(&input) else {
            return false;
        };
        worker.shared.lock().shutdown = true;
        worker.shared.wake.notify_one();
        if let Some(thread) = worker.thread.take() {
            // Bounded by one in-flight tool run, which the loop never
            // waits for.
            let _ = thread::Builder::new()
                .name(format!("freemix-media-file-retire-{input}"))
                .spawn(move || {
                    let _ = thread.join();
                });
        }
        true
    }

    /// Every input's source as opened at startup, for
    /// [`fm_control::ControlService::set_media_source`].
    pub(crate) fn sources(&self) -> &[(InputId, EngineMediaSource)] {
        &self.sources
    }

    /// Input `input`'s source as opened at startup, if it opened then.
    pub(crate) fn source(&self, input: InputId) -> Option<&EngineMediaSource> {
        self.sources
            .iter()
            .find(|(candidate, _)| *candidate == input)
            .map(|(_, source)| source)
    }

    /// Every media-file input, in input order.
    pub(crate) fn inputs(&self) -> Vec<InputId> {
        self.workers.keys().copied().collect()
    }

    /// Whether input `input`'s worker panicked: it decodes nothing more.
    pub(crate) fn is_failed(&self, input: InputId) -> bool {
        self.workers.get(&input).is_some_and(Worker::panicked)
    }

    /// Hands each worker the tick the engine projects for its input on
    /// `projected` (and a completion the engine requested). Never decodes.
    pub(crate) fn observe_frame(&mut self, projected: &FrameResult) {
        let frame = projected.frame.get();
        self.present_blanks(frame);
        for media in &projected.media {
            let Some(worker) = self.workers.get_mut(&media.input) else {
                continue;
            };
            if worker.closed || worker.panicked() {
                continue;
            }
            let request = media.tick.clone().map(|tick| Request { frame, tick });
            let changed = request.is_some() && worker.requested != request;
            if !changed && !media.completion_requested {
                continue;
            }
            let mut state = worker.shared.lock();
            if changed {
                state.request.clone_from(&request);
                worker.requested = request;
            }
            state.complete |= media.completion_requested;
            drop(state);
            worker.shared.wake.notify_one();
        }
    }

    /// Presents the blank picture of every input that closed since its last
    /// picture, for engine frame `frame`, once `frame` is after that picture
    /// (the source registry only takes pictures that move forward).
    fn present_blanks(&mut self, frame: u64) {
        for (&input, worker) in &mut self.workers {
            if !worker.closed
                || !worker.blank_pending
                || worker.last_frame.is_some_and(|last| frame <= last)
            {
                continue;
            }
            worker.blank_pending = false;
            worker.last_frame = Some(frame);
            if let Some(video) = output_timing(frame, self.rate, self.clock_domain)
                .and_then(|timing| blank_picture(self.dimensions, timing))
            {
                self.blanks.push(Presented {
                    input,
                    frame,
                    video: Some(video),
                    audio: None,
                });
            }
        }
    }

    /// Whether `projected`, due at host instant `deadline`, may be realized:
    /// every media input's projected tick has been presented and collected,
    /// or the frame waited for it long enough ([`SourceWait::waited_out`]).
    pub(crate) fn covered(&mut self, projected: &FrameResult, deadline: Instant) -> bool {
        let frame = projected.frame.get();
        let pending = projected.media.iter().any(|media| {
            self.workers.get(&media.input).is_some_and(|worker| {
                !worker.closed
                    && !worker.panicked()
                    && media.tick.as_ref().is_some_and(|tick| {
                        worker.collected.as_ref().is_none_or(|collected| {
                            collected.frame != frame || collected.tick != *tick
                        })
                    })
            })
        });
        !pending || self.wait.waited_out(frame, deadline, Instant::now())
    }

    /// Takes every presented tick not yet collected, and the blank picture
    /// of every input that closed, in input order. A tick a closed input's
    /// worker finished after it closed is dropped, and a picture never
    /// repeats or precedes the frame of one already taken (the source
    /// registry refuses a picture that does not move forward).
    pub(crate) fn take_completed(&mut self) -> Vec<Presented> {
        let mut presented = Vec::new();
        for (&input, worker) in &mut self.workers {
            let taken = worker.shared.lock().completed.take();
            let Some((request, mut video, audio)) = taken else {
                continue;
            };
            if worker.closed {
                continue;
            }
            if worker.last_frame.is_some_and(|last| request.frame <= last) {
                video = None;
            } else if video.is_some() {
                worker.last_frame = Some(request.frame);
            }
            presented.push(Presented {
                input,
                frame: request.frame,
                video,
                audio,
            });
            worker.collected = Some(request);
        }
        presented.append(&mut self.blanks);
        presented
    }

    /// Closes (`true`) or reopens (`false`) media-file input `input`
    /// (IN-026): a closed input's worker releases its decoder, and reopens
    /// the file when the input reopens. Returns whether the input has a
    /// worker.
    pub(crate) fn set_closed(&mut self, input: InputId, closed: bool) -> bool {
        let Some(worker) = self.workers.get_mut(&input) else {
            return false;
        };
        if worker.closed != closed {
            worker.closed = closed;
            worker.blank_pending = closed;
            worker.requested = None;
            worker.collected = None;
            if !closed {
                self.blanks.retain(|blank| blank.input != input);
            }
        }
        let mut state = worker.shared.lock();
        if state.closed != closed {
            state.closed = closed;
            if !closed {
                state.reopened = state.reopened.wrapping_add(1);
            }
            state.request = None;
            drop(state);
            worker.shared.wake.notify_one();
        }
        true
    }

    /// Whether media-file input `input`'s worker is closed (IN-026).
    pub(crate) fn is_closed(&self, input: InputId) -> bool {
        self.workers.get(&input).is_some_and(|worker| worker.closed)
    }

    /// Takes every worker's reports for the engine, in input order.
    pub(crate) fn take_reports(&self) -> Vec<(InputId, FileReport)> {
        let mut reports = Vec::new();
        for (&input, worker) in &self.workers {
            let taken = std::mem::take(&mut worker.shared.lock().reports);
            reports.extend(taken.into_iter().map(|report| (input, report)));
        }
        reports
    }

    /// Frames realized before a media input's tick was presented.
    pub(crate) const fn late_frames(&self) -> u64 {
        self.wait.late_frames()
    }
}

impl Drop for NativeMediaFiles {
    fn drop(&mut self) {
        for worker in self.workers.values_mut() {
            worker.shared.lock().shutdown = true;
            worker.shared.wake.notify_one();
        }
        for worker in self.workers.values_mut() {
            if let Some(thread) = worker.thread.take() {
                // Bounded by one in-flight tool run.
                let _ = thread.join();
            }
        }
    }
}

/// Follows a growing source: completes it on request, and refreshes it
/// while the transport waits at the live edge or once a second. Returns
/// what changed, if anything.
fn follow_growing_source(
    player: &mut FilePlayer,
    complete: bool,
    waiting: bool,
    next_refresh: &mut Instant,
) -> Option<FileReport> {
    let refresh = if complete {
        player.complete_source()
    } else if waiting || Instant::now() >= *next_refresh {
        *next_refresh = Instant::now() + Duration::from_secs(1);
        player.refresh()
    } else {
        return None;
    };
    match refresh {
        Ok(SourceRefresh::Grew { available_frames }) => Some(FileReport::Source {
            frames: available_frames,
            complete: false,
        }),
        Ok(SourceRefresh::Completed { frames }) => Some(FileReport::Source {
            frames,
            complete: true,
        }),
        Ok(SourceRefresh::Unchanged) => None,
        Err(error) => Some(FileReport::Fault(engine_fault(&error))),
    }
}

/// How a worker follows its file.
#[derive(Clone, Copy)]
struct WorkerMode {
    growing: bool,
    /// Added live: open the file before anything else.
    open_live: bool,
}

/// Runs one input's worker. A picture that is not `dimensions` (a file
/// reopened after it changed on disk) faults the input as unsupported
/// instead of reaching the source registry, which would fail the session.
#[allow(clippy::too_many_lines)] // one worker loop: close, reopen, present
fn run_worker(
    shared: &Shared,
    mut opened: Option<FilePlayer>,
    reopen: &dyn Fn() -> Result<FilePlayer, FilePlaybackError>,
    WorkerMode { growing, open_live }: WorkerMode,
    rate: FrameRate,
    clock_domain: ClockDomainId,
    dimensions: (u32, u32),
) {
    let second = Duration::from_secs(1);
    let mut next_refresh = Instant::now() + second;
    // Closed at startup: nothing is open until the input reopens. Open but
    // without a player: the file did not open, and the input is offline.
    // Added live: the file opens first, as on reopening.
    let mut released = shared.lock().closed || open_live;
    let mut reopened = shared.lock().reopened;
    let mut faulted = opened.is_none() && !released;
    let mut last_picture: Option<Arc<CpuVideoFrame>> = None;
    loop {
        let (request, complete) = {
            let mut state = shared.lock();
            loop {
                if state.shutdown {
                    return;
                }
                if state.closed != released || state.reopened != reopened {
                    break;
                }
                if state.closed {
                    state = shared
                        .wake
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                    continue;
                }
                if state.request.is_some() || state.complete {
                    break;
                }
                let now = Instant::now();
                if growing && !faulted && now >= next_refresh {
                    break;
                }
                state = if growing && !faulted {
                    shared
                        .wake
                        .wait_timeout(state, next_refresh.saturating_duration_since(now))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                } else {
                    shared
                        .wake
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner)
                };
            }
            if state.closed != released || state.reopened != reopened {
                released = state.closed;
                reopened = state.reopened;
                state.request = None;
                state.completed = None;
                if released {
                    // Closing releases the decoder and its tool processes.
                    drop(state);
                    opened = None;
                    last_picture = None;
                    continue;
                }
                // Reopening opens the file again; a file that no longer
                // opens takes the input offline through its fault.
                drop(state);
                opened = None;
                last_picture = None;
                match reopen() {
                    Ok(player) => {
                        shared
                            .lock()
                            .reports
                            .push(FileReport::Opened(engine_source(&player, growing)));
                        opened = Some(player);
                        faulted = false;
                    }
                    Err(error) => {
                        faulted = true;
                        shared
                            .lock()
                            .reports
                            .push(FileReport::Fault(engine_fault(&error)));
                    }
                }
                continue;
            }
            (state.request.take(), std::mem::take(&mut state.complete))
        };
        let Some(player) = opened.as_mut() else {
            // A file that did not reopen presents nothing.
            if let Some(request) = request {
                shared.lock().completed = Some((request, None, None));
            }
            continue;
        };
        let mut reports = Vec::new();
        if !faulted && growing {
            let waiting = request
                .as_ref()
                .is_some_and(|request| request.tick.state == MediaState::Waiting);
            if let Some(report) =
                follow_growing_source(player, complete, waiting, &mut next_refresh)
            {
                faulted = matches!(report, FileReport::Fault(_));
                reports.push(report);
            }
        }
        let mut completed = None;
        if let Some(request) = request {
            let (video, audio) = if faulted {
                (None, None)
            } else {
                match player.present(request.tick.clone()) {
                    // A picture of another size, or one the GPU import
                    // refuses (no colour metadata), would fail the shared
                    // source registry: it faults this input instead.
                    Ok(output)
                        if output.video.as_ref().is_some_and(|picture| {
                            let size = picture.payload().dimensions();
                            (size.width(), size.height()) != dimensions
                                || fm_color::validate_native_import(picture).is_err()
                        }) =>
                    {
                        faulted = true;
                        reports.push(FileReport::Fault(EngineMediaFault::Unsupported));
                        (None, None)
                    }
                    Ok(output) => {
                        let fresh = output.video.filter(|picture| {
                            last_picture
                                .as_ref()
                                .is_none_or(|last| !Arc::ptr_eq(last, picture))
                        });
                        let video = fresh.and_then(|picture| {
                            let timing = output_timing(request.frame, rate, clock_domain)?;
                            let moved = redated(&picture, timing);
                            last_picture = Some(picture);
                            moved
                        });
                        (video, output.audio)
                    }
                    Err(error) => {
                        faulted = true;
                        reports.push(FileReport::Fault(engine_fault(&error)));
                        (None, None)
                    }
                }
            };
            completed = Some((request, video, audio));
        }
        {
            let mut state = shared.lock();
            if let Some((request, mut video, audio)) = completed {
                // A picture presented for a tick the loop never collected is
                // still the newest picture when this tick repeats it.
                if video.is_none()
                    && let Some((_, unseen, _)) = state.completed.take()
                {
                    video = unseen;
                }
                state.completed = Some((request, video, audio));
            }
            state.reports.extend(reports);
        }
        // Decode the next window while the loop realizes this frame.
        if !faulted && let Err(error) = player.prefetch() {
            faulted = true;
            shared
                .lock()
                .reports
                .push(FileReport::Fault(engine_fault(&error)));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU128;

    use super::*;

    fn input() -> InputId {
        InputId::new(NonZeroU128::new(7).unwrap())
    }

    /// One media-file input whose worker never runs, last presented on
    /// engine frame 5.
    fn files() -> NativeMediaFiles {
        let (mut files, _) = NativeMediaFiles::start_empty(
            FrameRate::new(25, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
            (32, 18),
        );
        files.workers.insert(
            input(),
            Worker {
                shared: Arc::new(Shared {
                    state: Mutex::new(WorkerState::default()),
                    wake: Condvar::new(),
                    panicked: AtomicBool::new(false),
                }),
                thread: None,
                requested: None,
                collected: None,
                closed: false,
                blank_pending: false,
                last_frame: Some(5),
            },
        );
        files
    }

    #[test]
    fn a_closed_input_presents_one_blank_picture_after_its_last_one() {
        let mut files = files();
        assert!(files.set_closed(input(), true));
        // Not on the frame its last picture was presented for.
        files.present_blanks(5);
        assert!(files.take_completed().is_empty());
        files.present_blanks(6);
        let presented = files.take_completed();
        assert_eq!(presented.len(), 1);
        let blank = &presented[0];
        assert_eq!((blank.input, blank.frame), (input(), 6));
        assert!(blank.audio.is_none());
        let video = blank.video.as_ref().expect("a blank picture");
        assert_eq!(video.timing().sequence().get(), 7);
        let size = video.payload().dimensions();
        assert_eq!((size.width(), size.height()), (32, 18));
        assert!(
            video
                .payload()
                .planes()
                .iter()
                .all(|plane| plane.bytes().iter().all(|byte| *byte == 0)),
            "transparent black"
        );
        // Once.
        files.present_blanks(7);
        assert!(files.take_completed().is_empty());
    }

    #[test]
    fn a_tick_finished_after_close_is_dropped() {
        let mut files = files();
        files.set_closed(input(), true);
        let shared = Arc::clone(&files.workers[&input()].shared);
        shared.lock().completed = Some((
            Request {
                frame: 9,
                tick: MediaTick {
                    output_tick: 9,
                    frame: None,
                    repeated: false,
                    dropped_frames: 0,
                    discontinuity: false,
                    state: MediaState::Playing,
                    audio: fm_engine::fm_playback::AudioPlan::None,
                    events: Vec::new(),
                },
            },
            None,
            None,
        ));
        assert!(files.take_completed().is_empty());
        // Reopened, a pending blank is not presented over the file.
        files.set_closed(input(), false);
        files.present_blanks(10);
        assert!(files.take_completed().is_empty());
    }

    /// A frame on time waits for its tick until [`MAX_MEDIA_FRAME_WAIT`]
    /// past its deadline, as before.
    #[test]
    fn an_on_time_frame_waits_for_its_tick_until_past_its_deadline() {
        let mut files = files();
        let early = Instant::now();
        let deadline = early + Duration::from_millis(30);
        assert!(!files.wait.waited_out(9, deadline, early));
        assert!(
            !files
                .wait
                .waited_out(9, deadline, deadline + Duration::from_millis(99))
        );
        assert!(
            files
                .wait
                .waited_out(9, deadline, deadline + MAX_MEDIA_FRAME_WAIT)
        );
        assert_eq!(files.late_frames(), 1);
    }

    /// A loop running seconds behind (an overloaded host) still gives each
    /// frame's media tick time to be presented, so its audio is mixed
    /// instead of arriving after the frame and being dropped; a tick that
    /// never comes still releases the frame, counted once.
    #[test]
    fn a_late_frame_still_waits_for_its_tick() {
        let mut files = files();
        let deadline = Instant::now();
        let now = deadline + Duration::from_secs(8);
        assert!(
            !files.wait.waited_out(9, deadline, now),
            "waits on its first check"
        );
        assert!(
            !files
                .wait
                .waited_out(9, deadline, now + Duration::from_millis(50))
        );
        assert!(
            files
                .wait
                .waited_out(9, deadline, now + MAX_MEDIA_FRAME_WAIT)
        );
        assert!(
            files
                .wait
                .waited_out(9, deadline, now + MAX_MEDIA_FRAME_WAIT * 2)
        );
        assert_eq!(files.late_frames(), 1);
        // The next late frame gets its own wait.
        let later = now + MAX_MEDIA_FRAME_WAIT * 2;
        assert!(!files.wait.waited_out(10, deadline, later));
        assert!(
            files
                .wait
                .waited_out(10, deadline, later + MAX_MEDIA_FRAME_WAIT)
        );
        assert_eq!(files.late_frames(), 2);
    }

    #[test]
    fn a_panicking_worker_faults_its_input_and_is_not_waited_for() {
        let files = files();
        let shared = Arc::clone(&files.workers[&input()].shared);
        run_guarded(&shared, || panic!("decoder bug"));
        assert!(files.is_failed(input()));
        assert_eq!(
            files.take_reports(),
            [(input(), FileReport::Fault(EngineMediaFault::DecodeFailed))]
        );
    }
}
