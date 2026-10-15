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
//! its host deadline; a frame realized without it shows the input's previous
//! picture and silence, and is counted. A planned frame a growing source
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
//! source.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::PathBuf;
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
    AudioBlock, ClockDomainId, CpuVideoFrame, MediaTimestamp, MediaTiming, NormalizedDuration,
    NormalizedTimestamp, OriginalTimestamp, SequenceNumber, TimeBase,
};
use fm_model::MediaPlaybackSettings;
use fm_types::{FrameRate, InputId};

/// Longest a projected frame waits for a media input's tick to be presented
/// before it is realized with the input's previous picture.
pub(crate) const MAX_MEDIA_FRAME_WAIT: Duration = Duration::from_millis(100);

/// One media-file input a native daemon plays.
pub(crate) struct MediaFileInput {
    pub(crate) input: InputId,
    pub(crate) path: PathBuf,
    pub(crate) settings: MediaPlaybackSettings,
}

/// What a worker reports to the engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FileReport {
    /// The source now has `frames` frames, complete or still growing.
    Source { frames: u64, complete: bool },
    /// The source failed; its transport stops.
    Fault(EngineMediaFault),
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
    shutdown: bool,
}

struct Shared {
    state: Mutex<WorkerState>,
    wake: Condvar,
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
}

/// The media-file inputs of one native daemon and their workers.
pub(crate) struct NativeMediaFiles {
    workers: BTreeMap<InputId, Worker>,
    /// Each input's source as opened, for the engine.
    sources: Vec<(InputId, EngineMediaSource)>,
    late_frame: Option<u64>,
    late_frames: u64,
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
fn output_timing(frame: u64, rate: FrameRate, clock_domain: ClockDomainId) -> Option<MediaTiming> {
    timing_at(frame.checked_add(1)?, rate, clock_domain)
}

/// Sequence `sequence` on the program frame grid, one frame long.
fn timing_at(sequence: u64, rate: FrameRate, clock_domain: ClockDomainId) -> Option<MediaTiming> {
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

/// `frame` re-dated onto the output timeline.
fn redated(frame: &CpuVideoFrame, timing: MediaTiming) -> Option<CpuVideoFrame> {
    let moved = CpuVideoFrame::new(timing, frame.payload().clone());
    match frame.metadata() {
        Some(metadata) => moved.with_metadata(metadata).ok(),
        None => Some(moved),
    }
}

impl NativeMediaFiles {
    /// Opens every input's file, decodes its startup picture (the frame its
    /// transport cues), and starts its worker. Returns the startup pictures
    /// to seed the source registry with, dated sequence zero.
    ///
    /// # Errors
    ///
    /// Names the first input whose file does not open, has no picture yet,
    /// or does not have the project's `dimensions`.
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
        } in inputs
        {
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
            let mut player = adapter.open_playback(&path, config).map_err(|error| {
                format!("native media input {input} cannot open its file: {error}")
            })?;
            let frames = player.frame_count();
            let cue = settings.start_frame.max(settings.trim_in);
            let last = player.controller().playable_end().get().saturating_sub(1);
            let seed = player
                .decode_frame(FrameIndex::new(cue.min(last)))
                .map_err(|error| {
                    format!("native media input {input} has no startup picture: {error}")
                })?;
            let size = seed.payload().dimensions();
            if (size.width(), size.height()) != dimensions {
                return Err(format!(
                    "native media input {input} is {}x{}, but must match project output {}x{}",
                    size.width(),
                    size.height(),
                    dimensions.0,
                    dimensions.1
                ));
            }
            let seed_timing = timing_at(0, rate, clock_domain)
                .ok_or_else(|| format!("native media input {input} cannot be timed"))?;
            seeds.push((
                input,
                redated(&seed, seed_timing)
                    .ok_or_else(|| format!("native media input {input} cannot be timed"))?,
            ));
            sources.push((
                input,
                EngineMediaSource::external(
                    player.frame_rate(),
                    frames,
                    settings.growing.is_none(),
                    player.audio_tracks().to_vec(),
                ),
            ));
            let shared = Arc::new(Shared {
                state: Mutex::new(WorkerState::default()),
                wake: Condvar::new(),
            });
            let worker_shared = Arc::clone(&shared);
            let growing = settings.growing.is_some();
            let reopen_adapter = adapter.clone();
            let thread = thread::Builder::new()
                .name(format!("freemix-media-file-{input}"))
                .spawn(move || {
                    let reopen = || reopen_adapter.open_playback(&path, config);
                    run_worker(
                        &worker_shared,
                        Some(player),
                        &reopen,
                        growing,
                        rate,
                        clock_domain,
                    );
                })
                .map_err(|error| format!("native media input {input} worker: {error}"))?;
            workers.insert(
                input,
                Worker {
                    shared,
                    thread: Some(thread),
                    requested: None,
                    collected: None,
                    closed: false,
                },
            );
        }
        Ok((
            Self {
                workers,
                sources,
                late_frame: None,
                late_frames: 0,
            },
            seeds,
        ))
    }

    /// No media-file inputs (a project without local video).
    pub(crate) fn start_empty() -> (Self, Vec<(InputId, CpuVideoFrame)>) {
        (
            Self {
                workers: BTreeMap::new(),
                sources: Vec::new(),
                late_frame: None,
                late_frames: 0,
            },
            Vec::new(),
        )
    }

    /// Every input's source as opened, for
    /// [`fm_control::ControlService::set_media_source`].
    pub(crate) fn sources(&self) -> &[(InputId, EngineMediaSource)] {
        &self.sources
    }

    /// Hands each worker the tick the engine projects for its input on
    /// `projected` (and a completion the engine requested). Never decodes.
    pub(crate) fn observe_frame(&mut self, projected: &FrameResult) {
        let frame = projected.frame.get();
        for media in &projected.media {
            let Some(worker) = self.workers.get_mut(&media.input) else {
                continue;
            };
            if worker.closed {
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

    /// Whether `projected`, due at host instant `deadline`, may be realized:
    /// every media input's projected tick has been presented and collected,
    /// or the frame is [`MAX_MEDIA_FRAME_WAIT`] past its deadline.
    pub(crate) fn covered(&mut self, projected: &FrameResult, deadline: Instant) -> bool {
        let frame = projected.frame.get();
        let pending = projected.media.iter().any(|media| {
            self.workers.get(&media.input).is_some_and(|worker| {
                !worker.closed
                    && media.tick.as_ref().is_some_and(|tick| {
                        worker.collected.as_ref().is_none_or(|collected| {
                            collected.frame != frame || collected.tick != *tick
                        })
                    })
            })
        });
        if !pending {
            return true;
        }
        if Instant::now() < deadline + MAX_MEDIA_FRAME_WAIT {
            return false;
        }
        if self.late_frame != Some(frame) {
            self.late_frame = Some(frame);
            self.late_frames = self.late_frames.saturating_add(1);
        }
        true
    }

    /// Takes every presented tick not yet collected, in input order.
    pub(crate) fn take_completed(&mut self) -> Vec<Presented> {
        let mut presented = Vec::new();
        for (&input, worker) in &mut self.workers {
            let Some((request, video, audio)) = worker.shared.lock().completed.take() else {
                continue;
            };
            presented.push(Presented {
                input,
                frame: request.frame,
                video,
                audio,
            });
            worker.collected = Some(request);
        }
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
            worker.requested = None;
            worker.collected = None;
        }
        let mut state = worker.shared.lock();
        if state.closed != closed {
            state.closed = closed;
            state.request = None;
            drop(state);
            worker.shared.wake.notify_one();
        }
        true
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
        self.late_frames
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

#[allow(clippy::too_many_lines)] // one worker loop: close, reopen, present
fn run_worker(
    shared: &Shared,
    mut opened: Option<FilePlayer>,
    reopen: &dyn Fn() -> Result<FilePlayer, FilePlaybackError>,
    growing: bool,
    rate: FrameRate,
    clock_domain: ClockDomainId,
) {
    let second = Duration::from_secs(1);
    let mut next_refresh = Instant::now() + second;
    let mut faulted = false;
    let mut released = false;
    let mut last_picture: Option<Arc<CpuVideoFrame>> = None;
    loop {
        let (request, complete) = {
            let mut state = shared.lock();
            loop {
                if state.shutdown {
                    return;
                }
                if state.closed != released {
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
            if state.closed != released {
                released = state.closed;
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
                match reopen() {
                    Ok(player) => {
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
