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
//! On success the recorder drains and finalizes the part file, which is
//! renamed to the destination and reported through
//! [`ReelHost::reel_export_finished`]. On an encoding or I/O failure the
//! worker cancels the recorder, removes the part file, and the failure is
//! reported through [`ReelHost::reel_export_failed`]. A cancelled export
//! (the engine already reported `Cancelled`) stops the worker, drops its
//! chunk queue, and removes the part file; nothing is reported back.

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
use fm_control::ControlService;
use fm_engine::{
    EngineReelChunk, EngineReelError, EngineReelExportState, EngineReelExportStatus,
    EngineReelLayer,
};
use fm_frame::SequenceNumber;
use fm_types::{ChannelLayout, SampleRate};

use crate::native_replay::{AUDIO_RATE, ReplayFormat, SegmentDecoder, enqueue, retimed, silence};

/// Most chunks one export worker may hold beyond the chunk it encodes.
const MAX_QUEUED_CHUNKS: usize = 2;
/// Most output frames one chunk handed to an export worker holds.
const MAX_CHUNK_FRAMES: u64 = 60;

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
                | EngineReelExportState::Cancelled => {
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
            .find(|export| {
                matches!(
                    export.state,
                    EngineReelExportState::Queued | EngineReelExportState::Running
                )
            })
            .cloned();
        if let Some(status) = status {
            self.start(host, &status);
        }
    }

    /// Starts the worker of `status`, creating its part file exclusively.
    fn start(&mut self, host: &mut dyn ReelHost, status: &EngineReelExportStatus) {
        let destination = PathBuf::from(status.destination.as_str());
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

/// `destination`'s part file: the same name with `.part` appended.
fn part_path(destination: &Path) -> PathBuf {
    let mut part = destination.as_os_str().to_os_string();
    part.push(".part");
    PathBuf::from(part)
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
    } = worker;
    let fail = |reason: &str| {
        remove_part(&part);
        let _ = events.send(WorkerEvent::Failed(reason.to_owned()));
    };
    let cancel = |recorder: &mut Recorder| {
        let _ = recorder.cancel();
        remove_part(&part);
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
        let chunk = match queue.recv() {
            Ok(chunk) => chunk,
            Err(_) => {
                cancel(&mut recorder);
                return;
            }
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
            let Some(audio) = silence(format.rate, format.clock_domain, frame.output_frame) else {
                let _ = recorder.cancel();
                return fail("frame:audio");
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
        let Ok(metadata) = fs::metadata(&part) else {
            return fail("output:metadata");
        };
        let bytes = metadata.len();
        if destination.exists() {
            return fail("output:exists");
        }
        if fs::rename(&part, &destination).is_err() {
            return fail("output:rename");
        }
        let _ = events.send(WorkerEvent::Finished { bytes });
        return;
    }
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
