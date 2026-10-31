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
//! An audio-only file (a WAV, MP3, AAC/M4A, FLAC, or Ogg music bed or sting;
//! IN-005) plays through the same transport: its frames are synthesized at
//! the program rate and every one shows one transparent-black picture of
//! the project's dimensions, so the input is black on Program on its own and
//! absent as a layer, while its audio reaches the Master through its strip
//! like any clip's. The shared picture is ingested once, not per frame.
//!
//! A media list (IN-006) keeps a standby of its next item: the file opened,
//! indexed, and cued at the item's in mark on its own thread
//! ([`NativeMediaFiles::prefetch`]). When the list steps to that item the
//! input's worker is replaced by one that plays the cued player
//! ([`NativeMediaFiles::adopt`]) before the step's frame is projected, so
//! the switch frame presents the new item instead of holding the last one.
//!
//! A list's transition between items (IN-006, Protocol 2.95) composites
//! the two items on the CPU. The replaced worker is not retired at once: it
//! becomes the input's *outgoing* worker, presenting the outgoing item's
//! ticks the engine projects ([`fm_engine::EngineMediaOutgoing`]) into a
//! hand-off slot instead of the source registry. The incoming worker blends
//! its picture with the outgoing one of the same frame (waiting at most
//! [`MAX_OUTGOING_WAIT`] for it, else using its latest) with
//! [`fm_video::blend_items`], and mixes the two items' sound with the
//! transition's gains, so the input stays one source. The outgoing worker
//! retires on the first frame without a transition. A step to an item of
//! the file already open has no second decoder: the outgoing item holds
//! the last picture presented before the step, silent. A step whose next
//! file was not opened ahead in time (its standby still opening) cuts to
//! it once the file opens.
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
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
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
    CpuVideoFrame, CpuVideoPayload, CpuVideoPlane, MatrixCoefficients, MediaTimestamp, MediaTiming,
    NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, SequenceNumber, SignalRange,
    TimeBase, TransferFunction, VideoFrameMetadata,
};
use fm_model::MediaPlaybackSettings;
use fm_types::{FrameRate, InputId, PixelFormat, VideoDimensions};

use crate::source_wait::SourceWait;

/// Longest a projected frame waits for a media input's tick to be presented
/// before it is realized with the input's previous picture.
pub(crate) const MAX_MEDIA_FRAME_WAIT: Duration = Duration::from_millis(100);

/// Most media list standbys (IN-006) opening their files at once; the
/// others wait their turn on their own threads.
pub(crate) const MAX_CONCURRENT_PREFETCHES: usize = 2;

/// Longest an incoming item's worker waits for the outgoing item's picture
/// of the same frame during a list transition before it blends the
/// outgoing item's latest one.
pub(crate) const MAX_OUTGOING_WAIT: Duration = Duration::from_millis(40);

/// Bounds how many standbys open their files at once.
#[derive(Default)]
pub(crate) struct PrefetchGate {
    opening: Mutex<usize>,
    freed: Condvar,
}

/// One standby's turn to open its file; the turn passes on when dropped.
pub(crate) struct PrefetchPermit<'g>(&'g PrefetchGate);

impl Drop for PrefetchPermit<'_> {
    fn drop(&mut self) {
        let mut opening = self
            .0
            .opening
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *opening = opening.saturating_sub(1);
        self.0.freed.notify_all();
    }
}

impl PrefetchGate {
    /// Waits for a turn among at most `max` open at once; `None` once
    /// `cancelled` is set (the standby was dropped while it waited).
    pub(crate) fn acquire(&self, max: usize, cancelled: &AtomicBool) -> Option<PrefetchPermit<'_>> {
        let mut opening = self.opening.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if cancelled.load(Ordering::Acquire) {
                return None;
            }
            if *opening < max {
                *opening += 1;
                return Some(PrefetchPermit(self));
            }
            // A dropped standby sets its flag without the lock; the
            // timeout bounds how late its waiter notices.
            opening = self
                .freed
                .wait_timeout(opening, Duration::from_millis(100))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Wakes every waiter to look at its cancellation again.
    fn wake(&self) {
        self.freed.notify_all();
    }
}

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
    /// A list transition composites this tick's picture over the outgoing
    /// item's (IN-006).
    blend: Option<Blend>,
}

/// How one frame of a media list transition composites its two items.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Blend {
    kind: fm_video::ItemTransition,
    /// The incoming item's weight, in basis points.
    weight: u16,
}

/// What an outgoing worker presented last, for the incoming one.
#[derive(Clone)]
struct Handoff {
    frame: u64,
    picture: Option<Arc<CpuVideoFrame>>,
    audio: Option<AudioBlock>,
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // independent worker flags
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
    /// The worker plays a list's outgoing item (IN-006): it presents into
    /// `handoff`, never to the loop.
    outgoing: bool,
    handoff: Option<Handoff>,
}

struct Shared {
    state: Mutex<WorkerState>,
    wake: Condvar,
    /// The worker panicked: it presents nothing more and is not waited for.
    panicked: AtomicBool,
    /// An outgoing worker handed off a picture, or retired.
    handed_off: Condvar,
}

impl Shared {
    fn new(state: WorkerState, panicked: bool) -> Self {
        Self {
            state: Mutex::new(state),
            wake: Condvar::new(),
            panicked: AtomicBool::new(panicked),
            handed_off: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, WorkerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What outgoing worker `self` presented for engine frame `frame`, or
    /// its latest once [`MAX_OUTGOING_WAIT`] passes; `None` once it retired
    /// (or never presented).
    fn handoff_for(&self, frame: u64) -> Option<Handoff> {
        let deadline = Instant::now() + MAX_OUTGOING_WAIT;
        let mut state = self.lock();
        loop {
            if state.shutdown || !state.outgoing || self.panicked.load(Ordering::Acquire) {
                return None;
            }
            if state
                .handoff
                .as_ref()
                .is_some_and(|handoff| handoff.frame >= frame)
            {
                return state.handoff.clone();
            }
            let now = Instant::now();
            if now >= deadline {
                return state.handoff.clone();
            }
            state = self
                .handed_off
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
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
    /// An outgoing worker's latest engine frame and transition step, so a
    /// transition that restarts without it (a re-cue of the incoming file)
    /// retires it.
    outgoing_step: Option<(u64, u32)>,
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
    /// Each media list's neighbouring items, opened and cued ahead of need
    /// (IN-006), by list input and role.
    standbys: BTreeMap<(InputId, PrefetchRole), Standby>,
    /// Bounds how many standbys open their files at once.
    prefetch_gate: Arc<PrefetchGate>,
    /// Each media list's outgoing item during a transition (IN-006): the
    /// worker a step replaced, still presenting.
    outgoing: BTreeMap<InputId, Worker>,
}

/// Which neighbour of a media list's active item a standby opens: the item
/// Auto Next or Next steps to, or the one Previous steps to (IN-006).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum PrefetchRole {
    Next,
    Previous,
}

/// A media list's neighbouring item file, opened, indexed, and cued on its
/// own thread ahead of need (IN-006): its in-mark picture and first audio
/// block are decoded and the window after it too, so the list switches to
/// it on the switch frame instead of holding the item before while the
/// file opens.
struct Standby {
    path: PathBuf,
    /// The source frame it is cued at (the item's in mark).
    cue: u64,
    config: FilePlaybackConfig,
    result: mpsc::Receiver<Result<Prefetched, EngineMediaFault>>,
    /// Set when the standby is dropped, so a thread still waiting for its
    /// turn to open the file gives up.
    cancelled: Arc<AtomicBool>,
    gate: Arc<PrefetchGate>,
}

impl Drop for Standby {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.gate.wake();
    }
}

/// A file a [`Standby`] opened and cued.
pub(crate) struct Prefetched {
    player: FilePlayer,
    config: FilePlaybackConfig,
    growing: bool,
    /// What the engine opens the transport on.
    pub(crate) source: EngineMediaSource,
}

/// Opens `path` and decodes what playing it from `cue` presents first (the
/// picture, which must be `dimensions`, and the first audio block), then
/// the video window after it, so a worker that takes the player presents
/// its first tick from memory.
fn open_cued(
    adapter: &Adapter,
    path: &std::path::Path,
    config: FilePlaybackConfig,
    cue: u64,
    dimensions: (u32, u32),
) -> Result<Prefetched, EngineMediaFault> {
    let growing = config.growing.is_some();
    let mut player = adapter
        .open_playback(path, config)
        .map_err(|error| engine_fault(&error))?;
    let last = player.controller().playable_end().get().saturating_sub(1);
    let controller = player.controller_mut();
    controller
        .seek(FrameIndex::new(cue.min(last)))
        .map_err(|_| EngineMediaFault::Unsupported)?;
    controller.play();
    let first = player.tick().map_err(|error| engine_fault(&error))?;
    let fits = first.video.as_ref().is_some_and(|picture| {
        let size = picture.payload().dimensions();
        (size.width(), size.height()) == dimensions
            && fm_color::validate_native_import(picture).is_ok()
    });
    if !fits {
        return Err(EngineMediaFault::Unsupported);
    }
    player.prefetch().map_err(|error| engine_fault(&error))?;
    Ok(Prefetched {
        source: engine_source(&player, growing),
        player,
        config,
        growing,
    })
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
    /// The outgoing worker of a list transition this worker blends with
    /// (IN-006).
    outgoing: Option<Arc<Shared>>,
}

/// The playback configuration of an input with `settings`. An audio-only
/// file presents a transparent picture of the project's `dimensions`.
fn playback_config(
    settings: &MediaPlaybackSettings,
    rate: FrameRate,
    clock_domain: ClockDomainId,
    dimensions: (u32, u32),
) -> FilePlaybackConfig {
    let mut config = FilePlaybackConfig::new(clock_domain, rate);
    config.audio_only_picture = VideoDimensions::new(dimensions.0, dimensions.1);
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
        outgoing,
    } = start;
    let shared = Arc::new(Shared::new(
        WorkerState {
            reports,
            closed,
            ..WorkerState::default()
        },
        false,
    ));
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
                    WorkerMode {
                        growing,
                        open_live,
                        outgoing: outgoing.as_deref(),
                    },
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
        outgoing_step: None,
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
pub(crate) fn redated(frame: &CpuVideoFrame, timing: MediaTiming) -> Option<CpuVideoFrame> {
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
            let config = playback_config(&settings, rate, clock_domain, dimensions);
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
                    outgoing: None,
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
                standbys: BTreeMap::new(),
                prefetch_gate: Arc::default(),
                outgoing: BTreeMap::new(),
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
                standbys: BTreeMap::new(),
                prefetch_gate: Arc::default(),
                outgoing: BTreeMap::new(),
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
        let config = playback_config(&settings, self.rate, self.clock_domain, self.dimensions);
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
                    outgoing: None,
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
                shared: Arc::new(Shared::new(
                    WorkerState {
                        reports: vec![FileReport::Fault(EngineMediaFault::ToolUnavailable)],
                        closed,
                        ..WorkerState::default()
                    },
                    true,
                )),
                thread: None,
                requested: None,
                collected: None,
                closed,
                blank_pending: false,
                last_frame: None,
                outgoing_step: None,
            }
        };
        self.workers.insert(input, worker);
        Ok(seed)
    }

    /// Opens the neighbouring media list item `role` of input `input`, the
    /// file at `path` played with `settings` (the item's marks), on its own
    /// thread, cued at its in mark (IN-006); see [`Standby`]. A standby
    /// already opening or open for the same role, file, cue, and playback
    /// configuration is kept; one for another item is dropped. `adapter`
    /// makes the codec adapter when none exists yet; without one nothing is
    /// prefetched.
    pub(crate) fn prefetch(
        &mut self,
        input: InputId,
        role: PrefetchRole,
        path: PathBuf,
        settings: &MediaPlaybackSettings,
        adapter: impl FnOnce() -> Option<Adapter>,
    ) {
        let key = (input, role);
        let config = playback_config(settings, self.rate, self.clock_domain, self.dimensions);
        let cue = settings.start_frame.max(settings.trim_in);
        if self.standbys.get(&key).is_some_and(|standby| {
            standby.path == path && standby.cue == cue && standby.config == config
        }) {
            return;
        }
        self.standbys.remove(&key);
        if self.adapter.is_none() {
            self.adapter = adapter();
        }
        let Some(adapter) = self.adapter.clone() else {
            return;
        };
        let (sender, result) = mpsc::sync_channel(1);
        let dimensions = self.dimensions;
        let opening = path.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let gate = Arc::clone(&self.prefetch_gate);
        let (thread_cancelled, thread_gate) = (Arc::clone(&cancelled), Arc::clone(&gate));
        let thread_name = match role {
            PrefetchRole::Next => format!("freemix-media-next-{input}"),
            PrefetchRole::Previous => format!("freemix-media-previous-{input}"),
        };
        let spawned = thread::Builder::new().name(thread_name).spawn(move || {
            // At most MAX_CONCURRENT_PREFETCHES files open at once.
            let Some(_turn) = thread_gate.acquire(MAX_CONCURRENT_PREFETCHES, &thread_cancelled)
            else {
                return;
            };
            // A still image is not a media file: its standby holds
            // nothing and the list opens it the slow way. The sniff
            // reads the file, so it runs here, off the session loop.
            if crate::native_catalogue::is_still(&opening) {
                let _ = sender.send(Err(EngineMediaFault::Unsupported));
                return;
            }
            let opened = catch_unwind(AssertUnwindSafe(|| {
                open_cued(&adapter, &opening, config, cue, dimensions)
            }))
            .unwrap_or(Err(EngineMediaFault::DecodeFailed));
            let _ = sender.send(opened);
        });
        if spawned.is_ok() {
            self.standbys.insert(
                key,
                Standby {
                    path,
                    cue,
                    config,
                    result,
                    cancelled,
                    gate,
                },
            );
        }
    }

    /// Drops every standby whose input and role `keep` does not name.
    pub(crate) fn retain_prefetched(&mut self, keep: impl Fn(InputId, PrefetchRole) -> bool) {
        self.standbys.retain(|(input, role), _| keep(*input, *role));
    }

    /// Takes media list input `input`'s standby when it opened the file at
    /// `path`, cued for `settings`, and is ready, whichever neighbour role it
    /// holds. One still opening stays (and the caller opens the file the slow
    /// way); one that failed is dropped, and the file faults when the slow
    /// way opens it.
    pub(crate) fn take_prefetched(
        &mut self,
        input: InputId,
        path: &std::path::Path,
        settings: &MediaPlaybackSettings,
    ) -> Option<Prefetched> {
        let config = playback_config(settings, self.rate, self.clock_domain, self.dimensions);
        let cue = settings.start_frame.max(settings.trim_in);
        let key = self
            .standbys
            .iter()
            .find(|((candidate, _), standby)| {
                *candidate == input
                    && standby.path == path
                    && standby.cue == cue
                    && standby.config == config
            })
            .map(|(key, _)| *key)?;
        let standby = self.standbys.get(&key)?;
        let taken = match standby.result.try_recv() {
            Err(mpsc::TryRecvError::Empty) => return None,
            Ok(Ok(prefetched)) => Some(prefetched),
            Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.standbys.remove(&key);
        taken
    }

    /// Every media list input with a standby, once each.
    pub(crate) fn prefetched_inputs(&self) -> Vec<InputId> {
        let mut inputs: Vec<InputId> = self.standbys.keys().map(|(input, _)| *input).collect();
        inputs.dedup();
        inputs
    }

    /// Replaces media-file input `input`'s worker with one that plays
    /// `prefetched` (a media list's next item, IN-006) from the next tick:
    /// the old worker becomes the input's outgoing worker (retired on the
    /// first projected frame without a transition), and the new one
    /// presents the projected ticks of the transport the caller opens on
    /// [`Prefetched::source`], blending with the outgoing one while the
    /// engine projects a transition.
    ///
    /// # Errors
    ///
    /// Names an input whose worker cannot be started.
    pub(crate) fn adopt(
        &mut self,
        input: InputId,
        path: PathBuf,
        prefetched: Prefetched,
    ) -> Result<(), String> {
        let Prefetched {
            player,
            config,
            growing,
            ..
        } = prefetched;
        self.standbys
            .retain(|(candidate, _), _| *candidate != input);
        let outgoing = self.keep_outgoing(input);
        let adapter = self
            .adapter
            .clone()
            .ok_or_else(|| format!("native media input {input} has no codec adapter"))?;
        let worker = spawn_worker(
            adapter,
            input,
            path,
            config,
            WorkerStart {
                player: Some(player),
                reports: Vec::new(),
                closed: false,
                open_live: false,
                outgoing,
            },
            growing,
            (self.rate, self.clock_domain, self.dimensions),
        )?;
        self.workers.insert(input, worker);
        Ok(())
    }

    /// Removes media-file input `input` (an input removed, or given another
    /// source, live): its worker stops and is joined off the session loop,
    /// and nothing more of it is presented or reported. Returns whether it
    /// had a worker.
    pub(crate) fn remove(&mut self, input: InputId) -> bool {
        self.sources.retain(|(candidate, _)| *candidate != input);
        self.blanks.retain(|blank| blank.input != input);
        if let Some(outgoing) = self.outgoing.remove(&input) {
            retire(input, outgoing);
        }
        let Some(worker) = self.workers.remove(&input) else {
            return false;
        };
        retire(input, worker);
        true
    }

    /// Turns media list input `input`'s worker into its outgoing worker
    /// (IN-006), retiring an earlier one, and returns the link the next
    /// worker blends through. A worker that is closed or failed plays out
    /// nothing and retires.
    fn keep_outgoing(&mut self, input: InputId) -> Option<Arc<Shared>> {
        self.sources.retain(|(candidate, _)| *candidate != input);
        self.blanks.retain(|blank| blank.input != input);
        if let Some(earlier) = self.outgoing.remove(&input) {
            retire(input, earlier);
        }
        let worker = self.workers.remove(&input)?;
        if worker.closed || worker.panicked() {
            retire(input, worker);
            return None;
        }
        {
            let mut state = worker.shared.lock();
            state.outgoing = true;
            state.request = None;
            state.completed = None;
        }
        let link = Arc::clone(&worker.shared);
        self.outgoing.insert(input, worker);
        Some(link)
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
            self.observe_outgoing(frame, media.input, media.outgoing.as_ref());
            let Some(worker) = self.workers.get_mut(&media.input) else {
                continue;
            };
            if worker.closed || worker.panicked() {
                continue;
            }
            let blend = media.outgoing.as_ref().map(|outgoing| Blend {
                kind: item_transition(outgoing.kind),
                weight: outgoing.mix_basis_points,
            });
            let request = media
                .tick
                .clone()
                .map(|tick| Request { frame, tick, blend });
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

    /// Hands input `input`'s outgoing worker the outgoing item's tick on
    /// engine frame `frame`. The outgoing item of a list transition plays
    /// its own ticks; the first frame without one retires it.
    fn observe_outgoing(
        &mut self,
        frame: u64,
        input: InputId,
        outgoing: Option<&fm_engine::EngineMediaOutgoing>,
    ) {
        let Some(outgoing) = outgoing else {
            if let Some(worker) = self.outgoing.remove(&input) {
                retire(input, worker);
            }
            return;
        };
        // A step no later in the transition on a later frame (or an
        // earlier step on the same one) is another transition: the item
        // this worker played is no longer the outgoing one.
        let restarted = self.outgoing.get(&input).is_some_and(|worker| {
            worker.outgoing_step.is_some_and(|(last_frame, last_step)| {
                outgoing.step < last_step || (outgoing.step == last_step && frame > last_frame)
            })
        });
        if restarted && let Some(worker) = self.outgoing.remove(&input) {
            retire(input, worker);
        }
        if let Some(worker) = self.outgoing.get_mut(&input) {
            worker.outgoing_step = Some((frame, outgoing.step));
            let request = Some(Request {
                frame,
                tick: outgoing.tick.clone(),
                blend: None,
            });
            if worker.requested != request {
                worker.shared.lock().request.clone_from(&request);
                worker.requested = request;
                worker.shared.wake.notify_one();
            }
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

/// Stops `worker` (input `input`'s) and joins it off the session loop.
fn retire(input: InputId, mut worker: Worker) {
    {
        let mut state = worker.shared.lock();
        state.shutdown = true;
        state.handoff = None;
    }
    worker.shared.wake.notify_one();
    worker.shared.handed_off.notify_all();
    if let Some(thread) = worker.thread.take() {
        // Bounded by one in-flight tool run, which the loop never waits
        // for.
        let _ = thread::Builder::new()
            .name(format!("freemix-media-file-retire-{input}"))
            .spawn(move || {
                let _ = thread.join();
            });
    }
}

/// The CPU blend of a list transition kind; a cut composites nothing (the
/// engine never projects one).
const fn item_transition(kind: fm_model::MediaListTransitionKind) -> fm_video::ItemTransition {
    match kind {
        fm_model::MediaListTransitionKind::Dip => fm_video::ItemTransition::Dip,
        fm_model::MediaListTransitionKind::Wipe => fm_video::ItemTransition::Wipe,
        fm_model::MediaListTransitionKind::Cut | fm_model::MediaListTransitionKind::Fade => {
            fm_video::ItemTransition::Mix
        }
    }
}

/// `incoming` composited over `outgoing` for `blend`, dated `timing`
/// with the incoming picture's metadata; `None` when the two pictures are
/// not the same RGBA8 raster.
fn blended_picture(
    outgoing: &CpuVideoFrame,
    incoming: &CpuVideoFrame,
    blend: Blend,
    timing: MediaTiming,
) -> Option<CpuVideoFrame> {
    let (from, to) = (outgoing.payload(), incoming.payload());
    if from.format() != PixelFormat::Rgba8
        || to.format() != PixelFormat::Rgba8
        || from.dimensions() != to.dimensions()
    {
        return None;
    }
    let (from_plane, to_plane) = (from.plane(0)?, to.plane(0)?);
    if from_plane.stride() != to_plane.stride() {
        return None;
    }
    let size = to.dimensions();
    let mut bytes = vec![0; to_plane.bytes().len()];
    fm_video::blend_items(
        blend.kind,
        from_plane.bytes(),
        to_plane.bytes(),
        &mut bytes,
        (size.width(), size.height(), to_plane.stride()),
        blend.weight,
    )
    .ok()?;
    let plane = CpuVideoPlane::new(to_plane.stride(), bytes).ok()?;
    let payload = CpuVideoPayload::new(PixelFormat::Rgba8, size, vec![plane]).ok()?;
    let frame = CpuVideoFrame::new(timing, payload);
    match incoming.metadata() {
        Some(metadata) => frame.with_metadata(metadata).ok(),
        None => Some(frame),
    }
}

/// `block` with every sample scaled by `gain`.
fn scaled_audio(block: &AudioBlock, gain: f32) -> Option<AudioBlock> {
    AudioBlock::new(
        block.timing(),
        block.sample_rate(),
        block.channel_layout().clone(),
        block
            .planes()
            .iter()
            .map(|plane| plane.iter().map(|sample| sample * gain).collect())
            .collect(),
    )
    .ok()
}

/// The two items' sound of one transition frame, at their gains. Blocks of
/// one rate and layout are summed in the incoming block's length (the two
/// transports' sample cadences need not be in phase: a missing outgoing
/// sample is silence). Blocks of differing rates or layouts are never
/// summed: the item at the larger gain plays alone (the outgoing one over a
/// dip's first half, the incoming one over its second). A silent item leaves
/// the other one's sound.
fn blended_audio(
    outgoing: Option<&AudioBlock>,
    incoming: Option<&AudioBlock>,
    blend: Blend,
) -> Option<AudioBlock> {
    let (out_gain, in_gain) = blend.kind.audio_gains(blend.weight);
    match (outgoing, incoming) {
        (Some(from), Some(to))
            if from.sample_rate() == to.sample_rate()
                && from.channel_layout() == to.channel_layout() =>
        {
            AudioBlock::new(
                to.timing(),
                to.sample_rate(),
                to.channel_layout().clone(),
                from.planes()
                    .iter()
                    .zip(to.planes())
                    .map(|(a, b)| {
                        b.iter()
                            .enumerate()
                            .map(|(index, b)| {
                                a.get(index).copied().unwrap_or(0.0) * out_gain + b * in_gain
                            })
                            .collect()
                    })
                    .collect(),
            )
            .ok()
        }
        (Some(from), Some(_)) if out_gain > in_gain => scaled_audio(from, out_gain),
        (_, Some(to)) => scaled_audio(to, in_gain),
        (Some(from), None) => scaled_audio(from, out_gain),
        (None, None) => None,
    }
}

impl Drop for NativeMediaFiles {
    fn drop(&mut self) {
        for worker in self.workers.values_mut().chain(self.outgoing.values_mut()) {
            worker.shared.lock().shutdown = true;
            worker.shared.wake.notify_one();
            worker.shared.handed_off.notify_all();
        }
        for worker in self.workers.values_mut().chain(self.outgoing.values_mut()) {
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

/// The pictures a worker presented, for what it presents next.
#[derive(Default)]
struct Pictures {
    /// The file's picture presented last.
    last: Option<Arc<CpuVideoFrame>>,
    /// A list transition with no outgoing worker (a re-cue of the open
    /// file) holds the picture presented before it.
    held: Option<Arc<CpuVideoFrame>>,
    /// The last frame was a blend: the next presents the plain picture
    /// even when it did not change.
    blended: bool,
    /// A list transition with neither an outgoing worker nor a picture to
    /// hold (a step to a file that was not prefetched) cuts: its picture
    /// and sound play in full until the next plain tick.
    cut: bool,
}

impl Pictures {
    /// What a tick presents: its picture when it is new (else the input
    /// keeps its current one), re-dated `timing`, and its sound.
    fn plain(
        &mut self,
        output: fm_codec_ffmpeg::PlaybackOutput,
        timing: Option<MediaTiming>,
    ) -> (Option<CpuVideoFrame>, Option<AudioBlock>) {
        self.held = None;
        self.cut = false;
        let after_blend = std::mem::take(&mut self.blended);
        let fresh = output.video.filter(|picture| {
            after_blend
                || self
                    .last
                    .as_ref()
                    .is_none_or(|last| !Arc::ptr_eq(last, picture))
        });
        let video = fresh.and_then(|picture| {
            let moved = redated(&picture, timing?);
            self.last = Some(picture);
            moved
        });
        (video, output.audio)
    }

    /// What a list transition's frame presents: the tick's picture blended
    /// over the outgoing item's (`handoff`, else the held picture), and the
    /// two items' sound at the transition's gains.
    fn blend(
        &mut self,
        output: &fm_codec_ffmpeg::PlaybackOutput,
        blend: Blend,
        handoff: Option<Handoff>,
        timing: Option<MediaTiming>,
    ) -> (Option<CpuVideoFrame>, Option<AudioBlock>) {
        let previous = self.last.clone();
        if let Some(picture) = &output.video {
            self.last = Some(Arc::clone(picture));
        }
        let (from_picture, from_audio, cut) = if let Some(handoff) = handoff {
            (handoff.picture, handoff.audio, false)
        } else {
            if self.held.is_none() && !self.cut {
                self.cut = previous.is_none();
                self.held = previous;
            }
            (self.held.clone(), None, self.cut)
        };
        self.blended = true;
        if cut {
            let video = output
                .video
                .as_ref()
                .zip(timing)
                .and_then(|(to, timing)| redated(to, timing));
            return (video, output.audio.clone());
        }
        let video = match (&from_picture, &output.video, timing) {
            (Some(from), Some(to), Some(timing)) => {
                blended_picture(from, to, blend, timing).or_else(|| redated(to, timing))
            }
            (None, Some(to), Some(timing)) => redated(to, timing),
            _ => None,
        };
        (
            video,
            blended_audio(from_audio.as_ref(), output.audio.as_ref(), blend),
        )
    }
}

/// How a worker follows its file.
#[derive(Clone, Copy)]
struct WorkerMode<'a> {
    growing: bool,
    /// Added live: open the file before anything else.
    open_live: bool,
    /// The outgoing worker of the list transition this worker blends with
    /// (IN-006).
    outgoing: Option<&'a Shared>,
}

/// Runs one input's worker. A picture that is not `dimensions` (a file
/// reopened after it changed on disk) faults the input as unsupported
/// instead of reaching the source registry, which would fail the session.
#[allow(clippy::too_many_lines)] // one worker loop: close, reopen, present
fn run_worker(
    shared: &Shared,
    mut opened: Option<FilePlayer>,
    reopen: &dyn Fn() -> Result<FilePlayer, FilePlaybackError>,
    WorkerMode {
        growing,
        open_live,
        outgoing,
    }: WorkerMode<'_>,
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
    let mut pictures = Pictures::default();
    loop {
        let (request, complete, handing_off) = {
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
                    pictures = Pictures::default();
                    continue;
                }
                // Reopening opens the file again; a file that no longer
                // opens takes the input offline through its fault.
                drop(state);
                opened = None;
                pictures = Pictures::default();
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
            (
                state.request.take(),
                std::mem::take(&mut state.complete),
                state.outgoing,
            )
        };
        let Some(player) = opened.as_mut() else {
            // A file that did not reopen presents nothing.
            if let Some(request) = request {
                if handing_off {
                    shared.lock().handoff = Some(Handoff {
                        frame: request.frame,
                        picture: None,
                        audio: None,
                    });
                    shared.handed_off.notify_all();
                } else {
                    shared.lock().completed = Some((request, None, None));
                }
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
            let mut handoff = None;
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
                    Ok(output) if handing_off => {
                        // An outgoing item hands its picture and sound to
                        // the incoming item's worker.
                        handoff = Some((output.video, output.audio));
                        (None, None)
                    }
                    Ok(output) => {
                        let timing = output_timing(request.frame, rate, clock_domain);
                        if let Some(blend) = request.blend {
                            let handoff = outgoing.and_then(|link| link.handoff_for(request.frame));
                            pictures.blend(&output, blend, handoff, timing)
                        } else {
                            pictures.plain(output, timing)
                        }
                    }
                    Err(error) => {
                        faulted = true;
                        reports.push(FileReport::Fault(engine_fault(&error)));
                        (None, None)
                    }
                }
            };
            if handing_off {
                // Even a failed tick hands off, so the incoming item's
                // worker never waits for it.
                let (picture, audio) = handoff.unwrap_or((None, None));
                shared.lock().handoff = Some(Handoff {
                    frame: request.frame,
                    picture,
                    audio,
                });
                shared.handed_off.notify_all();
            } else {
                completed = Some((request, video, audio));
            }
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

/// Decodes media list item pictures (IN-006) for the input thumbnail
/// worker: the item's in-mark frame, opened through its own codec adapter
/// confined to the bundle's assets. The adapter is made on first use: a
/// bundle without media has no assets directory until a list imports one.
struct ItemFrames {
    assets_root: PathBuf,
    adapter: Mutex<Option<Adapter>>,
}

impl crate::input_thumbnails::ItemPictures for ItemFrames {
    fn picture(
        &self,
        path: &std::path::Path,
        frame: u64,
        (width, height): (u32, u32),
    ) -> Option<fm_sim::ImageFrame> {
        let rate = FrameRate::new(25, 1).ok()?;
        let domain = ClockDomainId::new(std::num::NonZeroU128::MIN);
        let mut config = FilePlaybackConfig::new(domain, rate);
        config.audio = None;
        config.window_frames = NonZeroU32::MIN;
        let adapter = {
            let mut adapter = self.adapter.lock().unwrap_or_else(PoisonError::into_inner);
            if adapter.is_none() {
                *adapter = Adapter::new(fm_codec_ffmpeg::Config {
                    allowed_root: Some(self.assets_root.clone()),
                    ..fm_codec_ffmpeg::Config::default()
                })
                .ok();
            }
            adapter.clone()?
        };
        let mut player = adapter.open_playback(path, config).ok()?;
        let last = player.controller().playable_end().get().checked_sub(1)?;
        let picture = player.decode_frame(FrameIndex::new(frame.min(last))).ok()?;
        crate::scene_preview::sampled_live_picture(&picture, width, height)
    }
}

/// The item picture decoder of a daemon whose bundle is `store`; `None`
/// without a bundle.
pub(crate) fn item_pictures(
    store: Option<&fm_persistence::ProjectStore>,
) -> Option<Arc<dyn crate::input_thumbnails::ItemPictures>> {
    Some(Arc::new(ItemFrames {
        assets_root: store?.assets_root(),
        adapter: Mutex::new(None),
    }))
}

/// Probes media list item lengths (Protocol 2.91) through the decoder path
/// playback opens files with: the file opened at the program rate, so its
/// frame count is the one the transport reports once the item plays (an
/// audio-only file's frames counted at the program rate). Confined to the
/// bundle's assets; the adapter is made on first use.
struct ItemLengths {
    assets_root: PathBuf,
    adapter: Mutex<Option<Adapter>>,
}

impl crate::media_lengths::LengthProbe for ItemLengths {
    fn length(
        &self,
        path: Option<&std::path::Path>,
        rate: FrameRate,
    ) -> Option<fm_protocol::MediaListItemLength> {
        let path = path?;
        let domain = ClockDomainId::new(std::num::NonZeroU128::MIN);
        let mut config = FilePlaybackConfig::new(domain, rate);
        config.audio = None;
        config.window_frames = NonZeroU32::MIN;
        // Only counted, never shown.
        config.audio_only_picture = VideoDimensions::new(16, 16);
        let adapter = {
            let mut adapter = self.adapter.lock().unwrap_or_else(PoisonError::into_inner);
            if adapter.is_none() {
                *adapter = Adapter::new(fm_codec_ffmpeg::Config {
                    allowed_root: Some(self.assets_root.clone()),
                    ..fm_codec_ffmpeg::Config::default()
                })
                .ok();
            }
            adapter.clone()?
        };
        let player = adapter.open_playback(path, config).ok()?;
        Some(fm_protocol::MediaListItemLength {
            frames: player.frame_count(),
            rate: player.frame_rate(),
        })
    }
}

/// The item length probe of a native daemon whose bundle is `store`;
/// `None` without a bundle.
pub(crate) fn item_lengths(
    store: Option<&fm_persistence::ProjectStore>,
) -> Option<Arc<dyn crate::media_lengths::LengthProbe>> {
    Some(Arc::new(ItemLengths {
        assets_root: store?.assets_root(),
        adapter: Mutex::new(None),
    }))
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
                shared: Arc::new(Shared::new(WorkerState::default(), false)),
                thread: None,
                requested: None,
                collected: None,
                closed: false,
                blank_pending: false,
                last_frame: Some(5),
                outgoing_step: None,
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
                blend: None,
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

    /// Standbys open at most [`MAX_CONCURRENT_PREFETCHES`] files at once,
    /// and one dropped while it waits for its turn gives up.
    #[test]
    fn background_opens_are_capped_and_a_dropped_waiter_gives_up() {
        let gate = Arc::new(PrefetchGate::default());
        let open = Arc::new(Mutex::new((0_usize, 0_usize)));
        let openers: Vec<_> = (0..8)
            .map(|_| {
                let (gate, open) = (Arc::clone(&gate), Arc::clone(&open));
                thread::spawn(move || {
                    let never = AtomicBool::new(false);
                    let _turn = gate.acquire(MAX_CONCURRENT_PREFETCHES, &never).unwrap();
                    {
                        let mut open = open.lock().unwrap();
                        open.0 += 1;
                        open.1 = open.1.max(open.0);
                    }
                    thread::sleep(Duration::from_millis(20));
                    open.lock().unwrap().0 -= 1;
                })
            })
            .collect();
        for opener in openers {
            opener.join().unwrap();
        }
        let most = open.lock().unwrap().1;
        assert!(
            (1..=MAX_CONCURRENT_PREFETCHES).contains(&most),
            "{most} files opened at once"
        );

        let held: Vec<_> = (0..MAX_CONCURRENT_PREFETCHES)
            .map(|_| gate.acquire(MAX_CONCURRENT_PREFETCHES, &AtomicBool::new(false)))
            .collect();
        assert!(held.iter().all(Option::is_some));
        let cancelled = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (gate, cancelled) = (Arc::clone(&gate), Arc::clone(&cancelled));
            thread::spawn(move || {
                gate.acquire(MAX_CONCURRENT_PREFETCHES, &cancelled)
                    .is_some()
            })
        };
        thread::sleep(Duration::from_millis(30));
        cancelled.store(true, Ordering::Release);
        gate.wake();
        assert!(!waiter.join().unwrap(), "a dropped standby never opens");
        drop(held);
        assert!(
            gate.acquire(MAX_CONCURRENT_PREFETCHES, &AtomicBool::new(false))
                .is_some()
        );
    }

    /// A block of `samples` samples per channel, every one `value`.
    fn audio(
        rate: u32,
        layout: &fm_types::ChannelLayout,
        samples: usize,
        value: f32,
    ) -> AudioBlock {
        let timing = timing_at(
            0,
            FrameRate::new(25, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        )
        .unwrap();
        AudioBlock::new(
            timing,
            fm_types::SampleRate::new(rate).unwrap(),
            layout.clone(),
            vec![vec![value; samples]; layout.channels().len()],
        )
        .unwrap()
    }

    /// At 29.97 fps the two items' blocks alternate 1601 and 1602 samples
    /// out of phase: a fade still hears the outgoing item over the samples
    /// both carry, in the incoming block's length.
    #[test]
    fn a_fade_sums_blocks_of_differing_lengths() {
        let stereo = fm_types::ChannelLayout::stereo();
        let from = audio(48_000, &stereo, 1601, 1.0);
        let to = audio(48_000, &stereo, 1602, 0.0);
        let mix = Blend {
            kind: fm_video::ItemTransition::Mix,
            weight: 5_000,
        };
        let mixed = blended_audio(Some(&from), Some(&to), mix).unwrap();
        assert_eq!(mixed.sample_count(), 1602);
        for plane in mixed.planes() {
            assert!(
                plane[..1601]
                    .iter()
                    .all(|sample| (*sample - 0.5).abs() < f32::EPSILON)
            );
            assert!(plane[1601].abs() < f32::EPSILON);
        }
    }

    /// Items of differing rates or layouts are never summed, but the
    /// outgoing item is still heard while it outweighs the incoming one (a
    /// dip's first half), and the incoming one after.
    #[test]
    fn a_dip_across_formats_fades_the_outgoing_item_out() {
        let mono = fm_types::ChannelLayout::new(vec![fm_types::Channel::Mono]).unwrap();
        let from = audio(44_100, &mono, 1764, 1.0);
        let to = audio(48_000, &fm_types::ChannelLayout::stereo(), 1920, 1.0);
        let dip = |weight| Blend {
            kind: fm_video::ItemTransition::Dip,
            weight,
        };
        let first = blended_audio(Some(&from), Some(&to), dip(2_500)).unwrap();
        assert_eq!(first.sample_rate(), from.sample_rate());
        assert!(
            first
                .planes()
                .iter()
                .flatten()
                .all(|sample| (*sample - 0.5).abs() < f32::EPSILON)
        );
        let second = blended_audio(Some(&from), Some(&to), dip(7_500)).unwrap();
        assert_eq!(second.sample_rate(), to.sample_rate());
        assert!(
            second
                .planes()
                .iter()
                .flatten()
                .all(|sample| (*sample - 0.5).abs() < f32::EPSILON)
        );
    }

    /// A decoded tick of a 32x18 picture and a stereo block of `value`.
    fn decoded(value: f32) -> fm_codec_ffmpeg::PlaybackOutput {
        let timing = timing_at(
            0,
            FrameRate::new(25, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        )
        .unwrap();
        fm_codec_ffmpeg::PlaybackOutput {
            tick: MediaTick {
                output_tick: 0,
                frame: None,
                repeated: false,
                dropped_frames: 0,
                discontinuity: false,
                state: MediaState::Playing,
                audio: fm_engine::fm_playback::AudioPlan::None,
                events: Vec::new(),
            },
            video: Some(Arc::new(blank_picture((32, 18), timing).unwrap())),
            video_late: false,
            audio: Some(audio(
                48_000,
                &fm_types::ChannelLayout::stereo(),
                1920,
                value,
            )),
        }
    }

    /// A step to a file that was not prefetched cuts (a fresh worker has no
    /// outgoing picture to blend over): its sound plays at full gain with
    /// its picture, even over a dip's first half, while a re-cue of the
    /// open file (a picture to hold) fades its sound in with the picture.
    #[test]
    fn a_transition_without_an_outgoing_picture_cuts_its_sound_too() {
        let timing = timing_at(
            3,
            FrameRate::new(25, 1).unwrap(),
            ClockDomainId::new(NonZeroU128::new(1).unwrap()),
        );
        let dip = Blend {
            kind: fm_video::ItemTransition::Dip,
            weight: 2_500,
        };
        let mut fresh = Pictures::default();
        for _ in 0..2 {
            let (video, sound) = fresh.blend(&decoded(1.0), dip, None, timing);
            assert!(video.is_some());
            let sound = sound.unwrap();
            assert!(
                sound
                    .planes()
                    .iter()
                    .flatten()
                    .all(|sample| (*sample - 1.0).abs() < f32::EPSILON)
            );
        }

        let mut recued = Pictures::default();
        let _ = recued.plain(decoded(1.0), timing);
        let (_, sound) = recued.blend(&decoded(1.0), dip, None, timing);
        assert!(
            sound
                .unwrap()
                .planes()
                .iter()
                .flatten()
                .all(|sample| (*sample - 0.0).abs() < f32::EPSILON)
        );
    }

    /// The outgoing item on step `step` of a fade, presenting source frame
    /// `source`.
    fn outgoing_at(step: u32, source: u64) -> fm_engine::EngineMediaOutgoing {
        fm_engine::EngineMediaOutgoing {
            tick: MediaTick {
                output_tick: source,
                frame: Some(FrameIndex::new(source)),
                repeated: false,
                dropped_frames: 0,
                discontinuity: false,
                state: MediaState::Playing,
                audio: fm_engine::fm_playback::AudioPlan::None,
                events: Vec::new(),
            },
            kind: fm_model::MediaListTransitionKind::Fade,
            step,
            frames: 10,
            mix_basis_points: 1_000,
        }
    }

    /// Items A (a.mp4), B and C (two ranges of b.mov) fade: stepping A to
    /// B makes A's worker the outgoing one, and stepping on to C mid-fade
    /// (a re-cue of b.mov, which adopts nothing) restarts the transition
    /// with B's transport outgoing. A's worker retires instead of being
    /// handed b.mov's frames, so the incoming item blends over its held
    /// picture of B.
    #[test]
    fn an_outgoing_worker_retires_when_a_re_cue_restarts_the_transition() {
        for (restart_frame, restart_step) in [(12, 1), (11, 1), (12, 2)] {
            let mut files = files();
            let shared = Arc::new(Shared::new(
                WorkerState {
                    outgoing: true,
                    ..WorkerState::default()
                },
                false,
            ));
            files.outgoing.insert(
                input(),
                Worker {
                    shared: Arc::clone(&shared),
                    thread: None,
                    requested: None,
                    collected: None,
                    closed: false,
                    blank_pending: false,
                    last_frame: None,
                    outgoing_step: None,
                },
            );
            files.observe_outgoing(10, input(), Some(&outgoing_at(1, 40)));
            files.observe_outgoing(11, input(), Some(&outgoing_at(2, 41)));
            // A frame projected again keeps the transition.
            files.observe_outgoing(11, input(), Some(&outgoing_at(2, 41)));
            assert!(files.outgoing.contains_key(&input()));
            assert!(!shared.lock().shutdown);

            files.observe_outgoing(restart_frame, input(), Some(&outgoing_at(restart_step, 25)));
            assert!(
                !files.outgoing.contains_key(&input()),
                "restarted on frame {restart_frame} at step {restart_step}"
            );
            assert!(shared.lock().shutdown);
        }
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
