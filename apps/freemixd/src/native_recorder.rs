//! The native program recorder: one `fm-record` session per configured
//! output, segment rotation, its pair queue, and the recording commands.
//!
//! Extracted from `main.rs` as a mechanical move; the block is compiled only
//! for a native daemon.

#[allow(unused_imports, clippy::wildcard_imports)]
// the binary items, some of them native only
use super::*;

#[cfg(feature = "native-media")]
pub(crate) struct NativeProgramRecorder {
    pub(crate) readback: NativeProgramReadback,
    pub(crate) format: RecordFormat,
    pub(crate) sink: RecorderSink,
    /// Pairs captured while the segment is still opening, or that the open
    /// recorder has not accepted yet, in sequence order. Every newer pair
    /// queues behind them, so the segment stays gap-free from its first
    /// sequence; once the backlog drains the recorder is fed directly again.
    pub(crate) backlog: std::collections::VecDeque<PairedFrame>,
    /// Bytes retained by [`Self::backlog`].
    pub(crate) backlog_bytes: usize,
    /// Upper bound for [`Self::backlog_bytes`]: the recorder's own retained
    /// byte budget, so an opening segment never holds more than a running one
    /// may, and a stalled recorder is failed once it would exceed it.
    pub(crate) backlog_limit: usize,
    /// Pairs that waited in [`Self::backlog`] instead of going straight to
    /// the recorder: the segment was opening or the recorder fell behind.
    pub(crate) backlogged_pairs: u64,
    /// Highest [`Self::backlog_bytes`] this segment reached.
    pub(crate) backlog_peak_bytes: usize,
    pub(crate) capture: RecorderCapturePolicy,
    pub(crate) finalization_clean: Option<bool>,
    pub(crate) startup_pair_timeout: Duration,
    /// Mux-readiness deadline of a runtime `RecordStart` segment that has not
    /// produced muxed output yet. The command's acknowledgement is held until
    /// this clears, exactly like the former blocking barrier, but the daemon
    /// loop keeps running meanwhile.
    pub(crate) priming: Option<Instant>,
    /// Whether the segment must reach mux readiness once it opens: runtime
    /// starts do, rotations never did.
    pub(crate) prime_on_open: bool,
    /// Parent directory of the open segment, sampled for free space when the
    /// final report is frozen.
    pub(crate) parent: PathBuf,
    /// 1-based recorder slot number this segment belongs to; every report and
    /// failure notice this recorder emits is annotated with it.
    pub(crate) slot: usize,
    /// Configured `libx264` video bitrate for this segment; `None` keeps the
    /// encoder default. Reported in `FREEMIXD_RECORDER`.
    pub(crate) video_bitrate_kbps: Option<u32>,
    /// Configured AAC audio bitrate for this segment; `None` keeps the encoder
    /// default. Reported in `FREEMIXD_RECORDER`.
    pub(crate) audio_bitrate_kbps: Option<u32>,
    /// Estimated recording bitrate, for the free-space preflight.
    pub(crate) bitrate_bits_per_second: u64,
    /// Configured capacity of the open recorder's own pair queue, for the
    /// encoder statistics sample.
    pub(crate) queue_capacity: usize,
    /// WAV sidecar bitrate with `--record-wav`, which also gives the segment
    /// its WAV session.
    pub(crate) wav_bits_per_second: Option<u64>,
    /// The crash-safe session the open recorder writes into; reported in
    /// `FREEMIXD_RECORDER_SESSION` and watched for failovers.
    pub(crate) session: Option<RecorderSession>,
}

/// The fault-tolerant session behind one open segment.
#[cfg(feature = "native-media")]
pub(crate) struct RecorderSession {
    pub(crate) name: String,
    pub(crate) status: recording_session::SessionStatus,
    pub(crate) failover: recording_session::FailoverWatch,
    pub(crate) remaining: recording_session::RemainingTimeWatch,
}

/// A started recorder and the session it writes into.
#[cfg(feature = "native-media")]
pub(crate) struct OpenedRecorder {
    pub(crate) recorder: Recorder,
    pub(crate) session: RecorderSession,
}

/// Starts a planned session and a recorder writing into it.
///
/// Runs on the open worker for runtime starts and rotations, and inline for
/// configured startup recording. A refused recorder start removes the
/// session's manifests again when no segment was written, so the refused
/// name is reused by the next start exactly like the former refused file.
/// With `--record-wav` the WAV sidecar streams into its own session through
/// [`Recorder::start_with_sinks`], so it fails over with the video.
#[cfg(feature = "native-media")]
pub(crate) fn start_session_recorder(
    planned: recording_session::PlannedSession,
    config: RecordConfig,
) -> Result<OpenedRecorder, String> {
    let recording_session::OpenedSession {
        output,
        wav,
        status,
    } = recording_session::open(planned)?;
    let started = match wav {
        Some(wav) => Recorder::start_with_sinks(Box::new(output), Box::new(wav), config),
        None => Recorder::start_with_sink(Box::new(output), config),
    };
    match started {
        Ok(recorder) => Ok(OpenedRecorder {
            recorder,
            session: RecorderSession {
                name: status.name().to_owned(),
                status,
                failover: recording_session::FailoverWatch::default(),
                remaining: recording_session::RemainingTimeWatch::default(),
            },
        }),
        Err(error) => {
            recording_session::discard_unused(&status);
            Err(error.to_string())
        }
    }
}

/// Where an open segment's pairs go.
///
/// A runtime start spawns `FFmpeg` and waits for it to connect, which takes
/// from hundreds of milliseconds up to the recorder's connect timeout. That
/// happens on a dedicated worker, never on the daemon loop: the segment is
/// [`Self::Opening`] until the worker hands over the started [`Recorder`].
#[cfg(feature = "native-media")]
pub(crate) enum RecorderSink {
    /// The worker is starting the segment's session and `FFmpeg`.
    Opening {
        opened: mpsc::Receiver<Result<OpenedRecorder, String>>,
        /// Failure-notice prefix if the open fails (`start` or `rotate`).
        failure_label: &'static str,
        /// A cancellation requested before the recorder existed; applied as
        /// soon as it arrives.
        cancel_requested: bool,
    },
    /// The recorder is running.
    Open(Box<Recorder>),
    /// The open failed; the failure was already reported and no segment
    /// exists, so no `FREEMIXD_RECORDER` report is emitted.
    Unopened,
}

/// Per-segment options for a segment opened off the daemon loop.
#[cfg(feature = "native-media")]
#[derive(Clone, Copy)]
pub(crate) struct SegmentOpening {
    pub(crate) wav: bool,
    pub(crate) video_bitrate_kbps: Option<u32>,
    pub(crate) audio_bitrate_kbps: Option<u32>,
    /// 1-based recorder slot number.
    pub(crate) slot: usize,
    /// Failure-notice prefix if the open fails (`start` or `rotate`).
    pub(crate) failure_label: &'static str,
    /// Whether the acknowledgement-holding mux-readiness barrier applies.
    pub(crate) prime_on_open: bool,
}

/// Progress of an opening segment, observed without blocking.
#[cfg(feature = "native-media")]
pub(crate) enum OpenPoll {
    /// The segment is open (or was already).
    Open,
    /// The worker is still starting the recorder.
    Pending,
    /// The open failed; the slot must be released and this notice reported.
    Failed(String),
}

#[cfg(feature = "native-media")]
#[derive(Default)]
pub(crate) struct RecorderCapturePolicy {
    pub(crate) first_failure: Option<String>,
    /// What peers see of `first_failure` (Protocol 2.67).
    pub(crate) code: Option<OutputFailure>,
    pub(crate) app_capture_failure: bool,
}

#[cfg(feature = "native-media")]
impl RecorderCapturePolicy {
    pub(crate) fn active(&self) -> bool {
        self.first_failure.is_none()
    }

    pub(crate) fn fail(
        &mut self,
        failure: String,
        app_capture_failure: bool,
    ) -> Option<(&str, bool)> {
        if self.first_failure.is_some() {
            return None;
        }
        self.first_failure = Some(failure);
        self.app_capture_failure = app_capture_failure;
        let failure = self.first_failure.as_deref()?;
        Some((failure, app_capture_failure))
    }
}

/// Minimum positive frame count covering at least `seconds` at `rate`:
/// `ceil(seconds * rate.numerator() / rate.denominator())`.
#[cfg(feature = "native-media")]
pub(crate) fn segment_frame_limit(seconds: NonZeroU32, rate: fm_types::FrameRate) -> u64 {
    let frames = u128::from(seconds.get())
        .saturating_mul(u128::from(rate.numerator()))
        .div_ceil(u128::from(rate.denominator()))
        .max(1);
    u64::try_from(frames).unwrap_or(u64::MAX)
}

/// Non-drop-frame start timecode for the absolute engine frame index
/// `first_sequence` at `rate`, in the project's nominal frame-rate domain.
///
/// The nominal frames per second is the exact integer rounding of the
/// rational rate, `(numerator + denominator / 2) / denominator`, clamped to
/// the timecode's representable `1..=255`. Components wrap modulo their
/// conventional bases (24 hours, 60 minutes, 60 seconds). `None` is returned
/// only if [`fm_types::Timecode::new`] rejects a component, which the wrap and
/// the nominal clamp make unreachable but which stays typed rather than
/// panicking.
#[cfg(feature = "native-media")]
pub(crate) fn recording_start_timecode(
    first_sequence: u64,
    rate: fm_types::FrameRate,
) -> Option<fm_types::Timecode> {
    let numerator = u64::from(rate.numerator());
    let denominator = u64::from(rate.denominator());
    let nominal = u8::try_from(((numerator + denominator / 2) / denominator).clamp(1, 255)).ok()?;
    let frames_per_second = u64::from(nominal);
    let total_seconds = first_sequence / frames_per_second;
    fm_types::Timecode::new(
        u8::try_from((total_seconds / 3_600) % 24).ok()?,
        u8::try_from((total_seconds / 60) % 60).ok()?,
        u8::try_from(total_seconds % 60).ok()?,
        u8::try_from(first_sequence % frames_per_second).ok()?,
        nominal,
    )
    .ok()
}

#[cfg(feature = "native-media")]
impl NativeProgramRecorder {
    /// The recording format of a segment of `stored` opened at frame
    /// `first_frame`.
    pub(crate) fn stored_format(
        stored: &StoredProject,
        first_frame: u64,
    ) -> AppResult<RecordFormat> {
        let settings = stored.project().settings();
        let dimensions = settings.video.dimensions;
        Ok(RecordFormat::new(
            dimensions.width(),
            dimensions.height(),
            settings.frame_rate,
            settings.audio.sample_rate,
            settings.audio.channels.clone(),
            SequenceNumber::new(first_frame),
        )?)
    }

    /// Plans this segment's crash-safe session on the daemon loop. The name
    /// is reserved until the session's manifest exists, so the next segment
    /// name derived while this one is still opening never collides with it.
    pub(crate) fn plan_session(
        &self,
        path: &Path,
        names: &recording_session::SegmentNames,
    ) -> AppResult<recording_session::PlannedSession> {
        let parent = record_output_parent(path)?;
        recording_session::plan(
            path,
            &parent,
            names,
            self.slot,
            self.bitrate_bits_per_second,
            self.wav_bits_per_second,
        )
        .map_err(|error| AppFailure(error).into())
    }

    /// Opens a segment for an accepted runtime `RecordStart` without blocking
    /// the daemon loop: the segment file and `FFmpeg` start on a worker while
    /// captured pairs queue in the backlog, and mux readiness is awaited once
    /// the recorder arrives.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open(
        runtime: &NativeMediaRuntime,
        stored: &StoredProject,
        first_frame: u64,
        path: &Path,
        names: &recording_session::SegmentNames,
        wav: bool,
        video_bitrate_kbps: Option<u32>,
        audio_bitrate_kbps: Option<u32>,
        slot: usize,
    ) -> AppResult<Self> {
        let format = Self::stored_format(stored, first_frame)?;
        let timecode = recording_start_timecode(format.first_sequence().get(), format.frame_rate());
        Self::open_with_format(
            runtime,
            format,
            timecode,
            path,
            names,
            SegmentOpening {
                wav,
                video_bitrate_kbps,
                audio_bitrate_kbps,
                slot,
                failure_label: "start",
                prime_on_open: true,
            },
        )
    }

    /// Opens a segment on a worker; see [`Self::open`].
    pub(crate) fn open_with_format(
        runtime: &NativeMediaRuntime,
        format: RecordFormat,
        timecode: Option<fm_types::Timecode>,
        path: &Path,
        names: &recording_session::SegmentNames,
        opening: SegmentOpening,
    ) -> AppResult<Self> {
        let (mut recorder, config) = Self::prepare(
            runtime,
            format,
            timecode,
            path,
            opening.wav,
            opening.video_bitrate_kbps,
            opening.audio_bitrate_kbps,
            opening.slot,
        )?;
        // The session is planned and its name reserved here, so the next
        // segment name derived while this one is still opening never collides
        // with it; the free-space preflight, the session manifest, and the
        // FFmpeg start run on the worker.
        let planned = recorder.plan_session(path, names)?;
        let (sender, opened) = mpsc::sync_channel(1);
        let spawned = thread::Builder::new()
            .name(format!("freemix-native-recording-open-{}", opening.slot))
            .spawn(move || {
                let started = start_session_recorder(planned, config);
                // The segment always waits for this hand-over before it is
                // finalized, so the send only fails if the daemon is gone; the
                // dropped recorder then stops itself.
                let _ = sender.send(started);
            });
        spawned?;
        recorder.sink = RecorderSink::Opening {
            opened,
            failure_label: opening.failure_label,
            cancel_requested: false,
        };
        recorder.prime_on_open = opening.prime_on_open;
        Ok(recorder)
    }

    /// Everything a segment needs except its started recorder: the program
    /// readback, the recorder configuration, and the report metadata.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare(
        runtime: &NativeMediaRuntime,
        format: RecordFormat,
        timecode: Option<fm_types::Timecode>,
        path: &Path,
        wav: bool,
        video_bitrate_kbps: Option<u32>,
        audio_bitrate_kbps: Option<u32>,
        slot: usize,
    ) -> AppResult<(Self, RecordConfig)> {
        let dimensions = format.dimensions();
        let readback = runtime.create_program_readback_blocking(
            NonZeroU32::new(dimensions.width()).unwrap_or(NonZeroU32::MIN),
            NonZeroU32::new(dimensions.height()).unwrap_or(NonZeroU32::MIN),
        )?;
        let parent = recording_parent(path).to_path_buf();
        let mut config = RecordConfig::new(format.clone());
        config.limits.stop_timeout = PROGRAM_RECORDER_STOP_TIMEOUT;
        config.limits.kill_timeout = PROGRAM_RECORDER_KILL_TIMEOUT;
        // The WAV sidecar goes to its own session sink, never to a path.
        let wav_bits_per_second = wav.then(|| {
            recording_session::wav_bitrate(
                format.sample_rate().hertz(),
                format.channel_layout().channels().len(),
            )
        });
        config.video_bitrate_kbps = video_bitrate_kbps;
        config.audio_bitrate_kbps = audio_bitrate_kbps;
        config.timecode = timecode;
        let startup_pair_timeout = config
            .limits
            .connect_timeout
            .checked_add(config.limits.no_progress_timeout)
            .ok_or_else(|| AppFailure("Program recorder startup timeout overflow".into()))?;
        let backlog_limit = config.limits.max_retained_bytes;
        let bitrate_bits_per_second = recording_session::estimated_bitrate(
            dimensions.width(),
            dimensions.height(),
            format.frame_rate(),
            format.channel_layout().channels().len(),
            video_bitrate_kbps,
            audio_bitrate_kbps,
        );
        Ok((
            Self {
                readback,
                format,
                sink: RecorderSink::Unopened,
                backlog: std::collections::VecDeque::new(),
                backlog_bytes: 0,
                backlog_limit,
                backlogged_pairs: 0,
                backlog_peak_bytes: 0,
                capture: RecorderCapturePolicy::default(),
                finalization_clean: None,
                startup_pair_timeout,
                priming: None,
                prime_on_open: false,
                parent,
                slot,
                video_bitrate_kbps,
                audio_bitrate_kbps,
                bitrate_bits_per_second,
                queue_capacity: config.limits.max_outstanding_pairs,
                wav_bits_per_second,
                session: None,
            },
            config,
        ))
    }

    /// The running recorder, once the segment is open.
    pub(crate) fn recorder(&self) -> Option<&Recorder> {
        match &self.sink {
            RecorderSink::Open(recorder) => Some(recorder.as_ref()),
            RecorderSink::Opening { .. } | RecorderSink::Unopened => None,
        }
    }

    /// Whether this segment is still being realized: opening, or open but
    /// not yet confirmed mux-ready. A `RecordStart` acknowledgement waits for
    /// this to clear.
    pub(crate) fn is_settling(&self) -> bool {
        matches!(self.sink, RecorderSink::Opening { .. }) || self.priming.is_some()
    }

    /// Observes an opening segment without blocking.
    pub(crate) fn poll_open(&mut self) -> OpenPoll {
        let RecorderSink::Opening { opened, .. } = &self.sink else {
            return OpenPoll::Open;
        };
        let received = match opened.try_recv() {
            Ok(received) => received,
            Err(mpsc::TryRecvError::Empty) => return OpenPoll::Pending,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("recorder open worker exited without a result".to_owned())
            }
        };
        self.finish_open(received)
    }

    /// Waits for an opening segment. Only finalization paths call this: a
    /// detached finalizer, or shutdown.
    pub(crate) fn wait_open(&mut self) -> OpenPoll {
        let RecorderSink::Opening { opened, .. } = &self.sink else {
            return OpenPoll::Open;
        };
        let received = opened
            .recv()
            .unwrap_or_else(|_| Err("recorder open worker exited without a result".to_owned()));
        self.finish_open(received)
    }

    pub(crate) fn finish_open(&mut self, received: Result<OpenedRecorder, String>) -> OpenPoll {
        let RecorderSink::Opening {
            failure_label,
            cancel_requested,
            ..
        } = &self.sink
        else {
            return OpenPoll::Open;
        };
        let (failure_label, cancel_requested) = (*failure_label, *cancel_requested);
        match received {
            Ok(OpenedRecorder {
                mut recorder,
                session,
            }) => {
                self.session = Some(session);
                if cancel_requested {
                    recorder.request_cancel();
                } else if self.prime_on_open {
                    // The barrier budget starts when the recorder exists,
                    // exactly as it did when the start blocked.
                    self.priming = Some(
                        Instant::now()
                            .checked_add(self.startup_pair_timeout)
                            .unwrap_or_else(Instant::now),
                    );
                }
                self.sink = RecorderSink::Open(Box::new(recorder));
                self.pump_backlog();
                OpenPoll::Open
            }
            Err(error) => {
                self.sink = RecorderSink::Unopened;
                self.clear_backlog();
                OpenPoll::Failed(format!("{failure_label}:{error}"))
            }
        }
    }

    /// Reports a directory the session just failed over from or returned
    /// to, and remaining-recording-time threshold crossings. The session
    /// keeps recording on the next usable directory, so these are notices,
    /// not recorder failures.
    pub(crate) fn report_failover(&mut self) {
        if let Some(session) = &mut self.session
            && let Some(record) =
                session
                    .failover
                    .observe(self.slot, &session.name, &session.status.get())
        {
            eprintln!("{record}");
        }
        if let Some(session) = &mut self.session
            && let Some(record) =
                session
                    .remaining
                    .observe(self.slot, &session.status, Instant::now())
        {
            eprintln!("{record}");
        }
    }

    pub(crate) fn clear_backlog(&mut self) {
        self.backlog.clear();
        self.backlog_bytes = 0;
    }

    /// Hands backlog pairs to the open recorder, oldest first, until it has
    /// no room for more. A full queue is not a failure here: the backlog keeps
    /// the pair and retries on the next pass.
    pub(crate) fn pump_backlog(&mut self) {
        let failure = {
            let RecorderSink::Open(recorder) = &mut self.sink else {
                return;
            };
            let mut failure = None;
            while let Some(pair) = self.backlog.pop_front() {
                let bytes = pair.retained_bytes();
                match recorder.enqueue(pair) {
                    Ok(()) => self.backlog_bytes = self.backlog_bytes.saturating_sub(bytes),
                    Err(error)
                        if matches!(
                            error.reason,
                            EnqueueRejection::QueueFull | EnqueueRejection::RetainedByteLimit
                        ) =>
                    {
                        self.backlog.push_front(error.into_frame());
                        break;
                    }
                    Err(error) => {
                        let app_capture_failure =
                            !matches!(&error.reason, EnqueueRejection::Failed(_));
                        failure =
                            Some((format!("enqueue:{:?}", error.reason), app_capture_failure));
                        break;
                    }
                }
            }
            failure
        };
        if let Some((failure, app_capture_failure)) = failure {
            self.fail(failure, app_capture_failure);
        }
    }

    /// Outstanding pairs and retained bytes across the recorder and the
    /// backlog, for whole-run telemetry.
    pub(crate) fn pipeline_load(&self) -> (u64, u64) {
        let (pairs, bytes) = self.recorder().map_or((0, 0), |recorder| {
            let telemetry = recorder.telemetry();
            (telemetry.outstanding_pairs, telemetry.retained_bytes)
        });
        (
            saturating_u64(pairs).saturating_add(saturating_u64(self.backlog.len())),
            saturating_u64(bytes).saturating_add(saturating_u64(self.backlog_bytes)),
        )
    }

    /// The open recorder's own pair queue against its configured capacity,
    /// for the encoder statistics sample. The pre-recorder backlog is not
    /// part of it; `None` while the segment has not opened.
    pub(crate) fn queue_load(&self) -> Option<(u64, u64)> {
        let telemetry = self.recorder()?.telemetry();
        Some((
            saturating_u64(telemetry.outstanding_pairs),
            saturating_u64(self.queue_capacity),
        ))
    }

    pub(crate) fn should_rotate(&self, segment_seconds: Option<NonZeroU32>) -> bool {
        self.capture.active()
            && self.backlog.is_empty()
            && segment_seconds.is_some_and(|seconds| {
                self.recorder().is_some_and(|recorder| {
                    recorder.telemetry().accepted_pairs
                        >= segment_frame_limit(seconds, self.format.frame_rate())
                })
            })
    }

    pub(crate) fn capture(
        &mut self,
        runtime: &NativeMediaRuntime,
        program: &NativeTexture,
        readbacks: &mut FrameReadbacks<'_>,
        audio: AudioBlock,
    ) {
        if !self.capture.active() {
            return;
        }
        if let Some(telemetry) = self.recorder().map(Recorder::telemetry)
            && telemetry.state == RecorderState::Failed
        {
            self.fail(format!("backend:{:?}", telemetry.failure), false);
            return;
        }
        let sequence = audio.timing().sequence();
        // This is the existing synchronous diagnostic readback path, not a
        // nonblocking or zero-copy encoder bridge; a stream group that read
        // the same picture this frame already paid for it.
        let readback = match readbacks.take(runtime, &mut self.readback, program) {
            Ok(readback) => readback,
            Err(error) => {
                self.fail(format!("readback:{error}"), true);
                return;
            }
        };
        let Some(expected_stride) = readback.width.checked_mul(4) else {
            self.fail("readback:stride_overflow".to_owned(), true);
            return;
        };
        if readback.width != self.readback.width()
            || readback.height != self.readback.height()
            || readback.stride != expected_stride
        {
            self.fail("readback:invalid_tight_layout".to_owned(), true);
            return;
        }
        let pair = match PairedFrame::new(&self.format, sequence, readback.rgba, audio) {
            Ok(pair) => pair,
            Err(error) => {
                self.fail(format!("frame:{error}"), true);
                return;
            }
        };
        let pair = if self.backlog.is_empty()
            && let RecorderSink::Open(recorder) = &mut self.sink
        {
            match recorder.enqueue(pair) {
                Ok(()) => return,
                // A full queue is backpressure, not a failure: `FFmpeg` is
                // still starting its encoder after the segment opened, or was
                // briefly descheduled or slowed by its disk. The pair waits in
                // the bounded backlog and drains once the recorder catches
                // up; only a stall long enough to exhaust the backlog (or the
                // recorder's own no-progress timeout) fails the segment.
                Err(error)
                    if matches!(
                        error.reason,
                        EnqueueRejection::QueueFull | EnqueueRejection::RetainedByteLimit
                    ) =>
                {
                    error.into_frame()
                }
                Err(error) => {
                    let app_capture_failure = !matches!(&error.reason, EnqueueRejection::Failed(_));
                    self.fail(format!("enqueue:{:?}", error.reason), app_capture_failure);
                    return;
                }
            }
        } else {
            pair
        };
        // The segment is still opening, older pairs are still waiting, or
        // the recorder's queue is full: queue behind them so the segment
        // stays in sequence order.
        let bytes = pair.retained_bytes();
        if self.backlog_bytes.saturating_add(bytes) > self.backlog_limit {
            let failure = if matches!(self.sink, RecorderSink::Open(_)) {
                "enqueue:backlog_full"
            } else {
                "enqueue:open_backlog_full"
            };
            self.fail(failure.to_owned(), true);
            return;
        }
        self.backlog_bytes += bytes;
        self.backlog_peak_bytes = self.backlog_peak_bytes.max(self.backlog_bytes);
        self.backlogged_pairs = self.backlogged_pairs.saturating_add(1);
        self.backlog.push_back(pair);
        self.pump_backlog();
    }

    pub(crate) fn fail(&mut self, failure: String, app_capture_failure: bool) {
        if self.capture.first_failure.is_some() {
            return;
        }
        // `FFmpeg`'s redacted stderr tail names the storage cause (a full
        // disk, a refused folder) the failure variant alone does not.
        let stderr = self
            .recorder()
            .map(|recorder| recorder.telemetry().stderr_tail)
            .unwrap_or_default();
        if let Some((failure, app_capture_failure)) =
            self.capture.fail(failure, app_capture_failure)
        {
            let notice = recorder_failure_notice(self.slot, failure, app_capture_failure);
            let code = output_failure::recorder_failure(failure, &stderr);
            eprintln!("{notice}");
            self.capture.code = Some(code);
            self.cancel_sink();
        }
    }

    /// Cancels whatever sink exists now, or the one still opening as soon as
    /// it arrives, and discards the backlog.
    pub(crate) fn cancel_sink(&mut self) {
        self.clear_backlog();
        match &mut self.sink {
            RecorderSink::Open(recorder) => recorder.request_cancel(),
            RecorderSink::Opening {
                cancel_requested, ..
            } => *cancel_requested = true,
            RecorderSink::Unopened => {}
        }
    }

    pub(crate) fn startup_decision(&self) -> AppResult<StartupPairDecision> {
        let decision = match self.recorder() {
            Some(recorder) => {
                let telemetry = recorder.telemetry();
                startup_pair_decision(
                    telemetry.state,
                    telemetry.completed_pairs,
                    telemetry.output_bytes,
                    telemetry.failure.is_some() || self.capture.first_failure.is_some(),
                )
            }
            None if self.capture.first_failure.is_some() => StartupPairDecision::Failed,
            None => StartupPairDecision::Pending,
        };
        if decision != StartupPairDecision::Failed {
            return Ok(decision);
        }
        let failure = self
            .capture
            .first_failure
            .clone()
            .or_else(|| {
                self.recorder()
                    .and_then(|recorder| recorder.telemetry().failure)
                    .map(|failure| format!("{failure:?}"))
            })
            .unwrap_or_else(|| "unknown".to_owned());
        Err(AppFailure(format!(
            "Program recorder failed before readiness: {}",
            diagnostic_field(&failure)
        ))
        .into())
    }

    pub(crate) fn fail_startup_timeout(&mut self) -> Box<dyn Error> {
        self.fail("startup:mux_readiness_timeout".to_owned(), false);
        AppFailure("Program recorder mux output timed out before readiness".into()).into()
    }

    /// Cancels the sink without waiting: future enqueues are refused and the
    /// child is asked to terminate within the kill budget.
    pub(crate) fn request_cancel(&mut self) {
        if self.finalization_clean.is_none() {
            self.cancel_sink();
        }
    }

    /// Feeds every backlog pair to the recorder before it is stopped, so a
    /// segment stopped right after it opened still holds every frame it
    /// accepted. Bounded by the recorder's own stop budget.
    pub(crate) fn drain_backlog_for_stop(&mut self) {
        let deadline = Instant::now().checked_add(PROGRAM_RECORDER_STOP_TIMEOUT);
        loop {
            self.pump_backlog();
            if self.backlog.is_empty() || self.recorder().is_none() {
                return;
            }
            if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                self.fail("enqueue:QueueFull".to_owned(), true);
                return;
            }
            thread::sleep(NATIVE_IO_POLL_INTERVAL);
        }
    }

    pub(crate) fn stop_and_report(&mut self) -> AppResult<()> {
        if let Some(clean) = self.finalization_clean {
            return if clean {
                Ok(())
            } else {
                Err(AppFailure("Program recording did not finalize cleanly".into()).into())
            };
        }
        if let OpenPoll::Failed(failure) = self.wait_open() {
            eprintln!("{}", recorder_failure_notice(self.slot, &failure, false));
        }
        self.drain_backlog_for_stop();
        let RecorderSink::Open(recorder) = &mut self.sink else {
            // The segment never opened: its failure was already reported and
            // there is no file to report on.
            self.finalization_clean = Some(false);
            return Err(AppFailure("Program recording did not finalize cleanly".into()).into());
        };
        let report = recorder.stop();
        // A fragment muxed while the recorder drained may have failed over.
        self.report_failover();
        let duration_ms =
            recorded_duration_ms(report.telemetry.completed_pairs, self.format.frame_rate());
        let free_bytes = disk::available_bytes(&self.parent).unwrap_or(0);
        let app_capture_failure = self.capture.app_capture_failure;
        let failure = self
            .capture
            .first_failure
            .clone()
            .or_else(|| {
                report
                    .telemetry
                    .failure
                    .as_ref()
                    .map(|value| format!("{value:?}"))
            })
            .unwrap_or_else(|| "none".to_owned());
        eprintln!(
            "{}",
            recorder_report(
                self.slot,
                &report,
                duration_ms,
                free_bytes,
                RecorderBacklog {
                    pairs: self.backlogged_pairs,
                    peak_bytes: self.backlog_peak_bytes,
                },
                app_capture_failure,
                &failure,
                self.video_bitrate_kbps,
                self.audio_bitrate_kbps,
            )
        );
        if let Some(session) = &self.session {
            eprintln!(
                "{}",
                recording_session::session_report(self.slot, &session.name, &session.status.get())
            );
        }
        let complete = report.telemetry.state == RecorderState::Stopped
            && report.outcome == StopOutcome::Clean
            && report.output == OutputFinalization::Synced
            && report.cleanup == CleanupStatus::Complete
            && report.telemetry.accepted_pairs == report.telemetry.completed_pairs
            && report.telemetry.failed_pairs == 0
            && !app_capture_failure;
        self.finalization_clean = Some(complete);
        if complete {
            Ok(())
        } else {
            Err(AppFailure("Program recording did not finalize cleanly".into()).into())
        }
    }
}

#[cfg(feature = "native-media")]
impl Drop for NativeProgramRecorder {
    fn drop(&mut self) {
        let _ = self.stop_and_report();
    }
}

#[cfg(feature = "native-media")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StartupPairDecision {
    Pending,
    Ready,
    Failed,
}

#[cfg(feature = "native-media")]
pub(crate) fn startup_pair_decision(
    state: RecorderState,
    completed_pairs: u64,
    output_bytes: u64,
    failed: bool,
) -> StartupPairDecision {
    if failed || state == RecorderState::Failed {
        StartupPairDecision::Failed
    } else if completed_pairs > 0 && output_bytes > 0 {
        StartupPairDecision::Ready
    } else {
        StartupPairDecision::Pending
    }
}

/// Wall duration of `pairs` completed frames at `rate`, in whole milliseconds.
///
/// The recorder knows only admitted and completed pair counts; the captured
/// cadence turns those into the media span the segment actually covers. The
/// arithmetic is exact integer math, saturating rather than wrapping.
#[cfg(feature = "native-media")]
pub(crate) fn recorded_duration_ms(pairs: u64, rate: fm_types::FrameRate) -> u64 {
    let numerator = u128::from(rate.numerator());
    let denominator = u128::from(rate.denominator());
    let millis = u128::from(pairs)
        .saturating_mul(1_000)
        .saturating_mul(denominator)
        / numerator;
    u64::try_from(millis).unwrap_or(u64::MAX)
}

/// Realization state of one configured recorder slot.
///
/// Recording is configured by `--record-program` (slot 0) or
/// `--record-program-2` (slot 1) but only OPENED while the engine's desired
/// flag is set, mirroring how stream sinks realize desired running flags. Each
/// slot is independent: one open segment per configured output, boxed to keep
/// this enum small and moved as a unit at every state change.
#[cfg(feature = "native-media")]
pub(crate) enum ProgramRecording {
    /// Recording is configured, but no segment is open.
    Off,
    /// One open segment capturing the program feed.
    Running(Box<NativeProgramRecorder>),
}

#[cfg(feature = "native-media")]
impl ProgramRecording {
    /// Whether this slot currently owns an open segment.
    pub(crate) fn is_running(&self) -> bool {
        matches!(self, Self::Running(_))
    }

    /// Whether this slot's segment is still opening or awaiting mux
    /// readiness.
    pub(crate) fn is_settling(&self) -> bool {
        matches!(self, Self::Running(recorder) if recorder.is_settling())
    }
}

/// The daemon's fixed number of recorder slots: slot 0 is `--record-program`
/// and slot 1 is `--record-program-2`. A second slot doubles the independent
/// destinations behind the engine's per-slot desired recording flags. The
/// count also sizes the per-slot CLI configuration arrays, so it stays
/// available in every build.
/// Selects the configured recorder slots whose restored desired flag asks for
/// an open segment at startup.
///
/// A slot is selected only when it is both configured and desired: an
/// unconfigured slot has no destination to open, and a configured slot whose
/// persisted flag is false stays closed even when the other slot is
/// recording. Runtime `RecordStart` commands open a slot later regardless of
/// this startup decision.
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
pub(crate) fn startup_recording_slots(
    configured: [bool; RECORDER_SLOTS],
    desired: [bool; RECORDER_SLOTS],
) -> [bool; RECORDER_SLOTS] {
    std::array::from_fn(|slot| configured[slot] && desired[slot])
}

/// Resolves an accepted recording command to its addressed slot.
///
/// Returns `(active, slot_index)` for a `RecordStart`/`RecordStop` and `None`
/// for every other command. `WireRecorderSlot` only constructs in `1..=2`, so
/// the index always names exactly one of the daemon's two recorder slots and
/// realization never touches the other one.
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
pub(crate) fn recording_command_slot(payload: &CommandPayload) -> Option<(bool, usize)> {
    match payload {
        CommandPayload::RecordStart { recorder } => Some((true, usize::from(recorder.get()) - 1)),
        CommandPayload::RecordStop { recorder } => Some((false, usize::from(recorder.get()) - 1)),
        _ => None,
    }
}

/// The recorder and stream commands an accepted cue GO or advance (RC-017)
/// or script run (RC-011) ran, in the order its actions ran them, as the
/// direct commands the native runtime realizes: a cue that starts REC 1
/// opens a segment exactly like `record_start`, and one that stops a stream
/// retires its worker exactly like `stream_stop`. Every other command yields
/// nothing; the direct record and stream commands are observed as
/// themselves.
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
pub(crate) fn cue_worker_commands(
    command: &CommandMessage,
    submission: &CommandSubmission,
) -> Vec<CommandMessage> {
    if !matches!(
        command.payload,
        CommandPayload::CueControl { .. }
            | CommandPayload::CueAdvance { .. }
            | CommandPayload::RunScript { .. }
            | CommandPayload::FireShortcut { .. }
    ) {
        return Vec::new();
    }
    let Some(accepted) = &submission.accepted else {
        return Vec::new();
    };
    accepted
        .events
        .iter()
        .filter_map(|event| match &event.payload {
            fm_engine::EngineEvent::CueListChanged { applied, .. }
            | fm_engine::EngineEvent::ScriptRan { applied, .. }
            | fm_engine::EngineEvent::ShortcutFired { applied, .. } => Some(applied),
            _ => None,
        })
        .flatten()
        .filter_map(|action| {
            let wire_recorder = |recorder: RecorderSlot| {
                fm_protocol::WireRecorderSlot::new(recorder.get())
                    .unwrap_or(fm_protocol::WireRecorderSlot::FIRST)
            };
            let payload = match action {
                fm_engine::EngineCommand::RecordStart { recorder } => CommandPayload::RecordStart {
                    recorder: wire_recorder(*recorder),
                },
                fm_engine::EngineCommand::RecordStop { recorder } => CommandPayload::RecordStop {
                    recorder: wire_recorder(*recorder),
                },
                fm_engine::EngineCommand::StreamStart { target } => CommandPayload::StreamStart {
                    target: WireStreamTargetId::new(target.get()),
                },
                fm_engine::EngineCommand::StreamStop { target } => CommandPayload::StreamStop {
                    target: WireStreamTargetId::new(target.get()),
                },
                _ => return None,
            };
            Some(CommandMessage {
                payload,
                ..command.clone()
            })
        })
        .collect()
}

/// One segment handed off the render path for bounded finalization.
///
/// The worker owns the recorder and emits exactly one `FREEMIXD_RECORDER`
/// stderr report; shutdown waits for completion within the recorder's own
/// stop budget and detaches rather than blocking forever. The completion
/// signal is a plain flag because the detailed report already reached stderr
/// and [`AppResult`] is not `Send`.
#[cfg(feature = "native-media")]
pub(crate) struct ProgramRecordingFinalizer {
    pub(crate) done: mpsc::Receiver<bool>,
    pub(crate) worker: thread::JoinHandle<()>,
}

/// Upper bound for waiting on a detached finalizer at shutdown. The worker's
/// own `stop()` drains within [`PROGRAM_RECORDER_STOP_TIMEOUT`] and kills
/// within [`PROGRAM_RECORDER_KILL_TIMEOUT`], so this adds only scheduler
/// slack; past it the worker is detached like stinger retirement cleanup.
#[cfg(feature = "native-media")]
pub(crate) const PROGRAM_RECORDER_FINALIZE_JOIN_TIMEOUT: Duration = Duration::from_secs(35);

/// First-segment ordinal for derived recording paths; later segments count up
/// from 002 so the configured path itself stays segment 001.
#[cfg(feature = "native-media")]
pub(crate) const RECORD_SEGMENT_LAST_ORDINAL: u32 = 999;

#[cfg(feature = "native-media")]
pub(crate) fn next_record_segment_path(
    base: &Path,
    mut exists: impl FnMut(&Path) -> bool,
) -> AppResult<PathBuf> {
    if base.extension().and_then(|extension| extension.to_str()) != Some("mp4") {
        return Err(
            AppFailure("recording output must have the final extension `.mp4`".into()).into(),
        );
    }
    let file_name = base
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AppFailure("recording output must name a final .mp4 file".into()))?;
    if !exists(base) {
        return Ok(base.to_path_buf());
    }
    // `to_string_lossy` keeps non-UTF8 stems refused instead of silently
    // re-encoded: a path that cannot be named deterministically is never
    // created.
    let stem = base
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| AppFailure("recording output stem is not valid Unicode".into()))?;
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    for ordinal in 2..=RECORD_SEGMENT_LAST_ORDINAL {
        let candidate = parent.join(format!("{stem}-{ordinal:03}.mp4"));
        if !exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(AppFailure(format!(
        "recording segments are exhausted past {RECORD_SEGMENT_LAST_ORDINAL} for {}",
        diagnostic_field(&file_name.to_string_lossy())
    ))
    .into())
}

/// Resolves the next segment path for one recorder slot.
///
/// Slot 0 derives from `--record-program` and slot 1 from
/// `--record-program-2`; an unconfigured slot derives nothing. Derivation
/// stays independent per slot, so two recorders writing into the same
/// directory fill their own `<stem>-NNN.mp4` gaps.
///
/// # Errors
///
/// Returns the same refusal [`next_record_segment_path`] produces for a
/// configured path that cannot be segmented.
#[cfg(feature = "native-media")]
pub(crate) fn next_slot_segment_path(
    slot: usize,
    primary: Option<&Path>,
    secondary: Option<&Path>,
    exists: impl FnMut(&Path) -> bool,
) -> AppResult<Option<PathBuf>> {
    let configured = match slot {
        0 => primary,
        1 => secondary,
        _ => None,
    };
    configured
        .map(|base| next_record_segment_path(base, exists))
        .transpose()
}

/// Selects the configured rotation duration addressed to one recorder slot.
///
/// Slot 0 follows `--record-segment-seconds` and slot 1 follows
/// `--record-segment-seconds-2`; `None` means that slot never rotates
/// automatically, even when the other slot is configured to rotate.
#[cfg(feature = "native-media")]
pub(crate) fn slot_segment_seconds(
    segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
    slot: usize,
) -> Option<NonZeroU32> {
    segment_seconds[slot]
}
/// Validates that every configured recorder slot's output selection names an
/// output declared by `project`.
///
/// # Errors
///
/// Returns an [`AppFailure`] naming the CLI flag and the unknown
/// [`fm_types::OutputId`] when a slot selects an output absent from `project`.
pub(crate) fn validate_recording_output_selections(
    project: &fm_model::Project,
    record_output: Option<fm_types::OutputId>,
    record_output_2: Option<fm_types::OutputId>,
) -> AppResult<()> {
    for (flag, selected) in [
        ("--record-output", record_output),
        ("--record-output-2", record_output_2),
    ] {
        let Some(output_id) = selected else {
            continue;
        };
        if !project
            .outputs()
            .iter()
            .any(|output| output.id == output_id)
        {
            return Err(
                AppFailure(format!("{flag} selects unknown project output {output_id}")).into(),
            );
        }
    }
    Ok(())
}

/// Parent directory of a configured recording output.
///
/// A bare file name resolves to the working directory the recorder writes to,
/// mirroring how [`next_record_segment_path`] keeps derived segment paths
/// relative.
pub(crate) fn recording_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Preflights every configured recording output's parent directory, refusing
/// startup when one has too little free space.
///
/// Every configured recorder slot is a configured show destination, so each
/// parent is held to the same floor; the whole-run sampler later takes the
/// minimum across these parents and every local HLS output.
///
/// # Errors
///
/// Returns a sanitized [`AppFailure`] naming the flag when any configured
/// parent cannot be queried or falls below
/// [`disk::RECORDING_MINIMUM_FREE_BYTES`]. The message carries free-space
/// counts or an I/O kind only; it never renders the operator's path.
pub(crate) fn preflight_recording_outputs<'a>(
    outputs: impl IntoIterator<Item = (&'static str, Option<&'a Path>)>,
) -> AppResult<()> {
    for (flag, path) in outputs {
        let Some(path) = path else {
            continue;
        };
        disk::preflight_recording_parent(recording_parent(path)).map_err(|error| {
            AppFailure(match error {
                disk::DiskPreflightError::BelowMinimum { available } => format!(
                    "{flag} requires at least {} free bytes, found {available}",
                    disk::RECORDING_MINIMUM_FREE_BYTES
                ),
                disk::DiskPreflightError::Unavailable(kind) => {
                    format!("{flag} free space is unavailable: {kind}")
                }
            })
        })?;
    }
    Ok(())
}

/// Checks every configured recording output and the optional fallback
/// directory against the crash-safe session contract before any recorder
/// exists: each output stem must yield valid session names, and the fallback
/// must not be an output's own directory.
///
/// # Errors
///
/// Returns an [`AppFailure`] naming the flag; it never renders a path.
#[cfg_attr(
    not(feature = "native-media"),
    allow(clippy::unnecessary_wraps, clippy::needless_pass_by_value)
)]
pub(crate) fn validate_recording_sessions<'a>(
    outputs: impl IntoIterator<Item = (&'static str, Option<&'a Path>)>,
    fallback: Option<&Path>,
) -> AppResult<()> {
    #[cfg(feature = "native-media")]
    for (flag, path) in outputs {
        let Some(path) = path else {
            continue;
        };
        recording_session::validate_output_stem(path)
            .map_err(|reason| AppFailure(format!("{flag} {reason}")))?;
        if let Some(fallback) = fallback
            && let (Ok(parent), Ok(fallback)) = (
                fs::canonicalize(recording_parent(path)),
                fs::canonicalize(fallback),
            )
            && parent == fallback
        {
            return Err(AppFailure(format!(
                "--record-fallback-dir must differ from the {flag} output directory"
            ))
            .into());
        }
    }
    // Recording requires native media; without it nothing is configured.
    #[cfg(not(feature = "native-media"))]
    let _ = (outputs, fallback);
    Ok(())
}
