//! Native highlight reel export (OR-013): the encoder behind `reel_export`.
//!
//! An accepted export is planned by the engine: it snapshots the compiled
//! reel, protects every recorded segment the reel reads from retention, and
//! answers bounded chunks of exact output frames ([`EngineReelChunk`]). Each
//! frame names, per layer, the segment file and the frame index inside it
//! ([`EngineReelLayer`]); a cut has exactly one layer.
//!
//! One worker thread per active export owns one `FFmpeg` [`Recorder`]
//! encoding the frames the engine resolves (H.264 yuv420p through `libx264`
//! at the show's frame rate and picture size, with silent stereo AAC at
//! 48 kHz) into `<destination>.part`. The frame loop only moves chunks from
//! the engine to the worker over a bounded synchronous channel; decoding,
//! retiming, composition, and encoding happen off the loop. The engine's
//! `completed_frames` counts the frames handed over, so the loop never runs
//! ahead of the worker by more than the channel bound.
//!
//! The worker decodes each layer's segment frame with the replay
//! [`SegmentDecoder`], retimes the picture onto the output frame grid, and
//! blends the (at most two) layers with their exact rational weights before
//! handing the encoder one RGBA picture and one silent audio block per
//! output frame.
//!
//! A reel with a music bed ([`ReelMusic`]) also streams its audio file
//! through one sequential decoder from the first output frame: each frame
//! takes the file's samples of exactly that frame's span, fitted to the
//! frame's 48 kHz stereo block, scaled by the bed's level, and added under
//! the clips' audio. While a real-time clip's recorded audio plays the bed
//! ramps to its ducked level over [`MUSIC_DUCK_SECONDS`] and back up after,
//! and it fades out over the reel's last [`MUSIC_FADE_SECONDS`] (or the
//! whole reel when shorter). Once the file ends the bed is silent. A bed
//! that cannot be opened fails the export `music:open`, and one that stops
//! decoding `music:decode`.
//!
//! On success the recorder drains and finalizes the part file (a
//! fragmented MP4, since it writes through a pipe), which a stream copy
//! remuxes into `<destination>.faststart.part`, a regular MP4 with its
//! index ahead of the media; the part file is removed, and the remux is
//! renamed to the destination and reported through
//! [`ReelHost::reel_export_finished`]. On an encoding, remux
//! (`output:faststart`), or I/O failure the worker cancels the recorder,
//! removes both part files, and the failure is reported through
//! [`ReelHost::reel_export_failed`]. A cancelled export
//! (the engine already reported `Cancelled`) stops the worker, drops its
//! chunk queue, and removes the part file; nothing is reported back.
//!
//! An export starts only when the filesystem of its destination has room
//! for its budget ([`export_budget`], a generous bound on the encoder's
//! default rate) above the recording floor
//! ([`crate::disk::RECORDING_MINIMUM_FREE_BYTES`]); otherwise it fails
//! `output:space` (or `output:space_unavailable` when the free space cannot
//! be read) before its part file exists. The worker reads the free space
//! again after each chunk and fails `output:space` once it drops below the
//! floor, so an export never fills the disk a live recording shares.

use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::Duration,
};

use fm_auth::Policy;
use fm_codec_ffmpeg::record::{
    CleanupStatus, OutputFinalization, PairedFrame, RecordConfig, RecordFormat, RecordLimits,
    Recorder, RecorderState, StopOutcome, X264Preset,
};
use fm_codec_ffmpeg::{Adapter, Config, LocalAudioDecoder, StreamSelector};
use fm_control::ControlService;
use fm_engine::{
    EngineReelChunk, EngineReelError, EngineReelExportState, EngineReelExportStatus,
    EngineReelLayer,
};
use fm_frame::{AudioBlock, SequenceNumber};
use fm_protocol::ReelMusic;
use fm_types::{ChannelLayout, SampleRate};

use crate::native_replay::{
    AUDIO_RATE, ReplayFormat, SegmentDecoder, enqueue, recorded_audio, retimed, silence,
};

/// Seconds a music bed takes to duck under clip audio, and to come back.
const MUSIC_DUCK_SECONDS: f32 = 0.25;
/// Seconds a music bed fades out over at the end of the reel.
const MUSIC_FADE_SECONDS: u32 = 1;
/// Audio blocks a music bed decodes at a time.
const MUSIC_DECODE_BLOCKS: u32 = 32;

/// Most chunks one export worker may hold beyond the chunk it encodes.
const MAX_QUEUED_CHUNKS: usize = 2;
/// Most output frames one chunk handed to an export worker holds.
const MAX_CHUNK_FRAMES: u64 = 60;
/// Bytes budgeted per output frame beyond its picture: the AAC audio and
/// the MP4 fragment overhead, generously.
const FRAME_OVERHEAD_BYTES: u64 = 4_096;

/// What the export worker needs from the engine.
pub(crate) trait ReelHost {
    /// A counter that moves whenever the export status may have changed.
    fn reel_generation(&self) -> u64;
    /// Every export job, active or terminal.
    fn reel_exports(&self) -> Vec<EngineReelExportStatus>;
    /// Resolves up to `max_frames` further output frames of export `job`.
    fn reel_export_chunk(
        &mut self,
        job: u64,
        max_frames: u64,
    ) -> Result<EngineReelChunk, EngineReelError>;
    /// Reports that export `job`'s file was written, `bytes` long.
    fn reel_export_finished(&mut self, job: u64, bytes: u64) -> Result<(), EngineReelError>;
    /// Reports that export `job`'s file could not be written.
    fn reel_export_failed(&mut self, job: u64, reason: String) -> Result<(), EngineReelError>;
}

impl ReelHost for ControlService<Policy> {
    fn reel_generation(&self) -> u64 {
        ControlService::reel_generation(self)
    }

    fn reel_exports(&self) -> Vec<EngineReelExportStatus> {
        ControlService::reel_exports(self)
    }

    fn reel_export_chunk(
        &mut self,
        job: u64,
        max_frames: u64,
    ) -> Result<EngineReelChunk, EngineReelError> {
        ControlService::reel_export_chunk(self, job, max_frames)
    }

    fn reel_export_finished(&mut self, job: u64, bytes: u64) -> Result<(), EngineReelError> {
        ControlService::reel_export_finished(self, job, bytes)
    }

    fn reel_export_failed(&mut self, job: u64, reason: String) -> Result<(), EngineReelError> {
        ControlService::reel_export_failed(self, job, reason)
    }
}

/// What an export worker reports.
enum WorkerEvent {
    /// The recorder runs and takes frames.
    Started,
    Finished {
        bytes: u64,
    },
    Failed(String),
    /// The chunk queue closed; the worker cancelled and removed its part
    /// file.
    Cancelled,
}

enum WorkerOutcome {
    Finished { bytes: u64 },
    Failed(String),
    Cancelled,
}

/// The active export's channel to its worker.
struct Active {
    job: u64,
    chunks: SyncSender<EngineReelChunk>,
    events: Receiver<WorkerEvent>,
    /// Set before the chunk queue is dropped so a worker that is already
    /// finalizing never renames a cancelled part file.
    cancelled: Arc<AtomicBool>,
    started: bool,
    /// The last chunk pulled from the engine finished the reel.
    resolved: bool,
    /// A chunk resolved from the engine but not yet accepted by the worker.
    pending: Option<EngineReelChunk>,
}

impl Active {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// The highlight reel exports of one native daemon.
pub(crate) struct NativeReel {
    format: ReplayFormat,
    active: Option<Active>,
    /// The engine generation `exports` was read at.
    generation: Option<u64>,
    exports: Vec<EngineReelExportStatus>,
}

impl NativeReel {
    pub(crate) fn new(format: ReplayFormat) -> Self {
        Self {
            format,
            active: None,
            generation: None,
            exports: Vec::new(),
        }
    }

    /// Follows the engine's exports: starts a worker for a queued or running
    /// export, moves bounded chunks to it, collects what it reports, and
    /// stops a worker whose export was cancelled or ended elsewhere.
    pub(crate) fn sync(&mut self, host: &mut dyn ReelHost) {
        let generation = host.reel_generation();
        if self.generation != Some(generation) {
            self.exports = host.reel_exports();
            self.generation = Some(generation);
        }
        if let Some(mut active) = self.active.take() {
            match poll(&mut active) {
                Some(WorkerOutcome::Finished { bytes }) => {
                    report_finished(host, active.job, bytes);
                    return;
                }
                Some(WorkerOutcome::Failed(reason)) => {
                    report_failed(host, active.job, &reason);
                    return;
                }
                Some(WorkerOutcome::Cancelled) => return,
                None => {}
            }
            let status = self
                .exports
                .iter()
                .find(|export| export.job == active.job)
                .cloned();
            let Some(status) = status else {
                // Forgotten elsewhere: drop the queue; the worker cancels its
                // recorder and removes the part file.
                active.cancel();
                return;
            };
            match status.state {
                EngineReelExportState::Queued
                | EngineReelExportState::Running
                | EngineReelExportState::Finalizing => {
                    if let Err(reason) = pump(host, &mut active, &status) {
                        active.cancel();
                        report_failed(host, active.job, &reason);
                        return;
                    }
                    self.active = Some(active);
                }
                EngineReelExportState::Completed
                | EngineReelExportState::Failed
                | EngineReelExportState::Cancelled
                | EngineReelExportState::Deferred => {
                    // Cancelled, failed, or finished elsewhere: drop the
                    // queue; the worker cancels its recorder and removes the
                    // part file.
                    active.cancel();
                }
            }
            return;
        }
        let status = self
            .exports
            .iter()
            .find(|export| worker_state(export.state))
            .cloned();
        if let Some(status) = status {
            self.start(host, &status);
        }
    }

    /// Starts the worker of `status`, creating its part file exclusively,
    /// once its destination has room for it.
    fn start(&mut self, host: &mut dyn ReelHost, status: &EngineReelExportStatus) {
        let destination = PathBuf::from(status.destination.as_str());
        let available = destination
            .parent()
            .and_then(crate::disk::existing_ancestor)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
            .and_then(crate::disk::available_bytes);
        if let Err(reason) = room_for(available, export_budget(self.format, status.total_frames)) {
            report_failed(host, status.job, reason);
            return;
        }
        let part = part_path(&destination);
        let Ok(file) = OpenOptions::new().write(true).create_new(true).open(&part) else {
            report_failed(host, status.job, "output:create");
            return;
        };
        let (chunks, queue) = mpsc::sync_channel(MAX_QUEUED_CHUNKS);
        let (sender, events) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let format = self.format;
        let worker_part = part.clone();
        let worker_cancelled = Arc::clone(&cancelled);
        let job = status.job;
        let music = status.music.clone();
        let total_frames = status.total_frames;
        let spawned = thread::Builder::new()
            .name(format!("freemixd-reel-export-{job}"))
            .spawn(move || {
                export_run(Worker {
                    format,
                    destination,
                    part: worker_part,
                    file,
                    queue,
                    events: sender,
                    cancelled: worker_cancelled,
                    music,
                    total_frames,
                });
            });
        if spawned.is_err() {
            remove_part(&part);
            report_failed(host, job, "start:thread");
            return;
        }
        self.active = Some(Active {
            job,
            chunks,
            events,
            cancelled,
            started: false,
            resolved: false,
            pending: None,
        });
    }
}

impl Drop for NativeReel {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            active.cancel();
        }
    }
}

/// Whether an export in `state` has finalized media and needs a worker. A
/// deferred export (Protocol 2.123) waits for the recorder, so the daemon
/// must not create its part file or resolve frames yet.
const fn worker_state(state: EngineReelExportState) -> bool {
    matches!(
        state,
        EngineReelExportState::Queued | EngineReelExportState::Running
    )
}

/// The bytes an export of `frames` output frames at `format` is budgeted:
/// an eighth of a byte per pixel per frame (about 6 Mbit/s at 1280x720 and
/// 25 fps, well above the default `libx264` rate of a replay picture) plus
/// [`FRAME_OVERHEAD_BYTES`], twice over, since the encoded part file and
/// its faststart copy coexist until the remux ends; saturating.
fn export_budget(format: ReplayFormat, frames: u64) -> u64 {
    let picture = u64::from(format.width) * u64::from(format.height) / 8;
    frames
        .saturating_mul(picture.saturating_add(FRAME_OVERHEAD_BYTES))
        .saturating_mul(2)
}

/// Whether `available` free bytes hold `budget` above the recording floor:
/// the failure reason when they do not, or cannot be read.
fn room_for(available: std::io::Result<u64>, budget: u64) -> Result<(), &'static str> {
    let available = available.map_err(|_| "output:space_unavailable")?;
    if available < budget.saturating_add(crate::disk::RECORDING_MINIMUM_FREE_BYTES) {
        return Err("output:space");
    }
    Ok(())
}

/// `destination`'s part file: the same name with `.part` appended.
fn part_path(destination: &Path) -> PathBuf {
    let mut part = destination.as_os_str().to_os_string();
    part.push(".part");
    PathBuf::from(part)
}

/// `destination`'s faststart remux: the same name with `.faststart.part`
/// appended.
fn remux_path(destination: &Path) -> PathBuf {
    let mut remuxed = destination.as_os_str().to_os_string();
    remuxed.push(".faststart.part");
    PathBuf::from(remuxed)
}

/// How long the faststart remux of `frames` output frames may run: a copy
/// remux moves thousands of frames a second, so half a minute plus a
/// second per hundred frames is ample.
fn remux_timeout(frames: u64) -> Duration {
    Duration::from_secs(30_u64.saturating_add(frames / 100))
}

/// Removes a part file, retrying briefly while a killed recorder's output
/// worker still holds it open (Windows refuses the removal until it closes).
fn remove_part(part: &Path) {
    for _ in 0..20 {
        if fs::remove_file(part).is_ok() || !part.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn poll(active: &mut Active) -> Option<WorkerOutcome> {
    loop {
        match active.events.try_recv() {
            Ok(WorkerEvent::Started) => active.started = true,
            Ok(WorkerEvent::Finished { bytes }) => {
                return Some(WorkerOutcome::Finished { bytes });
            }
            Ok(WorkerEvent::Failed(reason)) => return Some(WorkerOutcome::Failed(reason)),
            Ok(WorkerEvent::Cancelled) => return Some(WorkerOutcome::Cancelled),
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                return Some(WorkerOutcome::Failed("worker_lost".to_owned()));
            }
        }
    }
}

/// Moves at most one more chunk to `active`'s worker, never resolving more
/// frames than the engine says remain.
fn pump(
    host: &mut dyn ReelHost,
    active: &mut Active,
    status: &EngineReelExportStatus,
) -> Result<(), String> {
    if !active.started {
        return Ok(());
    }
    // Hand over a chunk the bounded queue refused earlier before resolving
    // another one: the final chunk must reach the worker even after the
    // engine reported every frame resolved.
    if let Some(chunk) = active.pending.take() {
        return hand(active, chunk);
    }
    if active.resolved {
        return Ok(());
    }
    let remaining = status.total_frames.saturating_sub(status.completed_frames);
    if remaining == 0 {
        return Ok(());
    }
    let chunk = host
        .reel_export_chunk(active.job, MAX_CHUNK_FRAMES.min(remaining))
        .map_err(|error| format!("resolve:{}", error_code(&error)))?;
    let resolved = u64::try_from(chunk.frames_out.len()).unwrap_or(u64::MAX);
    if resolved != chunk.frames.end.saturating_sub(chunk.frames.start) {
        return Err("resolve:frames".to_owned());
    }
    active.resolved = chunk.completed;
    hand(active, chunk)
}

/// Offers `chunk` to the worker, holding it in `pending` while the bounded
/// queue is full.
fn hand(active: &mut Active, chunk: EngineReelChunk) -> Result<(), String> {
    match active.chunks.try_send(chunk) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(chunk)) => {
            active.pending = Some(chunk);
            Ok(())
        }
        Err(TrySendError::Disconnected(_)) => Err("worker_lost".to_owned()),
    }
}

fn report_finished(host: &mut dyn ReelHost, job: u64, bytes: u64) {
    match host.reel_export_finished(job, bytes) {
        Ok(()) | Err(EngineReelError::ExportNotActive(_) | EngineReelError::UnknownExport(_)) => {}
        Err(error) => {
            eprintln!(
                "FREEMIXD_REEL_FAILURE\tv=1\tjob={job}\tfailure={}",
                crate::diagnostic_field(&format!("finished:{}", error_code(&error)))
            );
        }
    }
}

fn report_failed(host: &mut dyn ReelHost, job: u64, reason: &str) {
    match host.reel_export_failed(job, reason.to_owned()) {
        Ok(()) => eprintln!(
            "FREEMIXD_REEL_FAILURE\tv=1\tjob={job}\tfailure={}",
            crate::diagnostic_field(reason)
        ),
        // The job ended elsewhere (a cancel won the race): its terminal state
        // is the truth, and reporting a failure again would be refused.
        Err(EngineReelError::ExportNotActive(_) | EngineReelError::UnknownExport(_)) => {}
        Err(error) => eprintln!(
            "FREEMIXD_REEL_FAILURE\tv=1\tjob={job}\tfailure={}",
            crate::diagnostic_field(&format!("failed:{}", error_code(&error)))
        ),
    }
}

/// The path-free code of an engine refusal.
fn error_code(error: &EngineReelError) -> &'static str {
    match error {
        EngineReelError::UnknownReel(_) => "unknown_reel",
        EngineReelError::UnknownExport(_) => "unknown_export",
        EngineReelError::ExportNotActive(_) => "not_active",
        EngineReelError::ExportNotFinalizing(_) => "not_finalizing",
        EngineReelError::ExportStillActive(_) => "still_active",
        EngineReelError::UnknownEvent(_) => "unknown_event",
        EngineReelError::InvalidSpeed(_) => "invalid_speed",
        EngineReelError::Unresolved { .. } => "unresolved",
        EngineReelError::Plan(_) => "plan",
        EngineReelError::Export(_) => "export",
    }
}

/// One export worker's inputs.
struct Worker {
    format: ReplayFormat,
    destination: PathBuf,
    part: PathBuf,
    file: File,
    queue: Receiver<EngineReelChunk>,
    events: Sender<WorkerEvent>,
    cancelled: Arc<AtomicBool>,
    music: Option<ReelMusic>,
    total_frames: u64,
}

/// One export's worker: starts the recorder, then encodes every frame of
/// every chunk handed over until the last chunk finalizes the file, the
/// queue closes (cancel), or encoding fails.
#[allow(clippy::too_many_lines)]
fn export_run(worker: Worker) {
    let Worker {
        format,
        destination,
        part,
        file,
        queue,
        events,
        cancelled,
        music,
        total_frames,
    } = worker;
    let remuxed = remux_path(&destination);
    let fail = |reason: &str| {
        remove_part(&part);
        remove_part(&remuxed);
        let _ = events.send(WorkerEvent::Failed(reason.to_owned()));
    };
    let cancel = |recorder: &mut Recorder| {
        let _ = recorder.cancel();
        remove_part(&part);
        remove_part(&remuxed);
        let _ = events.send(WorkerEvent::Cancelled);
    };
    let Some(sample_rate) = SampleRate::new(AUDIO_RATE) else {
        drop(file);
        return fail("format:sample_rate");
    };
    let Ok(record_format) = RecordFormat::new(
        format.width,
        format.height,
        format.rate,
        sample_rate,
        ChannelLayout::stereo(),
        SequenceNumber::new(0),
    ) else {
        drop(file);
        return fail("format:invalid");
    };
    let mut config = RecordConfig::new(record_format.clone());
    config.preset = Some(X264Preset::Veryfast);
    config.square_pixels = true;
    config.limits = RecordLimits {
        max_outstanding_pairs: 8,
        ..RecordLimits::default()
    };
    let mut bed = None;
    if let Some(music) = music {
        let Some(opened) = MusicBed::open(&music, format, total_frames) else {
            drop(file);
            return fail("music:open");
        };
        bed = Some(opened);
    }
    let Ok(mut recorder) = Recorder::start(file, config) else {
        return fail("encode:start");
    };
    if events.send(WorkerEvent::Started).is_err() {
        cancel(&mut recorder);
        return;
    }
    let mut decoders = Vec::new();
    let mut sequence = 0_u64;
    loop {
        if cancelled.load(Ordering::Acquire) {
            cancel(&mut recorder);
            return;
        }
        let Ok(chunk) = queue.recv() else {
            cancel(&mut recorder);
            return;
        };
        for frame in &chunk.frames_out {
            if cancelled.load(Ordering::Acquire) {
                cancel(&mut recorder);
                return;
            }
            if frame.output_frame != sequence {
                let _ = recorder.cancel();
                return fail("frame:sequence");
            }
            let Some(rgba) = compose(format, frame.output_frame, &frame.layers, &mut decoders)
            else {
                let _ = recorder.cancel();
                return fail("frame:decode");
            };
            let Some((audio, clip_audio)) =
                mix_audio(format, frame.output_frame, &frame.layers, &mut decoders)
            else {
                let _ = recorder.cancel();
                return fail("frame:audio");
            };
            let audio = match &mut bed {
                Some(bed) => match bed.mix_under(audio, frame.output_frame, clip_audio) {
                    Ok(Some(audio)) => audio,
                    Ok(None) => {
                        let _ = recorder.cancel();
                        return fail("frame:audio");
                    }
                    Err(reason) => {
                        let _ = recorder.cancel();
                        return fail(reason);
                    }
                },
                None => audio,
            };
            let Ok(pair) = PairedFrame::new(
                &record_format,
                SequenceNumber::new(frame.output_frame),
                rgba,
                audio,
            ) else {
                let _ = recorder.cancel();
                return fail("frame:pair");
            };
            if let Err(_rejected) = enqueue(&mut recorder, pair) {
                let _ = recorder.cancel();
                return fail("encode:enqueue");
            }
            sequence += 1;
        }
        if let Err(reason) = room_for(crate::disk::available_bytes(&part), 0) {
            let _ = recorder.cancel();
            return fail(reason);
        }
        if !chunk.completed {
            continue;
        }
        if cancelled.load(Ordering::Acquire) {
            cancel(&mut recorder);
            return;
        }
        let report = recorder.stop();
        let clean = report.telemetry.state == RecorderState::Stopped
            && report.outcome == StopOutcome::Clean
            && report.output == OutputFinalization::Synced
            && report.cleanup == CleanupStatus::Complete
            && report.telemetry.accepted_pairs == report.telemetry.completed_pairs
            && report.telemetry.failed_pairs == 0;
        if !clean {
            return fail("encode:stop");
        }
        if cancelled.load(Ordering::Acquire) {
            cancel(&mut recorder);
            return;
        }
        // The recorder writes a fragmented MP4 through a pipe; a copy remux
        // puts the index ahead of the media so any player or editor opens
        // the reel at once.
        let remux = Adapter::new(Config::default())
            .and_then(|adapter| adapter.remux_faststart(&part, &remuxed, remux_timeout(sequence)));
        if remux.is_err() {
            return fail("output:faststart");
        }
        remove_part(&part);
        if cancelled.load(Ordering::Acquire) {
            cancel(&mut recorder);
            return;
        }
        let Ok(metadata) = fs::metadata(&remuxed) else {
            return fail("output:metadata");
        };
        let bytes = metadata.len();
        if destination.exists() {
            return fail("output:exists");
        }
        if fs::rename(&remuxed, &destination).is_err() {
            return fail("output:rename");
        }
        let _ = events.send(WorkerEvent::Finished { bytes });
        return;
    }
}

/// `output_frame`'s audio: each real-time layer's recorded audio of its
/// frame, weighted like its picture (so a dissolve crossfades), summed;
/// slow-motion layers and layers without audio add nothing, and a frame
/// without any is silent. The flag says whether any clip audio played.
fn mix_audio(
    format: ReplayFormat,
    output_frame: u64,
    layers: &[EngineReelLayer],
    decoders: &mut Vec<SegmentDecoder>,
) -> Option<(AudioBlock, bool)> {
    let mut mixed: Option<Vec<Vec<f32>>> = None;
    for (index, layer) in layers.iter().enumerate() {
        if !layer.full_speed || layer.weight_num == 0 || layer.weight_den == 0 {
            continue;
        }
        while decoders.len() <= index {
            decoders.push(SegmentDecoder::new(format));
        }
        let Some(planes) = decoders[index].audio(&layer.path, layer.frame) else {
            continue;
        };
        #[allow(clippy::cast_precision_loss)] // weights are small integers
        let weight = layer.weight_num as f32 / layer.weight_den as f32;
        match &mut mixed {
            None => {
                mixed = Some(
                    planes
                        .into_iter()
                        .map(|plane| plane.into_iter().map(|sample| sample * weight).collect())
                        .collect(),
                );
            }
            Some(sum) => {
                for (target, plane) in sum.iter_mut().zip(planes) {
                    for (value, sample) in target.iter_mut().zip(plane) {
                        *value += sample * weight;
                    }
                }
            }
        }
    }
    match mixed {
        Some(planes) => recorded_audio(format.rate, format.clock_domain, output_frame, &planes)
            .map(|block| (block, true)),
        None => silence(format.rate, format.clock_domain, output_frame).map(|block| (block, false)),
    }
}

/// One export's music bed: a sequential decoder over its audio file and
/// the source samples decoded but not yet played.
struct MusicBed {
    format: ReplayFormat,
    decoder: LocalAudioDecoder,
    /// The file's sample rate, once its first block is decoded.
    rate: Option<u32>,
    /// Decoded source samples not yet played, per channel.
    pending: Vec<Vec<f32>>,
    /// The file has no more samples.
    ended: bool,
    level: f32,
    ducked: f32,
    /// The current duck gain, ramping between `ducked` and 1.
    gain: f32,
    /// The reel's last sample at 48 kHz, where the fade ends.
    end_sample: u64,
}

impl MusicBed {
    fn open(music: &ReelMusic, format: ReplayFormat, total_frames: u64) -> Option<Self> {
        let adapter = Adapter::new(Config::default()).ok()?;
        let decoder = adapter
            .open_local_audio(&music.path, format.clock_domain, StreamSelector::Best)
            .ok()?;
        Some(Self {
            format,
            decoder,
            rate: None,
            pending: Vec::new(),
            ended: false,
            level: f32::from(music.level_percent) / 100.0,
            ducked: f32::from(music.duck_percent) / 100.0,
            gain: 1.0,
            end_sample: sample_at(format, AUDIO_RATE, total_frames),
        })
    }

    /// Decodes one more window into `pending`.
    fn decode_more(&mut self) -> Result<(), &'static str> {
        let count = std::num::NonZeroU32::new(MUSIC_DECODE_BLOCKS).ok_or("music:decode")?;
        let window = self
            .decoder
            .decode_up_to(count)
            .map_err(|_| "music:decode")?;
        for block in &window.blocks {
            let rate = block.sample_rate().hertz();
            if self.rate.is_some_and(|known| known != rate) {
                return Err("music:decode");
            }
            self.rate = Some(rate);
            if self.pending.is_empty() {
                self.pending = vec![Vec::new(); block.planes().len()];
            }
            for (plane, samples) in self.pending.iter_mut().zip(block.planes()) {
                plane.extend_from_slice(samples);
            }
        }
        if window.end_of_stream || window.blocks.is_empty() {
            self.ended = true;
        }
        Ok(())
    }

    /// The file's samples of `output_frame`'s span, or `None` once the file
    /// has ended; the file's last samples are padded with silence.
    fn take(&mut self, output_frame: u64) -> Result<Option<Vec<Vec<f32>>>, &'static str> {
        if self.rate.is_none() && !self.ended {
            self.decode_more()?;
        }
        let Some(rate) = self.rate else {
            return Ok(None);
        };
        let start = sample_at(self.format, rate, output_frame);
        let end = sample_at(self.format, rate, output_frame + 1);
        let needed = usize::try_from(end - start).map_err(|_| "music:decode")?;
        while self.pending.first().map_or(0, Vec::len) < needed && !self.ended {
            self.decode_more()?;
        }
        let available = self.pending.first().map_or(0, Vec::len);
        if available == 0 {
            return Ok(None);
        }
        let taken = needed.min(available);
        Ok(Some(
            self.pending
                .iter_mut()
                .map(|plane| {
                    let mut samples: Vec<f32> = plane.drain(..taken).collect();
                    samples.resize(needed, 0.0);
                    samples
                })
                .collect(),
        ))
    }

    /// `clips` with the bed's audio of `output_frame` added under it,
    /// ducked while `clip_audio` plays and faded at the reel's end; `None`
    /// when the frame's block cannot be built.
    fn mix_under(
        &mut self,
        clips: AudioBlock,
        output_frame: u64,
        clip_audio: bool,
    ) -> Result<Option<AudioBlock>, &'static str> {
        let Some(source) = self.take(output_frame)? else {
            return Ok(Some(clips));
        };
        let Some(bed) = recorded_audio(
            self.format.rate,
            self.format.clock_domain,
            output_frame,
            &source,
        ) else {
            return Ok(None);
        };
        let target = if clip_audio { self.ducked } else { 1.0 };
        #[allow(clippy::cast_precision_loss)] // the sample rate is small
        let step = 1.0 / (AUDIO_RATE as f32 * MUSIC_DUCK_SECONDS);
        let first = sample_at(self.format, AUDIO_RATE, output_frame);
        let fade = u64::from(AUDIO_RATE * MUSIC_FADE_SECONDS).min(self.end_sample);
        let samples = clips.sample_count();
        let mut gains = Vec::with_capacity(samples);
        for index in 0..samples {
            self.gain = if self.gain < target {
                (self.gain + step).min(target)
            } else {
                (self.gain - step).max(target)
            };
            let remaining = self
                .end_sample
                .saturating_sub(first + u64::try_from(index).unwrap_or(u64::MAX));
            #[allow(clippy::cast_precision_loss)] // sample counts of a second
            let faded = if fade == 0 || remaining >= fade {
                1.0
            } else {
                remaining as f32 / fade as f32
            };
            gains.push(self.level * self.gain * faded);
        }
        let planes = clips
            .planes()
            .iter()
            .zip(bed.planes())
            .map(|(clip, music)| {
                clip.iter()
                    .zip(music)
                    .zip(&gains)
                    .map(|((clip, music), gain)| clip + music * gain)
                    .collect()
            })
            .collect();
        Ok(AudioBlock::new(
            clips.timing(),
            clips.sample_rate(),
            clips.channel_layout().clone(),
            planes,
        )
        .ok())
    }
}

/// The first sample of `frame` at `hertz` on the reel's frame grid.
fn sample_at(format: ReplayFormat, hertz: u32, frame: u64) -> u64 {
    let rate = format.rate;
    u64::try_from(
        u128::from(frame) * u128::from(hertz) * u128::from(rate.denominator())
            / u128::from(rate.numerator()),
    )
    .unwrap_or(u64::MAX)
}

/// Composes `output_frame`'s layers into one RGBA picture at the show size:
/// each layer's segment frame is decoded, retimed onto the output frame
/// grid, and blended with its exact rational weight.
fn compose(
    format: ReplayFormat,
    output_frame: u64,
    layers: &[EngineReelLayer],
    decoders: &mut Vec<SegmentDecoder>,
) -> Option<Vec<u8>> {
    let mut composed: Option<(Vec<u8>, u64)> = None;
    let mut denominator = 0_u32;
    for (index, layer) in layers.iter().enumerate() {
        while decoders.len() <= index {
            decoders.push(SegmentDecoder::new(format));
        }
        let picture = decoders[index].decode(&layer.path, layer.frame)?;
        let picture = retimed(picture.as_ref(), output_frame, format)?;
        let image =
            crate::scene_preview::sampled_live_picture(&picture, format.width, format.height)?;
        let pixels = image.pixels();
        if layer.weight_num == 0 {
            continue;
        }
        if denominator == 0 {
            denominator = layer.weight_den;
        } else if denominator != layer.weight_den {
            return None;
        }
        let numerator = u64::from(layer.weight_num);
        match &mut composed {
            None => composed = Some((pixels.to_vec(), numerator)),
            Some((base, base_numerator)) => {
                for (target, source) in base.chunks_exact_mut(4).zip(pixels.chunks_exact(4)) {
                    for (channel, value) in target.iter_mut().enumerate() {
                        let weighted = u64::from(*value) * *base_numerator
                            + u64::from(source[channel]) * numerator;
                        *value = u8::try_from(weighted / u64::from(denominator)).ok()?;
                    }
                }
                *base_numerator += numerator;
            }
        }
    }
    let (pixels, numerator) = composed?;
    let denominator = u64::from(denominator);
    if denominator == 0 {
        return None;
    }
    if numerator == denominator {
        return Some(pixels);
    }
    // The planner always weights a frame to the full denominator; a partial
    // sum composites over black.
    let mut scaled = pixels;
    for value in &mut scaled {
        *value = u8::try_from(u64::from(*value) * numerator / denominator).ok()?;
    }
    Some(scaled)
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;
    use std::io;

    use fm_frame::ClockDomainId;
    use fm_types::FrameRate;

    use super::{FRAME_OVERHEAD_BYTES, ReplayFormat, export_budget, room_for, worker_state};
    use crate::disk::RECORDING_MINIMUM_FREE_BYTES;
    use fm_engine::EngineReelExportState;

    fn format(width: u32, height: u32) -> ReplayFormat {
        ReplayFormat {
            width,
            height,
            rate: FrameRate::new(25, 1).unwrap(),
            clock_domain: ClockDomainId::new(NonZeroU128::MIN),
        }
    }

    /// Protocol 2.123: a deferred export waits for its media and gets no
    /// worker or part file until the engine reports it queued.
    #[test]
    fn only_a_finalized_export_starts_a_worker() {
        assert!(!worker_state(EngineReelExportState::Deferred));
        assert!(worker_state(EngineReelExportState::Queued));
        assert!(worker_state(EngineReelExportState::Running));
        assert!(!worker_state(EngineReelExportState::Finalizing));
        assert!(!worker_state(EngineReelExportState::Completed));
    }

    /// OR-013: an export's budget grows with its frames and picture size
    /// and saturates rather than wrapping.
    #[test]
    fn an_export_is_budgeted_by_its_frames_and_picture() {
        let frame = 1_280 * 720 / 8 + FRAME_OVERHEAD_BYTES;
        assert_eq!(export_budget(format(1_280, 720), 0), 0);
        assert_eq!(export_budget(format(1_280, 720), 250), 2 * 250 * frame);
        assert!(export_budget(format(1_920, 1_080), 250) > 2 * 250 * frame);
        assert_eq!(export_budget(format(1_920, 1_080), u64::MAX), u64::MAX);
    }

    /// OR-013: an export needs its budget above the recording floor, and an
    /// unreadable free space refuses it as such.
    #[test]
    fn an_export_needs_room_above_the_recording_floor() {
        let budget = 1_000;
        let floor = RECORDING_MINIMUM_FREE_BYTES;
        assert_eq!(room_for(Ok(floor + budget), budget), Ok(()));
        assert_eq!(
            room_for(Ok(floor + budget - 1), budget),
            Err("output:space")
        );
        assert_eq!(room_for(Ok(floor), 0), Ok(()));
        assert_eq!(room_for(Ok(floor - 1), 0), Err("output:space"));
        // A saturated budget is never met short of every byte.
        assert_eq!(room_for(Ok(u64::MAX - 1), u64::MAX), Err("output:space"));
        assert_eq!(
            room_for(Err(io::Error::from(io::ErrorKind::NotFound)), 0),
            Err("output:space_unavailable")
        );
    }
}
