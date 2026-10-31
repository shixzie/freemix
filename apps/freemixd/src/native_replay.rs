//! Native instant replay (OR-012): the recording and the Replay inputs.
//!
//! While the show records sources for replay ([`fm_protocol::ReplaySettings`])
//! each source has one recording *run*: a worker thread that owns one
//! `FFmpeg` [`Recorder`] encoding the source's newest picture once per engine
//! frame (H.264 at the project size, silent AAC) into a fragmented MP4
//! stream with a one-second GOP, which a [`ReplaySegmentWriter`] cuts into
//! rolling two-second segment files. The render loop only hands the worker
//! a shared reference to the picture; scaling, conversion, and encoding
//! happen off the loop. Every frame handed over is reported to the engine
//! ([`ControlService::replay_observe`]) with its stream frame, and every
//! finished segment file is cataloged
//! ([`ControlService::replay_segment_closed`]); the files retention releases
//! ([`ControlService::take_replay_pruned`]) are deleted.
//!
//! A run that falls behind (its queue fills) or fails ends and a new run
//! starts after [`RETRY_DELAY`]; what the ended run had cataloged stays
//! playable. A change of sources or storage root ends every run and starts
//! over, like the engine's recording.
//!
//! The files live in one `freemixd-replay-<pid>-<random>` session directory
//! under the storage root (`<input>-<run>/segment-NNNNNN.mp4`). A session's
//! `manifest.tsv` names every closed segment's input, engine-frame origin,
//! stream-frame origin, frame count, byte count, and file; on startup the
//! next daemon restores the newest compatible session's closed segments
//! (marking its manifest restored) and removes the other stale sessions.
//! Files a config change drops are still removed on the spot.
//!
//! Each Replay input has a decode worker that opens the segment file the
//! projected frame's player shows ([`fm_engine::EngineReplayFrame`]) and
//! decodes the exact frame; the projected frame waits a bounded time
//! ([`crate::native_files::MAX_MEDIA_FRAME_WAIT`]) for it, so playback at
//! every speed is frame-exact while the decoder keeps up. A player with
//! nothing cued shows black; one whose frame is not in a finished segment
//! yet (the live edge) holds its picture.
//!
//! Each frame's segment audio is the source's own audio of that frame,
//! before its strip, fader, and the mix, as stereo (a mono source on both
//! channels); a source without audio records silence. A Replay input
//! playing forward at 100% plays each shown frame's recorded audio through
//! its own strip; slow motion and a paused or cued player are silent.
//! Every segment's pictures start 80 ms after its sound (the encoder's
//! B-frame delay), so a frame's audio is read from the segment's first
//! video frame on, not from its first sample. A run's first segment opens
//! with the AAC priming packet at 0 ahead of a 2816-sample hole (ffmpeg's
//! fragmented MP4 muxer shifts every stream for the B-frame video but not
//! that packet); the adapter skips it and decodes from the second packet.
//!
//! Limitations: the recorded picture is each source's newest decoded
//! picture when the frame is realized (for a media file that can be a few
//! frames ahead of what aired).

use std::{
    collections::{BTreeMap, VecDeque},
    fmt::Write as _,
    fs, io,
    num::{NonZeroU32, NonZeroU128},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use fm_auth::Policy;
use fm_codec_ffmpeg::{
    Adapter, FilePlaybackConfig, FilePlayer,
    record::{
        EnqueueRejection, PairedFrame, RecordConfig, RecordFormat, RecordLimits, RecordSink,
        Recorder, X264Preset,
    },
};
use fm_control::ControlService;
use fm_engine::fm_playback::FrameIndex;
use fm_engine::{EngineReplayFeed, EngineReplayFrame, FrameResult};
use fm_frame::{
    AlphaMode, AudioBlock, ChromaLocation, ClockDomainId, ColorMetadata, ColorPrimaries,
    CpuVideoFrame, MatrixCoefficients, MediaTimestamp, MediaTiming, NormalizedDuration,
    NormalizedTimestamp, OriginalTimestamp, SequenceNumber, SignalRange, TimeBase,
    TransferFunction, VideoFrameMetadata,
};
use fm_replay::{ReplaySegmentWriter, SegmentFileEvent, SegmentWriterConfig};
use fm_types::{ChannelLayout, FrameRate, InputId, SampleRate};
use freemixd::native_media::{NativeMediaRuntime, NativeSourcePlayback};

use crate::source_wait::SourceWait;

/// How long a failed run waits before the next starts.
pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(5);
/// How often the storage root's free space is sampled, at most.
const STORAGE_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// One-second fragments per segment file.
const FRAGMENTS_PER_SEGMENT: NonZeroU32 = NonZeroU32::MIN.saturating_add(1);
/// Frames a run's worker may fall behind before the run ends.
const MAX_QUEUED_FRAMES: usize = 50;
/// Longest a worker retries a full recorder queue for one frame.
const ENQUEUE_TIMEOUT: Duration = Duration::from_secs(2);
const ENQUEUE_RETRY: Duration = Duration::from_millis(2);
/// The name of every session directory starts with this, followed by
/// `<pid>-<16 hex digits>`.
pub(crate) const SESSION_PREFIX: &str = "freemixd-replay-";
/// The manifest a session writes for the next run to reload (OR-012).
const MANIFEST_FILE: &str = "manifest.tsv";
/// The manifest name after its segments were restored.
const RESTORED_MANIFEST_FILE: &str = "manifest.restored";
const SEGMENT_PREFIX: &str = "segment";
/// Most decoded audio blocks read from one segment file: the adapter's
/// bound, well over its two seconds of AAC frames.
const SEGMENT_AUDIO_BLOCKS: u32 = 256;
pub(crate) const AUDIO_RATE: u32 = 48_000;

/// What the recording needs from the engine.
pub(crate) trait ReplayHost {
    /// The sources to record and the storage root, while the media plane
    /// feeds the recording.
    fn recording(&self) -> Option<(Vec<InputId>, PathBuf)>;
    fn observe(&mut self, input: InputId, frame: u64, stream_frame: u64) -> bool;
    #[allow(clippy::too_many_arguments)]
    fn segment_closed(
        &mut self,
        input: InputId,
        index: u64,
        path: PathBuf,
        first_frame: u64,
        first_stream_frame: u64,
        frames: u64,
        bytes: u64,
    ) -> bool;
    fn take_pruned(&mut self) -> Vec<PathBuf>;
    /// Records the free bytes on the replay storage root, or `None` when it
    /// cannot be sampled (Protocol 2.125, OR-014).
    fn set_storage_free(&mut self, free: Option<u64>);
    /// Catalogs a segment restored from a previous run, observing every
    /// frame it holds so the segment resolves exactly (OR-012).
    #[allow(clippy::too_many_arguments)]
    fn restore_segment(
        &mut self,
        input: InputId,
        index: u64,
        path: PathBuf,
        first_frame: u64,
        first_stream_frame: u64,
        frames: u64,
        bytes: u64,
    ) -> bool {
        let closed = self.segment_closed(
            input,
            index,
            path,
            first_frame,
            first_stream_frame,
            frames,
            bytes,
        );
        if closed {
            for offset in 0..frames {
                let _ = self.observe(
                    input,
                    first_frame.saturating_add(offset),
                    first_stream_frame.saturating_add(offset),
                );
            }
        }
        closed
    }
}

impl ReplayHost for ControlService<Policy> {
    fn recording(&self) -> Option<(Vec<InputId>, PathBuf)> {
        (self.replay_feed() == EngineReplayFeed::External).then(|| {
            (
                self.replay_settings().sources.clone(),
                self.replay_root().to_path_buf(),
            )
        })
    }

    fn observe(&mut self, input: InputId, frame: u64, stream_frame: u64) -> bool {
        self.replay_observe(input, frame, stream_frame)
    }

    fn segment_closed(
        &mut self,
        input: InputId,
        index: u64,
        path: PathBuf,
        first_frame: u64,
        first_stream_frame: u64,
        frames: u64,
        bytes: u64,
    ) -> bool {
        self.replay_segment_closed(
            input,
            index,
            path,
            first_frame,
            first_stream_frame,
            frames,
            bytes,
        )
    }

    fn take_pruned(&mut self) -> Vec<PathBuf> {
        self.take_replay_pruned()
    }

    fn set_storage_free(&mut self, free: Option<u64>) {
        self.set_replay_storage_free(free);
    }
}

/// What a run's worker reports.
enum RunEvent {
    /// The recorder runs and takes frames.
    Ready,
    Closed {
        index: u64,
        path: PathBuf,
        frames: u64,
        bytes: u64,
    },
    Failed(String),
    /// A [`Job::Stop`] finished.
    Stopped,
}

enum Job {
    /// One frame's picture and the source's own audio planes.
    Frame(Option<Arc<CpuVideoFrame>>, Option<Vec<Vec<f32>>>),
    /// Drains the recorder and closes its last segment.
    #[cfg_attr(not(test), allow(dead_code))]
    Stop,
}

/// The muxed stream into rolling segment files.
struct SegmentSink {
    writer: ReplaySegmentWriter,
    events: Sender<RunEvent>,
}

impl SegmentSink {
    fn forward(&self, events: Vec<SegmentFileEvent>) {
        for event in events {
            if let SegmentFileEvent::Closed {
                index,
                path,
                video_frames,
                bytes,
            } = event
            {
                let _ = self.events.send(RunEvent::Closed {
                    index,
                    path,
                    frames: video_frames,
                    bytes,
                });
            }
        }
    }
}

impl RecordSink for SegmentSink {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let events = self.writer.push(bytes).map_err(io::Error::other)?;
        self.forward(events);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        let events = self.writer.finish().map_err(io::Error::other)?;
        self.forward(events);
        Ok(())
    }
}

/// The picture size, frame rate, and clock of the show.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReplayFormat {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rate: FrameRate,
    pub(crate) clock_domain: ClockDomainId,
}

/// One recording run of one source (see the module documentation).
struct Run {
    jobs: SyncSender<Job>,
    events: Receiver<RunEvent>,
    ready: bool,
    /// The stream frame of this run's first frame.
    base: u64,
    /// Frames handed to the worker.
    sent: u64,
    /// The engine frame of every frame sent but not in a closed segment.
    engine_frames: VecDeque<u64>,
    /// Frames in closed segments.
    closed: u64,
}

enum SourceState {
    Idle,
    Running(Run),
    Failed { at: Instant },
}

struct Source {
    /// The stream frame the next run starts at.
    next_base: u64,
    runs: u64,
    state: SourceState,
}

/// The replay recording and players of one native daemon.
/// The decode side of replay playback: one player per Replay input, owned by
/// the video realizer (structural item 1).
pub(crate) struct ReplayPlayers {
    format: ReplayFormat,
    players: BTreeMap<InputId, Player>,
    wait: SourceWait,
}

impl ReplayPlayers {
    pub(crate) fn new(format: ReplayFormat) -> Self {
        Self {
            format,
            players: BTreeMap::new(),
            wait: SourceWait::new(crate::native_files::MAX_MEDIA_FRAME_WAIT),
        }
    }

    pub(crate) fn present(
        &mut self,
        projected: &FrameResult,
        deadline: Instant,
        runtime: &NativeMediaRuntime,
        playback: &mut NativeSourcePlayback,
        push_audio: &mut dyn FnMut(InputId, &AudioBlock),
    ) -> bool {
        let frame = projected.frame.get();
        self.players
            .retain(|input, _| projected.replay.iter().any(|replay| replay.input == *input));
        let mut pending = false;
        for replay in &projected.replay {
            let player = self
                .players
                .entry(replay.input)
                .or_insert_with(|| Player::start(replay.input, self.format));
            let wanted = Shown::of(replay);
            if let Shown::Frame(path, index) = &wanted
                && player.requested.as_ref() != Some(&(path.clone(), *index))
            {
                player.request(path.clone(), *index);
            }
            if let Some(decoded) = player.take_decoded() {
                player.decoded = Some(decoded);
            }
            if !playback.registry().contains(replay.input) || player.shown == wanted {
                continue;
            }
            let picture = match &wanted {
                Shown::Nothing | Shown::Hold => continue,
                Shown::Blank => crate::native_files::output_timing(
                    frame,
                    self.format.rate,
                    self.format.clock_domain,
                )
                .and_then(|timing| {
                    crate::native_files::blank_picture(
                        (self.format.width, self.format.height),
                        timing,
                    )
                }),
                Shown::Frame(path, index) => match &player.decoded {
                    Some((key, picture, audio)) if key.0 == *path && key.1 == *index => {
                        // Forward play at 100% plays the frame's recorded
                        // audio; slow motion stays silent.
                        if replay.playing
                            && !replay.shuttling
                            && replay.speed == fm_types::replay::ReplaySpeed::Full
                            && let Some(block) = audio.as_deref().and_then(|planes| {
                                recorded_audio(
                                    self.format.rate,
                                    self.format.clock_domain,
                                    frame,
                                    planes,
                                )
                            })
                        {
                            push_audio(replay.input, &block);
                        }
                        picture
                            .as_ref()
                            .and_then(|picture| retimed(picture, frame, self.format))
                    }
                    _ => {
                        pending = true;
                        continue;
                    }
                },
            };
            player.shown = wanted;
            // A picture the source refuses is reported, never fatal: the
            // Replay input holds its previous picture.
            if let Some(picture) = picture
                && let Err(error) =
                    runtime.ingest_live_video_frame_blocking(playback, replay.input, picture)
            {
                eprintln!(
                    "FREEMIXD_REPLAY_FAILURE\tv=1\tinput={}\tfailure={}",
                    replay.input,
                    crate::diagnostic_field(&format!("present:{error}"))
                );
            }
        }
        !pending || self.wait.waited_out(frame, deadline, Instant::now())
    }
}

pub(crate) struct NativeReplay {
    format: ReplayFormat,
    /// The sources and root recorded, as the engine last asked.
    recording: Option<(Vec<InputId>, PathBuf)>,
    session: Option<PathBuf>,
    sources: BTreeMap<InputId, Source>,
    /// Whether a previous run's segments were already looked for.
    restored: bool,
    /// When the storage root was last sampled.
    storage_sample: Option<Instant>,
    /// The root's free bytes fell below [`Self::min_free_bytes`]; recording
    /// is paused until the next sample clears it (OR-014).
    low_space: bool,
    /// Free bytes a replay root keeps before recording pauses, matching the
    /// recorder's floor.
    min_free_bytes: u64,
}

impl NativeReplay {
    pub(crate) fn new(format: ReplayFormat) -> Self {
        Self {
            format,
            recording: None,
            session: None,
            sources: BTreeMap::new(),
            restored: false,
            storage_sample: None,
            low_space: false,
            min_free_bytes: crate::disk::RECORDING_MINIMUM_FREE_BYTES,
        }
    }

    /// Follows what the engine records, collects what the runs report, and
    /// deletes what retention released.
    pub(crate) fn sync(&mut self, host: &mut dyn ReplayHost) {
        let wanted = host.recording().filter(|(sources, _)| !sources.is_empty());
        if wanted != self.recording {
            self.end_session();
            self.recording = wanted;
            if let Some((sources, root)) = self.recording.clone() {
                self.sources = sources
                    .iter()
                    .map(|input| {
                        (
                            *input,
                            Source {
                                next_base: 0,
                                runs: 0,
                                state: SourceState::Idle,
                            },
                        )
                    })
                    .collect();
                // The first configuration after startup may find the
                // previous run's closed segments; later configurations
                // started a new recording and must not resurrect them.
                if !self.restored {
                    self.restored = true;
                    self.restore(&root, host);
                }
            }
        }
        let inputs: Vec<InputId> = self.sources.keys().copied().collect();
        // Sample the root before starting or stopping runs: a root below the
        // recorder's floor pauses recording, and a recovered one resumes it.
        self.sample_storage(host);
        if self.low_space {
            for input in inputs {
                if matches!(self.sources[&input].state, SourceState::Running(_)) {
                    self.fail(input, "storage:low_space");
                }
            }
        } else {
            for input in inputs {
                self.advance(host, input);
            }
        }
        for path in host.take_pruned() {
            let _ = fs::remove_file(path);
        }
    }

    /// Samples the storage root's free space at most once a second and
    /// reports it to the engine, in whole mebibytes so the lossy status
    /// does not churn on every write. A root below
    /// [`Self::min_free_bytes`] pauses recording (OR-014).
    fn sample_storage(&mut self, host: &mut dyn ReplayHost) {
        let Some((_, root)) = &self.recording else {
            self.storage_sample = None;
            self.low_space = false;
            host.set_storage_free(None);
            return;
        };
        let now = Instant::now();
        if self
            .storage_sample
            .is_some_and(|last| now.duration_since(last) < STORAGE_SAMPLE_INTERVAL)
        {
            return;
        }
        self.storage_sample = Some(now);
        let free = crate::disk::existing_ancestor(root)
            .and_then(|path| crate::disk::available_bytes(path).ok())
            .map(|free| free / (1024 * 1024) * (1024 * 1024));
        self.low_space = free.is_some_and(|free| free < self.min_free_bytes);
        host.set_storage_free(free);
    }

    /// Records engine frame `frame`: every running source's newest picture
    /// (`latest`) and its own audio of the frame (`audio`), then
    /// [`Self::sync`].
    pub(crate) fn record(
        &mut self,
        host: &mut dyn ReplayHost,
        frame: u64,
        latest: &dyn Fn(InputId) -> Option<Arc<CpuVideoFrame>>,
        audio: &dyn Fn(InputId) -> Option<Vec<Vec<f32>>>,
    ) {
        self.sync(host);
        let mut failed = Vec::new();
        for (&input, source) in &mut self.sources {
            let SourceState::Running(run) = &mut source.state else {
                continue;
            };
            if !run.ready {
                continue;
            }
            match run.jobs.try_send(Job::Frame(latest(input), audio(input))) {
                Ok(()) => {
                    host.observe(input, frame, run.base + run.sent);
                    run.engine_frames.push_back(frame);
                    run.sent += 1;
                }
                Err(TrySendError::Full(_)) => failed.push((input, "encode:behind".to_owned())),
                Err(TrySendError::Disconnected(_)) => {
                    failed.push((input, "encode:worker_lost".to_owned()));
                }
            }
        }
        for (input, failure) in failed {
            self.fail(input, &failure);
        }
    }

    fn advance(&mut self, host: &mut dyn ReplayHost, input: InputId) {
        let Some(source) = self.sources.get_mut(&input) else {
            return;
        };
        match &mut source.state {
            SourceState::Idle if !self.low_space => self.start(input),
            SourceState::Idle => {}
            SourceState::Failed { at } if !self.low_space && at.elapsed() >= RETRY_DELAY => {
                self.start(input);
            }
            SourceState::Failed { .. } => {}
            SourceState::Running(run) => {
                let mut failure = None;
                loop {
                    match run.events.try_recv() {
                        Ok(RunEvent::Ready) => run.ready = true,
                        Ok(RunEvent::Closed {
                            index,
                            path,
                            frames,
                            bytes,
                        }) => {
                            Self::catalog(
                                &self.session,
                                host,
                                input,
                                run,
                                index,
                                path,
                                frames,
                                bytes,
                            );
                        }
                        Ok(RunEvent::Failed(reason)) => {
                            failure = Some(reason);
                            break;
                        }
                        Ok(RunEvent::Stopped) => {}
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            failure = Some("worker_lost".to_owned());
                            break;
                        }
                    }
                }
                if let Some(failure) = failure {
                    self.fail(input, &failure);
                }
            }
        }
    }

    fn catalog(
        session: &Option<PathBuf>,
        host: &mut dyn ReplayHost,
        input: InputId,
        run: &mut Run,
        index: u64,
        path: PathBuf,
        frames: u64,
        bytes: u64,
    ) {
        let Some(&first_frame) = run.engine_frames.front() else {
            return;
        };
        let first_stream_frame = run.base + run.closed;
        let closed = host.segment_closed(
            input,
            index,
            path.clone(),
            first_frame,
            first_stream_frame,
            frames,
            bytes,
        );
        if closed && let Some(session) = session {
            append_segment_manifest(
                session,
                input,
                index,
                &path,
                first_frame,
                first_stream_frame,
                frames,
                bytes,
            );
        }
        let taken = usize::try_from(frames)
            .unwrap_or(usize::MAX)
            .min(run.engine_frames.len());
        run.engine_frames.drain(..taken);
        run.closed += frames;
    }

    /// Creates the session directory under the storage root, once.
    fn session(&mut self) -> Option<PathBuf> {
        if let Some(session) = &self.session {
            return Some(session.clone());
        }
        let (_, root) = self.recording.as_ref()?;
        fs::create_dir_all(root).ok()?;
        let mut suffix = [0_u8; 8];
        let _ = getrandom::fill(&mut suffix);
        let suffix = suffix.iter().fold(String::new(), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        });
        let session = root.join(format!("{SESSION_PREFIX}{}-{suffix}", std::process::id()));
        // A fresh, owner-only directory: one that already exists is refused.
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&session).ok()?;
        // The manifest lets the next daemon reload these closed segments.
        write_session_manifest(&session, self.format, self.sources.keys().copied()).ok()?;
        self.session = Some(session.clone());
        Some(session)
    }

    /// Reloads the newest compatible previous session's closed segments and
    /// removes the other sessions (OR-012).
    fn restore(&mut self, root: &Path, host: &mut dyn ReplayHost) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        let mut sessions: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(SESSION_PREFIX)
            })
            .map(|entry| entry.path())
            .collect();
        sessions.sort_by_key(|path| {
            std::cmp::Reverse(fs::metadata(path).and_then(|metadata| metadata.modified()).ok())
        });
        let mut restored_session = None;
        for session in &sessions {
            let manifest = session.join(MANIFEST_FILE);
            let Ok(text) = fs::read_to_string(&manifest) else {
                continue;
            };
            if !manifest_header_matches(&text, self.format) {
                continue;
            }
            for segment in parse_manifest_segments(&text, session) {
                if !self.sources.contains_key(&segment.input) || !segment.path.is_file() {
                    continue;
                }
                if host.restore_segment(
                    segment.input,
                    segment.index,
                    segment.path,
                    segment.first_frame,
                    segment.first_stream_frame,
                    segment.frames,
                    segment.bytes,
                ) && let Some(source) = self.sources.get_mut(&segment.input)
                {
                    source.next_base = source
                        .next_base
                        .max(segment.first_stream_frame.saturating_add(segment.frames));
                }
            }
            // The files stay for playback; the manifest is marked so the
            // next restart does not catalog them twice.
            let _ = fs::rename(&manifest, session.join(RESTORED_MANIFEST_FILE));
            restored_session = Some(session.clone());
            break;
        }
        for session in &sessions {
            if restored_session.as_ref() == Some(session) {
                continue;
            }
            let _ = fs::remove_dir_all(session);
        }
    }

    fn start(&mut self, input: InputId) {
        let Some(session) = self.session() else {
            self.fail(input, "configure:directory");
            return;
        };
        let Some(source) = self.sources.get_mut(&input) else {
            return;
        };
        source.runs += 1;
        let directory = session.join(format!("{input}-{}", source.runs));
        if fs::create_dir(&directory).is_err() {
            self.fail(input, "configure:directory");
            return;
        }
        let (jobs, queue) = mpsc::sync_channel(MAX_QUEUED_FRAMES);
        let (sender, events) = mpsc::channel();
        let format = self.format;
        let spawned = thread::Builder::new()
            .name(format!("freemixd-replay-record-{input}"))
            .spawn(move || record_run(format, &directory, &queue, &sender));
        if spawned.is_err() {
            self.fail(input, "start:thread");
            return;
        }
        source.state = SourceState::Running(Run {
            jobs,
            events,
            ready: false,
            base: source.next_base,
            sent: 0,
            engine_frames: VecDeque::new(),
            closed: 0,
        });
    }

    fn fail(&mut self, input: InputId, failure: &str) {
        eprintln!(
            "FREEMIXD_REPLAY_FAILURE\tv=1\tinput={input}\tfailure={}",
            crate::diagnostic_field(failure)
        );
        if let Some(source) = self.sources.get_mut(&input) {
            if let SourceState::Running(run) = &source.state {
                source.next_base = run.base + run.sent;
            }
            // Dropping the run's queue makes its worker cancel the recorder.
            source.state = SourceState::Failed { at: Instant::now() };
        }
    }

    /// Ends every run and removes the session directory.
    fn end_session(&mut self) {
        self.sources.clear();
        if let Some(session) = self.session.take() {
            let _ = thread::Builder::new()
                .name("freemixd-replay-remove".into())
                .spawn(move || {
                    // The runs' workers cancel their recorders first.
                    thread::sleep(Duration::from_secs(3));
                    let _ = fs::remove_dir_all(session);
                });
        }
    }

    /// Drains every run and catalogs its last segment, waiting up to
    /// `timeout`; the runs end.
    #[cfg(test)]
    fn flush(&mut self, host: &mut dyn ReplayHost, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        for source in self.sources.values_mut() {
            let SourceState::Running(run) = &mut source.state else {
                continue;
            };
            let _ = run.jobs.send(Job::Stop);
        }
        let inputs: Vec<InputId> = self.sources.keys().copied().collect();
        for input in inputs {
            while let Some(Source {
                state: SourceState::Running(run),
                ..
            }) = self.sources.get_mut(&input)
            {
                match run.events.recv_timeout(Duration::from_millis(50)) {
                    Ok(RunEvent::Closed {
                        index,
                        path,
                        frames,
                        bytes,
                    }) => Self::catalog(
                        &self.session,
                        host,
                        input,
                        run,
                        index,
                        path,
                        frames,
                        bytes,
                    ),
                    Ok(RunEvent::Stopped) => {
                        self.sources.get_mut(&input).expect("source").state = SourceState::Idle;
                        break;
                    }
                    Ok(RunEvent::Ready) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Ok(RunEvent::Failed(failure)) => panic!("{failure}"),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                assert!(Instant::now() < deadline, "replay flush timed out");
            }
        }
        for path in host.take_pruned() {
            let _ = fs::remove_file(path);
        }
    }

    /// Whether every source's run takes frames.
    #[cfg(test)]
    fn ready(&self) -> bool {
        !self.sources.is_empty()
            && self
                .sources
                .values()
                .all(|source| matches!(&source.state, SourceState::Running(run) if run.ready))
    }
}

impl Drop for NativeReplay {
    fn drop(&mut self) {
        // Keep the session and its manifest so the next daemon reloads its
        // closed segments (OR-012). The workers stop with the process; a
        // segment they did not close is not in the manifest.
        self.sources.clear();
        self.session = None;
    }
}

/// Writes a new session's manifest header: the format and the sources.
fn write_session_manifest(
    session: &Path,
    format: ReplayFormat,
    sources: impl Iterator<Item = InputId>,
) -> io::Result<()> {
    let sources = sources
        .map(|input| input.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let text = format!(
        "format\t{}\t{}\t{}\t{}\t{}\t{}\n",
        format.width,
        format.height,
        format.rate.numerator(),
        format.rate.denominator(),
        format.clock_domain.get(),
        sources
    );
    fs::write(session.join(MANIFEST_FILE), text)
}

/// Whether `text`'s header describes the same picture format and clock.
fn manifest_header_matches(text: &str, format: ReplayFormat) -> bool {
    let Some(line) = text.lines().next() else {
        return false;
    };
    let parts: Vec<&str> = line.split('\t').collect();
    let ["format", width, height, numerator, denominator, clock, _sources] = parts.as_slice()
    else {
        return false;
    };
    width.parse::<u32>().ok() == Some(format.width)
        && height.parse::<u32>().ok() == Some(format.height)
        && numerator.parse::<u32>().ok() == Some(format.rate.numerator())
        && denominator.parse::<u32>().ok() == Some(format.rate.denominator())
        && clock.parse::<u128>().ok().and_then(NonZeroU128::new)
            == Some(format.clock_domain.get())
}

/// One closed segment a session manifest names.
struct RestoredSegment {
    input: InputId,
    index: u64,
    path: PathBuf,
    first_frame: u64,
    first_stream_frame: u64,
    frames: u64,
    bytes: u64,
}

/// Parses a session's segment lines, skipping a torn or malformed one.
fn parse_manifest_segments(text: &str, session: &Path) -> Vec<RestoredSegment> {
    let mut segments = Vec::new();
    for line in text.lines().skip(1) {
        let parts: Vec<&str> = line.split('\t').collect();
        let ["segment", input, index, first_frame, first_stream_frame, frames, bytes, relative] =
            parts.as_slice()
        else {
            continue;
        };
        let Some(input) = input
            .parse::<u128>()
            .ok()
            .and_then(NonZeroU128::new)
            .map(InputId::new)
        else {
            continue;
        };
        let (
            Ok(index),
            Ok(first_frame),
            Ok(first_stream_frame),
            Ok(frames),
            Ok(bytes),
        ) = (
            index.parse::<u64>(),
            first_frame.parse::<u64>(),
            first_stream_frame.parse::<u64>(),
            frames.parse::<u64>(),
            bytes.parse::<u64>(),
        )
        else {
            continue;
        };
        if frames == 0 {
            continue;
        }
        let path = session.join(relative);
        if !path.starts_with(session) {
            continue;
        }
        segments.push(RestoredSegment {
            input,
            index,
            path,
            first_frame,
            first_stream_frame,
            frames,
            bytes,
        });
    }
    segments.sort_by_key(|segment| segment.first_frame);
    segments
}

/// Appends one closed segment to its session's manifest.
fn append_segment_manifest(
    session: &Path,
    input: InputId,
    index: u64,
    path: &Path,
    first_frame: u64,
    first_stream_frame: u64,
    frames: u64,
    bytes: u64,
) {
    let Ok(relative) = path.strip_prefix(session) else {
        return;
    };
    let Some(relative) = relative.to_str() else {
        return;
    };
    if relative.contains(['\t', '\n', '\r']) {
        return;
    }
    let line = format!(
        "segment\t{input}\t{index}\t{first_frame}\t{first_stream_frame}\t{frames}\t{bytes}\t\
         {relative}\n"
    );
    let manifest = session.join(MANIFEST_FILE);
    if let Ok(mut file) = fs::OpenOptions::new().append(true).open(manifest) {
        let _ = std::io::Write::write_all(&mut file, line.as_bytes());
    }
}

/// One run's worker: starts the recorder, then encodes every frame handed
/// over until the queue closes (cancel, files removed), [`Job::Stop`]
/// arrives (drain), or the recorder fails (cancel, files kept).
fn record_run(
    format: ReplayFormat,
    directory: &Path,
    queue: &Receiver<Job>,
    events: &Sender<RunEvent>,
) {
    let fail = |failure: String| {
        let _ = events.send(RunEvent::Failed(failure));
    };
    let Some(sample_rate) = SampleRate::new(AUDIO_RATE) else {
        return;
    };
    let record_format = match RecordFormat::new(
        format.width,
        format.height,
        format.rate,
        sample_rate,
        ChannelLayout::stereo(),
        SequenceNumber::new(0),
    ) {
        Ok(record_format) => record_format,
        Err(error) => return fail(format!("format:{error:?}")),
    };
    let sink = SegmentSink {
        writer: ReplaySegmentWriter::new(SegmentWriterConfig {
            directory: directory.to_path_buf(),
            prefix: SEGMENT_PREFIX.to_owned(),
            fragments_per_segment: FRAGMENTS_PER_SEGMENT,
            max_unit_bytes: fm_replay::DEFAULT_SEGMENT_UNIT_BYTES,
        }),
        events: events.clone(),
    };
    let mut config = RecordConfig::new(record_format.clone());
    config.preset = Some(X264Preset::Veryfast);
    config.square_pixels = true;
    config.limits = RecordLimits {
        max_outstanding_pairs: 8,
        ..RecordLimits::default()
    };
    let mut recorder = match Recorder::start_with_sink(Box::new(sink), config) {
        Ok(recorder) => recorder,
        Err(error) => return fail(format!("start:{error}")),
    };
    let _ = events.send(RunEvent::Ready);
    let mut sequence = 0_u64;
    loop {
        match queue.recv() {
            Ok(Job::Frame(picture, audio)) => {
                let Some(pair) = pair(
                    &record_format,
                    format,
                    sequence,
                    picture.as_deref(),
                    audio.as_deref(),
                ) else {
                    let _ = recorder.cancel();
                    return fail("frame:invalid".to_owned());
                };
                sequence += 1;
                if let Err(failure) = enqueue(&mut recorder, pair) {
                    let _ = recorder.cancel();
                    return fail(failure);
                }
            }
            Ok(Job::Stop) => {
                let _ = recorder.stop();
                let _ = events.send(RunEvent::Stopped);
                return;
            }
            Err(mpsc::RecvError) => {
                let _ = recorder.cancel();
                let _ = fs::remove_dir_all(directory);
                return;
            }
        }
    }
}

/// Hands `pair` to the recorder, retrying a full queue for a bound.
pub(crate) fn enqueue(recorder: &mut Recorder, pair: PairedFrame) -> Result<(), String> {
    let deadline = Instant::now() + ENQUEUE_TIMEOUT;
    let mut pair = pair;
    loop {
        match recorder.enqueue(pair) {
            Ok(()) => return Ok(()),
            Err(error)
                if matches!(
                    error.reason,
                    EnqueueRejection::QueueFull | EnqueueRejection::RetainedByteLimit
                ) && Instant::now() < deadline =>
            {
                pair = error.into_frame();
                thread::sleep(ENQUEUE_RETRY);
            }
            Err(error) => return Err(format!("enqueue:{:?}", error.reason)),
        }
    }
}

/// The recorded pair of stream frame `sequence`: `picture` scaled to the
/// project size (black without one), with the source's `audio` (silence
/// without it).
fn pair(
    record_format: &RecordFormat,
    format: ReplayFormat,
    sequence: u64,
    picture: Option<&CpuVideoFrame>,
    audio: Option<&[Vec<f32>]>,
) -> Option<PairedFrame> {
    let rgba = picture
        .and_then(|picture| {
            crate::scene_preview::sampled_live_picture(picture, format.width, format.height)
        })
        .map_or_else(
            || {
                let pixels =
                    usize::try_from(format.width).ok()? * usize::try_from(format.height).ok()?;
                Some([0, 0, 0, u8::MAX].repeat(pixels))
            },
            |image| Some(image.pixels().to_vec()),
        )?;
    let audio = match audio {
        Some(planes) => recorded_audio(format.rate, format.clock_domain, sequence, planes)?,
        None => silence(format.rate, format.clock_domain, sequence)?,
    };
    PairedFrame::new(record_format, SequenceNumber::new(sequence), rgba, audio).ok()
}

/// A source's own `planes` as stereo audio of stream frame `sequence`: a
/// mono source on both channels, the first two of more, each plane
/// stretched to the frame's span on the recorder's sample grid; silence for
/// a source without planes.
pub(crate) fn recorded_audio(
    rate: FrameRate,
    clock_domain: ClockDomainId,
    sequence: u64,
    planes: &[Vec<f32>],
) -> Option<AudioBlock> {
    let (timing, samples) = frame_audio_span(rate, clock_domain, sequence)?;
    let fit = |plane: &[f32]| {
        if plane.len() == samples {
            plane.to_vec()
        } else {
            fm_engine::fm_playback::resample_linear(plane, samples)
        }
    };
    let stereo = match planes {
        [] => return silence(rate, clock_domain, sequence),
        [mono] => vec![fit(mono), fit(mono)],
        [left, right, ..] => vec![fit(left), fit(right)],
    };
    AudioBlock::new(
        timing,
        SampleRate::new(AUDIO_RATE)?,
        ChannelLayout::stereo(),
        stereo,
    )
    .ok()
}

/// The timing and sample count of stream frame `sequence` at
/// [`AUDIO_RATE`], on the recorder's absolute sample grid.
fn frame_audio_span(
    rate: FrameRate,
    clock_domain: ClockDomainId,
    sequence: u64,
) -> Option<(MediaTiming, usize)> {
    let hertz = u128::from(AUDIO_RATE);
    let boundary = |frame: u64| {
        u128::from(frame) * hertz * u128::from(rate.denominator()) / u128::from(rate.numerator())
    };
    let start = boundary(sequence);
    let end = boundary(sequence.checked_add(1)?);
    let start_nanos = start * 1_000_000_000 / hertz;
    let end_nanos = end * 1_000_000_000 / hertz;
    let timing = MediaTiming::new(
        OriginalTimestamp::new(
            MediaTimestamp::new(i64::try_from(start).ok()?),
            TimeBase::new(1, AUDIO_RATE).ok()?,
        ),
        NormalizedTimestamp::from_nanos(i64::try_from(start_nanos).ok()?),
        NormalizedDuration::from_nanos(u64::try_from(end_nanos - start_nanos).ok()?).ok()?,
        clock_domain,
        SequenceNumber::new(sequence),
    )
    .ok()?;
    Some((timing, usize::try_from(end - start).ok()?))
}

/// Silent stereo audio of stream frame `sequence` at [`AUDIO_RATE`], on the
/// recorder's absolute sample grid.
pub(crate) fn silence(
    rate: FrameRate,
    clock_domain: ClockDomainId,
    sequence: u64,
) -> Option<AudioBlock> {
    let (timing, samples) = frame_audio_span(rate, clock_domain, sequence)?;
    AudioBlock::silence(
        timing,
        SampleRate::new(AUDIO_RATE)?,
        ChannelLayout::stereo(),
        samples,
    )
    .ok()
}

/// What a Replay input shows.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Shown {
    /// Nothing yet (a new player).
    Nothing,
    /// Black: nothing is cued.
    Blank,
    /// The cued frame is not in a finished segment yet: the picture holds.
    Hold,
    Frame(PathBuf, u64),
}

impl Shown {
    fn of(replay: &EngineReplayFrame) -> Self {
        match (&replay.event, &replay.location) {
            (None, _) => Self::Blank,
            (Some(_), None) => Self::Hold,
            (Some(_), Some(location)) => Self::Frame(location.path.clone(), location.frame),
        }
    }
}

/// A decoded request: its picture and the frame's recorded audio planes.
type Decoded = (
    (PathBuf, u64),
    Option<Arc<CpuVideoFrame>>,
    Option<Vec<Vec<f32>>>,
);

#[derive(Default)]
struct DecodeSlot {
    request: Option<(PathBuf, u64)>,
    done: Option<Decoded>,
    stopped: bool,
}

struct DecodeShared {
    slot: Mutex<DecodeSlot>,
    wake: Condvar,
}

impl DecodeShared {
    fn lock(&self) -> std::sync::MutexGuard<'_, DecodeSlot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One Replay input's decode worker.
struct Player {
    shared: Arc<DecodeShared>,
    requested: Option<(PathBuf, u64)>,
    decoded: Option<Decoded>,
    shown: Shown,
}

impl Player {
    fn start(input: InputId, format: ReplayFormat) -> Self {
        let shared = Arc::new(DecodeShared {
            slot: Mutex::new(DecodeSlot::default()),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        let _ = thread::Builder::new()
            .name(format!("freemixd-replay-play-{input}"))
            .spawn(move || decode_worker(&worker, format));
        Self {
            shared,
            requested: None,
            decoded: None,
            shown: Shown::Nothing,
        }
    }

    fn request(&mut self, path: PathBuf, frame: u64) {
        self.requested = Some((path.clone(), frame));
        self.shared.lock().request = Some((path, frame));
        self.shared.wake.notify_one();
    }

    fn take_decoded(&self) -> Option<Decoded> {
        self.shared.lock().done.take()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.lock().stopped = true;
        self.shared.wake.notify_one();
    }
}

fn decode_worker(shared: &DecodeShared, format: ReplayFormat) {
    let mut decoder = SegmentDecoder::new(format);
    loop {
        let request = {
            let mut slot = shared.lock();
            loop {
                if slot.stopped {
                    return;
                }
                if let Some(request) = slot.request.take() {
                    break request;
                }
                slot = shared
                    .wake
                    .wait(slot)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let picture = decoder.decode(&request.0, request.1);
        let audio = decoder.audio(&request.0, request.1);
        shared.lock().done = Some((request, picture, audio));
    }
}

/// Decodes exact frames of segment files, keeping the last file open.
pub(crate) struct SegmentDecoder {
    format: ReplayFormat,
    adapter: Option<Adapter>,
    open: Option<(PathBuf, FilePlayer)>,
    /// The audio of the last segment asked for, decoded whole (two seconds
    /// of stereo), and the samples by which its first video frame starts
    /// after its first audio sample.
    audio: Option<(PathBuf, Option<SegmentAudio>)>,
}

/// A segment's decoded audio planes and the samples its first video frame
/// starts after them.
type SegmentAudio = (Vec<Vec<f32>>, i128);

impl SegmentDecoder {
    pub(crate) fn new(format: ReplayFormat) -> Self {
        Self {
            format,
            adapter: None,
            open: None,
            audio: None,
        }
    }

    /// The recorded audio planes of frame `frame` of segment file `path`:
    /// the segment's audio from the frame's first sample on the recorder's
    /// grid, one frame long; `None` for a segment without audio. The grid
    /// starts at the segment's first video frame, not its first audio
    /// sample: the encoder's B-frame delay starts the pictures 80 ms after
    /// the sound, and reading the audio from its own start played it that
    /// much early.
    pub(crate) fn audio(&mut self, path: &Path, frame: u64) -> Option<Vec<Vec<f32>>> {
        if self.audio.as_ref().is_none_or(|(open, _)| open != path) {
            let video_start = self
                .decode(path, 0)
                .map(|picture| picture.timing().original_timestamp());
            if self.adapter.is_none() {
                self.adapter = Adapter::new(fm_codec_ffmpeg::Config::default()).ok();
            }
            let planes = self.adapter.as_ref().and_then(|adapter| {
                let decoded = adapter
                    .decode_local_up_to(
                        path,
                        fm_codec_ffmpeg::DecodeRequest {
                            clock_domain: self.format.clock_domain,
                            video: None,
                            audio: Some(fm_codec_ffmpeg::SequenceRequest {
                                selector: fm_codec_ffmpeg::StreamSelector::Best,
                                count: NonZeroU32::new(SEGMENT_AUDIO_BLOCKS)?,
                            }),
                        },
                    )
                    .ok()?;
                let audio_start = decoded.audio.first()?.timing().original_timestamp();
                let mut planes: Vec<Vec<f32>> = Vec::new();
                for block in &decoded.audio {
                    if planes.is_empty() {
                        planes = vec![Vec::new(); block.planes().len()];
                    }
                    for (plane, samples) in planes.iter_mut().zip(block.planes()) {
                        plane.extend_from_slice(samples);
                    }
                }
                let offset =
                    video_start.map_or(Some(0), |video| samples_between(audio_start, video))?;
                (!planes.is_empty()).then_some((planes, offset))
            });
            self.audio = Some((path.to_path_buf(), planes));
        }
        let (planes, offset) = self.audio.as_ref()?.1.as_ref()?;
        let rate = self.format.rate;
        let boundary = |frame: u64| {
            let sample =
                i128::from(frame) * i128::from(AUDIO_RATE) * i128::from(rate.denominator())
                    / i128::from(rate.numerator())
                    + offset;
            usize::try_from(sample.max(0)).ok()
        };
        let (start, end) = (boundary(frame)?, boundary(frame.checked_add(1)?)?);
        planes
            .iter()
            .map(|plane| plane.get(start..end.min(plane.len())).map(<[f32]>::to_vec))
            .collect::<Option<Vec<_>>>()
            .filter(|planes| planes.first().is_some_and(|plane| !plane.is_empty()))
    }

    /// Frame `frame` of segment file `path`.
    pub(crate) fn decode(&mut self, path: &Path, frame: u64) -> Option<Arc<CpuVideoFrame>> {
        if self.open.as_ref().is_none_or(|(open, _)| open != path) {
            self.open = None;
            if self.adapter.is_none() {
                self.adapter = Adapter::new(fm_codec_ffmpeg::Config::default()).ok();
            }
            let mut config = FilePlaybackConfig::new(self.format.clock_domain, self.format.rate);
            config.audio = None;
            let player = self.adapter.as_ref()?.open_playback(path, config).ok()?;
            self.open = Some((path.to_path_buf(), player));
        }
        let (_, player) = self.open.as_mut()?;
        player.decode_frame(FrameIndex::new(frame)).ok()
    }
}

/// The samples at [`AUDIO_RATE`] from `from` to `to` (negative when `to`
/// comes first), rounded to the nearest sample.
fn samples_between(from: OriginalTimestamp, to: OriginalTimestamp) -> Option<i128> {
    let seconds = |stamp: OriginalTimestamp| {
        let base = stamp.time_base();
        (
            i128::from(stamp.timestamp().ticks()) * i128::from(base.numerator()),
            i128::from(base.denominator()),
        )
    };
    let ((from_num, from_den), (to_num, to_den)) = (seconds(from), seconds(to));
    let denominator = from_den.checked_mul(to_den)?;
    let numerator = (to_num * from_den - from_num * to_den).checked_mul(i128::from(AUDIO_RATE))?;
    let half = denominator / 2;
    Some(if numerator >= 0 {
        (numerator + half) / denominator
    } else {
        (numerator - half) / denominator
    })
}

/// `picture` on the program frame grid at engine frame `frame`.
pub(crate) fn retimed(
    picture: &CpuVideoFrame,
    frame: u64,
    format: ReplayFormat,
) -> Option<CpuVideoFrame> {
    let timing = crate::native_files::output_timing(frame, format.rate, format.clock_domain)?;
    let retimed = CpuVideoFrame::new(timing, picture.payload().clone());
    // The recording declares no colour: its pictures were straight sRGB.
    let metadata = picture.metadata().unwrap_or_else(|| {
        VideoFrameMetadata::new(
            ColorMetadata {
                primaries: ColorPrimaries::Bt709,
                transfer: TransferFunction::Srgb,
                matrix: MatrixCoefficients::Identity,
                range: SignalRange::Full,
                chroma_location: ChromaLocation::Center,
            },
            Some(AlphaMode::Straight),
        )
    });
    retimed.with_metadata(metadata).ok()
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;
    use std::process::{Command, Stdio};

    use fm_command::{CommandEnvelope, IdempotencyKey};
    use fm_engine::{Engine, EngineCommand, EngineInputSource, EngineReplayOperation, ShowState};
    use fm_frame::{CpuVideoPayload, CpuVideoPlane};
    use fm_types::{PixelFormat, VideoDimensions, replay::ReplaySettings, replay::ReplaySpeed};

    use super::*;

    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 36;

    impl ReplayHost for Engine {
        fn recording(&self) -> Option<(Vec<InputId>, PathBuf)> {
            (self.replay_feed() == EngineReplayFeed::External).then(|| {
                (
                    self.show().replay().sources.clone(),
                    self.replay_root().to_path_buf(),
                )
            })
        }

        fn observe(&mut self, input: InputId, frame: u64, stream_frame: u64) -> bool {
            self.replay_observe(input, frame, stream_frame)
        }

        fn segment_closed(
            &mut self,
            input: InputId,
            index: u64,
            path: PathBuf,
            first_frame: u64,
            first_stream_frame: u64,
            frames: u64,
            bytes: u64,
        ) -> bool {
            self.replay_segment_closed(
                input,
                index,
                path,
                first_frame,
                first_stream_frame,
                frames,
                bytes,
            )
        }

        fn take_pruned(&mut self) -> Vec<PathBuf> {
            self.take_replay_pruned()
        }

        fn set_storage_free(&mut self, free: Option<u64>) {
            self.set_replay_storage_free(free);
        }
    }

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn ffmpeg_available() -> bool {
        let available = ["ffmpeg", "ffprobe"].iter().all(|tool| {
            Command::new(tool)
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        });
        let required = ["FM_REQUIRE_FFMPEG", "FM_REQUIRE_NATIVE_MEDIA"]
            .iter()
            .any(|name| std::env::var(name).as_deref() == Ok("1"));
        assert!(
            available || !required,
            "ffmpeg/ffprobe are required but unavailable"
        );
        available
    }

    /// The gray level recorded on engine frame `frame`: distinct for 31
    /// consecutive frames and far from its neighbours.
    fn level(frame: u64) -> u8 {
        u8::try_from(16 + (frame * 7) % 217).unwrap()
    }

    fn picture(frame: u64) -> Arc<CpuVideoFrame> {
        let gray = level(frame);
        let bytes = [gray, gray, gray, u8::MAX].repeat((WIDTH * HEIGHT) as usize);
        let payload = CpuVideoPayload::new(
            PixelFormat::Rgba8,
            VideoDimensions::new(WIDTH, HEIGHT).unwrap(),
            vec![CpuVideoPlane::new(WIDTH as usize * 4, bytes).unwrap()],
        )
        .unwrap();
        let timing = crate::native_files::output_timing(
            frame,
            FrameRate::new(25, 1).unwrap(),
            clock_domain(),
        )
        .unwrap();
        Arc::new(
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
                .unwrap(),
        )
    }

    fn clock_domain() -> ClockDomainId {
        ClockDomainId::new(NonZeroU128::new(100).unwrap())
    }

    fn accept(engine: &mut Engine, key: &str, command: EngineCommand) {
        let outcome = engine
            .execute(
                CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
                0,
            )
            .unwrap();
        assert!(outcome.receipt.accepted().is_some(), "{key}");
    }

    /// The gray level of a decoded picture's first pixel.
    #[test]
    fn samples_between_timestamps_round_to_the_nearest_sample() {
        let stamp = |ticks, denominator| {
            OriginalTimestamp::new(
                fm_types::MediaTimestamp::new(ticks),
                fm_types::TimeBase::new(1, denominator).unwrap(),
            )
        };
        // 2.0 s of audio to 2.08 s of video.
        assert_eq!(
            super::samples_between(stamp(96_000, 48_000), stamp(26_624, 12_800)),
            Some(3_840)
        );
        assert_eq!(
            super::samples_between(stamp(26_624, 12_800), stamp(96_000, 48_000)),
            Some(-3_840)
        );
        // A third of a sample at 48 kHz rounds away; two thirds rounds up.
        assert_eq!(
            super::samples_between(stamp(0, 144_000), stamp(1, 144_000)),
            Some(0)
        );
        assert_eq!(
            super::samples_between(stamp(0, 144_000), stamp(2, 144_000)),
            Some(1)
        );
    }

    fn decoded_level(picture: &CpuVideoFrame) -> u8 {
        let payload = picture.payload();
        assert!(
            matches!(payload.format(), PixelFormat::Rgba8 | PixelFormat::Bgra8),
            "{:?}",
            payload.format()
        );
        payload.plane(0).unwrap().bytes()[1]
    }

    /// The recording writes real segment files that retention deletes
    /// unless an event holds them, and a Replay input at half speed shows
    /// every recorded frame of its event, decoded from those files, on
    /// exactly two consecutive frames.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn recorded_segments_play_frame_exactly_at_half_speed_and_retention_keeps_events() {
        if !ffmpeg_available() {
            eprintln!("skipped: ffmpeg/ffprobe unavailable");
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("replay");
        let rate = FrameRate::new(25, 1).unwrap();
        let show = ShowState::new(
            "replay",
            (1..=2)
                .map(|value| (input(value), format!("Camera {value}")))
                .collect(),
            input(1),
            input(2),
        )
        .unwrap();
        let mut engine = Engine::new(
            show,
            rate,
            fm_clock::ClockDomainId::new(NonZeroU128::new(100).unwrap()),
        );
        engine.set_replay_feed(EngineReplayFeed::External);
        accept(
            &mut engine,
            "player",
            EngineCommand::AddInput {
                input: input(9),
                name: "Replay".into(),
                source: EngineInputSource::Replay,
            },
        );
        accept(
            &mut engine,
            "configure",
            EngineCommand::Replay(EngineReplayOperation::Configure(Box::new(ReplaySettings {
                sources: vec![input(1)],
                retention_seconds: 10,
                storage_root: Some(root.to_string_lossy().into_owned()),
                auto_return: None,
                transition: fm_types::replay::ReplayTransition::CUT,
            }))),
        );
        let format = ReplayFormat {
            width: WIDTH,
            height: HEIGHT,
            rate,
            clock_domain: clock_domain(),
        };
        let mut replay = NativeReplay::new(format);
        let mut frame = engine.tick().unwrap().frame.get();
        let started = Instant::now();
        while !replay.ready() {
            replay.sync(&mut engine);
            assert!(started.elapsed() < Duration::from_secs(20), "never ready");
            thread::sleep(Duration::from_millis(10));
        }
        // An event of the first five seconds, then sixteen seconds more:
        // retention (ten seconds) releases what is older and no event holds.
        let mut event_frames = None;
        for step in 0..525 {
            // A mono 1 kHz tone at half scale: the source's own audio.
            let tone: Vec<f32> = (0..1_920)
                .map(|sample| {
                    0.5 * (f32::from(u16::try_from(sample).unwrap()) * core::f32::consts::TAU
                        / 48.0)
                        .sin()
                })
                .collect();
            replay.record(&mut engine, frame, &|_| Some(picture(frame)), &|_| {
                Some(vec![tone.clone()])
            });
            if step == 125 {
                accept(
                    &mut engine,
                    "capture",
                    EngineCommand::Replay(EngineReplayOperation::CaptureLast { seconds: 5 }),
                );
            }
            let result = engine.tick().unwrap();
            frame = result.frame.get();
            if event_frames.is_none()
                && let Some(event) = engine.replay_status().events.first()
            {
                event_frames = Some((event.id, event.in_frame, event.out_frame));
            }
            // The encoder keeps up with real time, not with a tight loop.
            thread::sleep(Duration::from_millis(2));
        }
        replay.flush(&mut engine, Duration::from_secs(30));
        let (event, in_frame, out_frame) = event_frames.expect("the event was captured");
        assert_eq!(out_frame - in_frame, 125);
        let status = engine.replay_status();
        assert!(status.events[0].complete, "{status:?}");
        let session = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_dir())
            .unwrap();
        let files: Vec<PathBuf> = fs::read_dir(session.join(format!("{}-1", input(1))))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        let retained = usize::try_from(status.sources[0].segments).unwrap();
        assert_eq!(files.len(), retained, "{files:?}");
        // The event's segments (its first five seconds) stay; the two after
        // them ended more than ten seconds behind the live edge and are
        // deleted.
        let segment = |index: u64| {
            fm_replay::segment_path(&session.join(format!("{}-1", input(1))), "segment", index)
        };
        for index in 1..=3 {
            assert!(files.contains(&segment(index)), "{index}: {files:?}");
        }
        for index in 4..=5 {
            assert!(!segment(index).exists(), "{index}: {files:?}");
        }
        // The segments carry the source's own audio on both channels.
        let pcm = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(segment(1))
            .args(["-map", "0:a", "-f", "f32le", "-ac", "2", "-"])
            .output()
            .unwrap();
        assert!(pcm.status.success(), "{pcm:?}");
        let samples: Vec<f32> = pcm
            .stdout
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        assert!(samples.len() >= 48_000 * 2, "{} samples", samples.len());
        #[allow(clippy::cast_precision_loss)] // a few hundred thousand samples
        let rms =
            (samples.iter().map(|value| value * value).sum::<f32>() / samples.len() as f32).sqrt();
        // A half-scale sine has an RMS of about 0.35; AAC keeps it close.
        assert!((0.3..0.4).contains(&rms), "recorded audio RMS {rms}");
        // The Replay input's decoder reads one frame of it back, the run's
        // first segment too: it opens with the encoder's priming packet
        // ahead of a gap, which the adapter skips, and its first decoded
        // block starts with its first picture.
        let mut decoder = SegmentDecoder::new(format);
        let first = decoder
            .audio(&segment(1), 10)
            .expect("the first segment's audio");
        assert_eq!(first[0].len(), 1_920);
        assert!(first[0].iter().any(|sample| sample.abs() > 0.3));
        assert_eq!(
            decoder
                .audio
                .as_ref()
                .and_then(|(_, audio)| audio.as_ref())
                .map(|audio| audio.1),
            Some(0)
        );
        let planes = decoder.audio(&segment(2), 10).expect("recorded audio");
        // The segment's pictures start two 25 fps frames (the encoder's
        // B-frame delay) after its sound, so frame 0's audio is read 3840
        // samples in.
        assert_eq!(
            decoder
                .audio
                .as_ref()
                .and_then(|(_, audio)| audio.as_ref())
                .map(|audio| audio.1),
            Some(3_840)
        );
        assert_eq!(planes.len(), 2);
        assert_eq!(planes[0].len(), 1_920);
        assert!(planes[0].iter().any(|sample| sample.abs() > 0.3));
        assert!(status.sources[0].first_frame.unwrap() <= in_frame);

        accept(
            &mut engine,
            "cue",
            EngineCommand::Replay(EngineReplayOperation::Cue {
                input: input(9),
                event,
                angle: None,
            }),
        );
        accept(
            &mut engine,
            "play",
            EngineCommand::Replay(EngineReplayOperation::Play {
                input: input(9),
                speed: ReplaySpeed::Half,
            }),
        );
        let mut decoder = SegmentDecoder::new(format);
        let mut shown = Vec::new();
        for _ in 0..80 {
            let result = engine.tick().unwrap();
            let Some(player) = result.replay.iter().find(|replay| replay.input == input(9)) else {
                continue;
            };
            if !player.playing {
                continue;
            }
            let source = player.source_frame.unwrap();
            let location = player.location.as_ref().expect("recorded");
            let picture = decoder.decode(&location.path, location.frame).unwrap();
            let actual = decoded_level(&picture);
            assert!(
                actual.abs_diff(level(source)) <= 3,
                "frame {source}: {actual} is not {}",
                level(source)
            );
            shown.push(source);
        }
        assert!(shown.len() >= 60, "{shown:?}");
        assert!(shown[0] <= in_frame + 1, "{shown:?}");
        // Every recorded frame is shown on exactly two consecutive frames
        // (the first run may be cut by the start of playback).
        let mut runs: Vec<(u64, usize)> = Vec::new();
        for source in &shown {
            match runs.last_mut() {
                Some((frame, count)) if frame == source => *count += 1,
                _ => runs.push((*source, 1)),
            }
        }
        for pair in runs.windows(2) {
            assert_eq!(pair[1].0, pair[0].0 + 1, "{runs:?}");
        }
        for (_, count) in &runs[1..runs.len() - 1] {
            assert_eq!(*count, 2, "{runs:?}");
        }
    }

    #[test]
    fn a_previous_run_session_is_restored_and_its_manifest_marked() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("replay");
        fs::create_dir_all(&root).unwrap();
        let rate = FrameRate::new(25, 1).unwrap();
        let format = ReplayFormat {
            width: WIDTH,
            height: HEIGHT,
            rate,
            clock_domain: clock_domain(),
        };
        let session = root.join(format!("{SESSION_PREFIX}1-00"));
        fs::create_dir(&session).unwrap();
        write_session_manifest(&session, format, [input(1)].into_iter()).unwrap();
        let source = session.join("1-0");
        fs::create_dir(&source).unwrap();
        let segment = source.join("segment-000001.mp4");
        fs::write(&segment, b"segment").unwrap();
        append_segment_manifest(&session, input(1), 1, &segment, 100, 0, 50, 7);

        let manifest = session.join(MANIFEST_FILE);
        let text = fs::read_to_string(&manifest).unwrap();
        assert!(manifest_header_matches(&text, format));
        assert!(!manifest_header_matches(
            &text,
            ReplayFormat {
                width: 100,
                ..format
            }
        ));
        let segments = parse_manifest_segments(&text, &session);
        assert_eq!(segments.len(), 1);
        assert_eq!(
            (
                segments[0].input,
                segments[0].first_frame,
                segments[0].first_stream_frame,
                segments[0].frames,
            ),
            (input(1), 100, 0, 50)
        );

        let show = ShowState::new(
            "replay",
            (1..=2)
                .map(|value| (input(value), format!("Camera {value}")))
                .collect(),
            input(1),
            input(2),
        )
        .unwrap();
        let mut engine = Engine::new(
            show,
            rate,
            fm_clock::ClockDomainId::new(NonZeroU128::new(100).unwrap()),
        );
        engine.set_replay_feed(EngineReplayFeed::External);
        accept(
            &mut engine,
            "configure",
            EngineCommand::Replay(EngineReplayOperation::Configure(Box::new(ReplaySettings {
                sources: vec![input(1)],
                retention_seconds: 10,
                storage_root: Some(root.to_string_lossy().into_owned()),
                auto_return: None,
                transition: fm_types::replay::ReplayTransition::CUT,
            }))),
        );
        // The configure command is realized on the next frame; the restore
        // runs once the runtime records the source.
        let _ = engine.tick().unwrap();
        let mut replay = NativeReplay::new(format);
        replay.sync(&mut engine);
        assert_eq!(engine.replay_status().sources[0].segments, 1);
        // The old manifest is marked so a later restart does not restore the
        // same segment twice, while the file stays playable.
        assert!(!manifest.exists());
        assert!(session.join(RESTORED_MANIFEST_FILE).exists());
        assert!(segment.exists());
    }

    #[test]
    fn a_replay_root_below_the_floor_pauses_recording() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("replay");
        fs::create_dir_all(&root).unwrap();
        let rate = FrameRate::new(25, 1).unwrap();
        let format = ReplayFormat {
            width: WIDTH,
            height: HEIGHT,
            rate,
            clock_domain: clock_domain(),
        };
        let show = ShowState::new(
            "replay",
            (1..=2)
                .map(|value| (input(value), format!("Camera {value}")))
                .collect(),
            input(1),
            input(2),
        )
        .unwrap();
        let mut engine = Engine::new(
            show,
            rate,
            fm_clock::ClockDomainId::new(NonZeroU128::new(100).unwrap()),
        );
        engine.set_replay_feed(EngineReplayFeed::External);
        accept(
            &mut engine,
            "configure",
            EngineCommand::Replay(EngineReplayOperation::Configure(Box::new(ReplaySettings {
                sources: vec![input(1)],
                retention_seconds: 10,
                storage_root: Some(root.to_string_lossy().into_owned()),
                auto_return: None,
                transition: fm_types::replay::ReplayTransition::CUT,
            }))),
        );
        let _ = engine.tick().unwrap();
        let mut replay = NativeReplay::new(format);
        // A root whose free space is below this floor is low on space.
        replay.min_free_bytes = u64::MAX;
        replay.sync(&mut engine);
        assert!(replay.low_space);
        assert!(matches!(
            replay.sources[&input(1)].state,
            SourceState::Idle
        ));
    }

}
